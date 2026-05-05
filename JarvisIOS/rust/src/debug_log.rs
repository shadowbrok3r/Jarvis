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
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LINES: usize = 1200;

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
// Persistent log file (survives crash)
// ---------------------------------------------------------------------------

static LOG_FILE: OnceLock<Mutex<Option<BufWriter<std::fs::File>>>> = OnceLock::new();

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
                *g = Some(BufWriter::with_capacity(4096, f));
            }
        }
        Err(e) => {
            eprintln!("[debug_log] failed to open log file {path}: {e}");
        }
    }
}

fn write_to_file(line: &str) {
    if let Ok(mut g) = log_file().lock() {
        if let Some(ref mut w) = *g {
            let _ = writeln!(w, "{line}");
            let _ = w.flush(); // synchronous flush — survives hard kill
        }
    }
}

// ---------------------------------------------------------------------------
// Core push (ring buffer + file)
// ---------------------------------------------------------------------------

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub fn jarvis_ios_debug_push(line: String) {
    let entry = format!("[Rust {}] {}", ts_prefix(), line);
    eprintln!("{entry}");
    write_to_file(&entry);
    if let Ok(mut g) = buffer().lock() {
        g.push_back(entry);
        while g.len() > MAX_LINES {
            g.pop_front();
        }
    }
}

// Internal: push a pre-formatted tracing event line (no extra prefix added).
fn push_raw(line: String) {
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
            let meta = event.metadata();
            let level = meta.level();
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

    // Install global tracing subscriber (only once).
    static SUBSCRIBER_INSTALLED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !SUBSCRIBER_INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(
                // bevy_egui::input emits a WARN every frame on iOS (no winit window).
                // Logging it synchronously to disk causes severe frame-rate degradation.
                "info,wgpu=warn,naga=warn,bevy_render=warn,bevy_asset=info,bevy_egui::input=off"
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

#[macro_export]
macro_rules! jarvis_ios_line {
    ($($arg:tt)*) => {{
        let __s = format!($($arg)*);
        $crate::debug_log::jarvis_ios_debug_push(__s);
    }};
}
