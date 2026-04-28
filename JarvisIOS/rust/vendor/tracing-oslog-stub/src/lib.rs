//! No-op `OsLogger` layer: satisfies `bevy_log` on iOS without compiling `wrapper.c` (no `xcrun`).

use tracing_core::Subscriber;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Default)]
pub struct OsLogger;

impl OsLogger {
    pub fn new<S, C>(_subsystem: S, _category: C) -> Self
    where
        S: AsRef<str>,
        C: AsRef<str>,
    {
        Self
    }
}

unsafe impl Send for OsLogger {}
unsafe impl Sync for OsLogger {}

impl<S> Layer<S> for OsLogger
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, _event: &tracing_core::Event<'_>, _ctx: Context<'_, S>) {
        // Intentionally silent — use tracing fmt / logcat on desktop or attach a real oslog build on macOS.
    }
}
