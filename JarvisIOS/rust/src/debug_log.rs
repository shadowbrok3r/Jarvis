//! In-app debug log buffer (read by Swift via `jarvis_ios_debug_log_snapshot`)
//! and persistent crash-log file written synchronously on every line.
//!
//! Boot sequence (called from Swift before `jarvis_renderer_new`):
//!   1. `jarvis_ios_set_log_file(path, prev_path)` — sets active + previous file paths
//!      and installs the `tracing` subscriber that feeds ALL Bevy / bevy_vrm1 log output.
//!   2. Bevy starts — `LogPlugin` is disabled in `ios_bevy.rs`.
//!   3. Every `info!`, `warn!`, `error!` from Bevy/crates goes through the subscriber
//!      → `jarvis_ios_debug_push` → ring buffer + file.
//!   4. `jarvis_ios_line!` macro does the same for explicit iOS-specific messages.
//!
//! The log file uses `O_SYNC`-equivalent: `BufWriter` is flushed after each write so
//! the OS commits the bytes even when the process is killed.

use std::collections::VecDeque;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LINES: usize = 1200;

// ---------------------------------------------------------------------------
// Verbosity controls (atomic so the macro can check without a Bevy resource lookup)
// ---------------------------------------------------------------------------
//
// Levels:
//   0 = OFF     — suppress every `jarvis_ios_line!` call (tracing events still go through
//                 the EnvFilter; this gates only our explicit per-frame diagnostic lines).
//   1 = QUIET   — drop the per-frame `frame=N`, `PostUpdate end`, `app.update()` lines and
//                 the slow-frame detector. Critical lifecycle messages (init/load/error)
//                 still go through.
//   2 = NORMAL  — current default: every 30 frames + first 30 + visibility changes.
//   3 = DEBUG   — log every single frame for deeper diagnostics.
//
// Critical lines that should bypass QUIET: anything whose first call argument starts with
// `[JarvisIOS] crit:` (we use that prefix for unrecoverable / load-time messages).
//
// The `jarvis_ios_line!` macro now takes an optional channel before the format string:
//   jarvis_ios_line!("plain frame log {x}");                    // gated: hidden at QUIET
//   jarvis_ios_line!(crit: "[JarvisIOS] crit: VRM failed: {e}"); // always logs (unless OFF)

pub const LOG_VERBOSITY_OFF: u8 = 0;
pub const LOG_VERBOSITY_QUIET: u8 = 1;
pub const LOG_VERBOSITY_NORMAL: u8 = 2;
pub const LOG_VERBOSITY_DEBUG: u8 = 3;

static LOG_VERBOSITY: AtomicU8 = AtomicU8::new(LOG_VERBOSITY_NORMAL);

#[inline]
pub fn log_verbosity() -> u8 {
    LOG_VERBOSITY.load(Ordering::Relaxed)
}

pub fn set_log_verbosity(level: u8) {
    let clamped = level.min(LOG_VERBOSITY_DEBUG);
    LOG_VERBOSITY.store(clamped, Ordering::Relaxed);
}

/// Should an ordinary diagnostic line (frame counters, asset stats, slow-frame detector,
/// `PostUpdate end`, `app.update()` enter/leave) be emitted right now? Returns false at
/// `QUIET` and `OFF` so the call site can short-circuit before formatting.
#[inline]
pub fn diag_logging_enabled() -> bool {
    log_verbosity() >= LOG_VERBOSITY_NORMAL
}

/// Should a per-frame ("every single tick") log line be emitted? Only true at `DEBUG`.
#[allow(dead_code)]
#[inline]
pub fn debug_logging_enabled() -> bool {
    log_verbosity() >= LOG_VERBOSITY_DEBUG
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn jarvis_ios_set_log_verbosity(level: u8) {
    set_log_verbosity(level);
}

// ---------------------------------------------------------------------------
// Ring buffer (in-app display)
// ---------------------------------------------------------------------------

fn buffer() -> &'static Mutex<VecDeque<String>> {
    static BUF: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LINES)))
}

fn ts_prefix() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}")
}

// ---------------------------------------------------------------------------
// Persistent log file (survives crash) — async writer thread
// ---------------------------------------------------------------------------
//
// Earlier versions called `BufWriter::flush()` on every line from the calling thread.
// On iOS that triggers a Mach kernel write that can stall 5–50ms whenever the OS
// commits dirty pages to disk, which manifests as visible frame hitches whenever the
// user pans the camera or drags an egui window (each input emits tracing events).
//
// We now hand log lines to a dedicated background thread via an unbounded mpsc channel.
// The hot path on the render thread is just a non-blocking channel send (no syscalls,
// no fsync). The background thread batches writes and flushes every 250ms, with a
// best-effort flush on app shutdown via `flush_log_file()`.

use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

