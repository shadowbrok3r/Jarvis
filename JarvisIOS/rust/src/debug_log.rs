//! In-app debug log buffer (read by Swift via `jarvis_ios_debug_log_snapshot`).
//! Populated by `crate::jarvis_ios_line!` alongside `eprintln!` for Xcode console.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LINES: usize = 700;

fn buffer() -> &'static Mutex<VecDeque<String>> {
    static BUF: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LINES)))
}

#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
fn ts_prefix() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}")
}

// Host `cargo build` does not compile iOS-only callers of `jarvis_ios_line!`; keep for the staticlib.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub fn jarvis_ios_debug_push(line: String) {
    let Ok(mut g) = buffer().lock() else {
        return;
    };
    let entry = format!("[Rust {}] {}", ts_prefix(), line);
    g.push_back(entry);
    while g.len() > MAX_LINES {
        g.pop_front();
    }
}

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

#[macro_export]
macro_rules! jarvis_ios_line {
    ($($arg:tt)*) => {{
        let __s = format!($($arg)*);
        eprintln!("{__s}");
        $crate::debug_log::jarvis_ios_debug_push(__s);
    }};
}