static LOG_FILE: OnceLock<Mutex<Option<BufWriter<std::fs::File>>>> = OnceLock::new();
static LOG_TX: OnceLock<Mutex<Option<Sender<LogMessage>>>> = OnceLock::new();

enum LogMessage {
    Line(String),
    Flush,
}

fn log_file() -> &'static Mutex<Option<BufWriter<std::fs::File>>> {
    LOG_FILE.get_or_init(|| Mutex::new(None))
}

fn open_log_file(path: &str) {
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => {
            if let Ok(mut g) = log_file().lock() {
                *g = Some(BufWriter::with_capacity(64 * 1024, f));
            }
            ensure_writer_thread();
        }
        Err(e) => {
            eprintln!("[debug_log] failed to open log file {path}: {e}");
        }
    }
}

fn ensure_writer_thread() {
    let tx_slot = LOG_TX.get_or_init(|| Mutex::new(None));
    let mut g = match tx_slot.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if g.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<LogMessage>();
    *g = Some(tx);
    drop(g);

    thread::Builder::new()
        .name("jarvis_ios_log_writer".into())
        .spawn(move || {
            let mut last_flush = std::time::Instant::now();
            let flush_interval = Duration::from_millis(250);
            loop {
                let timeout = flush_interval
                    .checked_sub(last_flush.elapsed())
                    .unwrap_or(Duration::from_millis(0));
                let msg = rx.recv_timeout(timeout);
                match msg {
                    Ok(LogMessage::Line(line)) => {
                        if let Ok(mut g) = log_file().lock() {
                            if let Some(ref mut w) = *g {
                                let _ = writeln!(w, "{line}");
                            }
                        }
                    }
                    Ok(LogMessage::Flush) | Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Ok(mut g) = log_file().lock() {
                            if let Some(ref mut w) = *g {
                                let _ = w.flush();
                            }
                        }
                        last_flush = std::time::Instant::now();
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return;
                    }
                }
            }
        })
        .expect("spawn jarvis_ios_log_writer");
}

fn write_to_file(line: &str) {
    // Non-blocking channel send; the writer thread does the actual disk I/O + flush.
    if let Some(tx_mutex) = LOG_TX.get() {
        if let Ok(g) = tx_mutex.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(LogMessage::Line(line.to_owned()));
                return;
            }
        }
    }
    // Fallback for the boot window before the writer thread has been initialized.
    if let Ok(mut g) = log_file().lock() {
        if let Some(ref mut w) = *g {
            let _ = writeln!(w, "{line}");
        }
    }
}

/// Best-effort flush — call on app background / shutdown to push pending lines to disk.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn jarvis_ios_flush_log() {
    if let Some(tx_mutex) = LOG_TX.get() {
        if let Ok(g) = tx_mutex.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(LogMessage::Flush);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core push (ring buffer + file)
// ---------------------------------------------------------------------------

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub fn jarvis_ios_debug_push(line: String) {
    let entry = format!("[Rust {}] {}", ts_prefix(), line);
    // `eprintln!` was previously called here too, but on iOS that goes through
    // Apple's NSLog/os_log pipeline which can stall the calling thread the same way
    // a synchronous file flush does. We keep stderr only on non-iOS targets where
    // it's strictly local (and useful during desktop tests).
    #[cfg(not(target_os = "ios"))]
    eprintln!("{entry}");
    write_to_file(&entry);
    if let Ok(mut g) = buffer().lock() {
        g.push_back(entry);
        while g.len() > MAX_LINES {
            g.pop_front();
        }
    }
}

fn push_raw(line: String) {
    #[cfg(not(target_os = "ios"))]
    eprintln!("{line}");
    write_to_file(&line);
    if let Ok(mut g) = buffer().lock() {
        g.push_back(line);
        while g.len() > MAX_LINES {
            g.pop_front();
        }
    }
}

// ---------------------------------------------------------------------------
// Tracing subscriber layer — captures ALL Bevy / crate log output
// ---------------------------------------------------------------------------

#[cfg(target_os = "ios")]
mod subscriber {
    use tracing::field::{Field, Visit};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    struct MsgVisitor(pub String);

    impl Visit for MsgVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{:?}", value);
            } else {
                self.0 += &format!(" {}={:?}", field.name(), value);
            }
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            if field.name() == "message" {
                self.0 = value.to_owned();
            } else {
                self.0 += &format!(" {}={value}", field.name());
            }
        }
    }

    pub struct FileLayer;

    impl<S: Subscriber> Layer<S> for FileLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            // Quiet/Off cuts ALL upstream tracing events too — they hit the same async writer.
            // We always allow ERROR through (you want to see crashes regardless).
            let level = event.metadata().level();
            let v = super::log_verbosity();
            let allow = match v {
                super::LOG_VERBOSITY_OFF => false,
                super::LOG_VERBOSITY_QUIET => *level <= tracing::Level::WARN,
                _ => true,
            };
            if !allow {
                return;
            }
            let meta = event.metadata();
            let target = meta.target();
            let mut visitor = MsgVisitor(String::new());
            event.record(&mut visitor);
            let line = format!(
                "[Trace {}]  {:5} {}: {}",
                super::ts_prefix(),
                level,
                target,
                visitor.0
            );
            super::push_raw(line);
        }
    }
}

// ---------------------------------------------------------------------------
// C-callable boot entry point (called from Swift before Bevy starts)
// ---------------------------------------------------------------------------

/// Called by Swift with:
///   - `log_path`: path for this session's log (e.g. `…/session_log.txt`)
///   - `prev_path`: path where the *previous* session log was already moved by Swift
///                  (purely informational; Rust just logs that rotation happened)
///
/// Also installs the global `tracing` subscriber so Bevy output is captured.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn jarvis_ios_set_log_file(
    log_path: *const std::ffi::c_char,
    prev_path: *const std::ffi::c_char,
) {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::EnvFilter;

    let path = unsafe {
        if log_path.is_null() {
            return;
        }
        std::ffi::CStr::from_ptr(log_path)
            .to_str()
            .unwrap_or("")
            .to_owned()
    };
    let prev = unsafe {
        if prev_path.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(prev_path)
                .to_str()
                .unwrap_or("")
                .to_owned()
        }
    };

    open_log_file(&path);

    // Honor JARVIS_IOS_LOG_VERBOSITY env var at boot (off|quiet|normal|debug or 0|1|2|3).
    if let Ok(v) = std::env::var("JARVIS_IOS_LOG_VERBOSITY") {
        let level = match v.trim().to_ascii_lowercase().as_str() {
            "off" | "0" => LOG_VERBOSITY_OFF,
            "quiet" | "1" => LOG_VERBOSITY_QUIET,
            "normal" | "2" => LOG_VERBOSITY_NORMAL,
            "debug" | "3" => LOG_VERBOSITY_DEBUG,
            _ => LOG_VERBOSITY_NORMAL,
        };
        set_log_verbosity(level);
    }

    // Install global tracing subscriber (only once).
    static SUBSCRIBER_INSTALLED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !SUBSCRIBER_INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(
                // bevy_egui::input emits a WARN every frame on iOS (no winit window).
                // bevy_winit, bevy_input, bevy_picking, and naga can fire several times per
                // touch/pan event. Logging any of those synchronously degrades pan smoothness.
                "info,wgpu=warn,naga=warn,bevy_render=warn,bevy_asset=warn,\
                 bevy_egui::input=off,bevy_winit=warn,bevy_input=warn,bevy_picking=warn,\
                 bevy_panorbit_camera=warn"
            ));
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(subscriber::FileLayer);
        let _ = tracing::subscriber::set_global_default(registry);
    }

    let hdr = format!(
        "=== JarvisIOS session start {} ===",
        chrono_ish_now()
    );
    push_raw(hdr);
    if !prev.is_empty() {
        push_raw(format!("(previous session log rotated to: {prev})"));
    }
}

#[cfg(not(target_os = "ios"))]
pub fn jarvis_ios_set_log_file_noop() {}

fn chrono_ish_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("unix={secs} ({h:02}:{m:02}:{s:02} UTC)")
}

// ---------------------------------------------------------------------------
// Public snapshot / clear (called from Swift + bridge)
// ---------------------------------------------------------------------------

pub fn jarvis_ios_debug_log_snapshot() -> String {
    buffer()
        .lock()
        .map(|g| g.iter().cloned().collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

pub fn jarvis_ios_debug_log_clear() {
    if let Ok(mut g) = buffer().lock() {
        g.clear();
    }
}

// ---------------------------------------------------------------------------
// Macro
// ---------------------------------------------------------------------------
//
// Two channels:
//   `jarvis_ios_line!("...")`           — gated by NORMAL verbosity (default).
//                                          Suppressed at QUIET/OFF.
//   `jarvis_ios_line!(crit: "...")`     — only suppressed at OFF; everything else passes.
//                                          Use for lifecycle/error messages.
//
// Format-args are not evaluated when the verbosity gate is closed, so verbose call sites
// in the hot path (frame counters, asset stats, mem snapshots) do not spend CPU on
// formatting when the user has dialed verbosity down.

#[macro_export]
macro_rules! jarvis_ios_line {
    (crit: $($arg:tt)*) => {{
        if $crate::debug_log::log_verbosity() != $crate::debug_log::LOG_VERBOSITY_OFF {
            let __s = format!($($arg)*);
            $crate::debug_log::jarvis_ios_debug_push(__s);
        }
    }};
    ($($arg:tt)*) => {{
        if $crate::debug_log::diag_logging_enabled() {
            let __s = format!($($arg)*);
            $crate::debug_log::jarvis_ios_debug_push(__s);
        }
    }};
}
