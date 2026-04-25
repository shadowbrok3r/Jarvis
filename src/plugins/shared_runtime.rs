//! Shared multi-thread Tokio runtime exposed to the rest of the app as a Bevy
//! resource. The rest of the codebase spawns its own per-service runtimes
//! (hub, gateway, mcp, tts) — this one exists specifically so **Bevy-side
//! systems and egui callbacks** can run ad-hoc async work (e.g. calling
//! `KimodoClient::generate_motion` from the debug UI) without pulling a new
//! runtime out of thin air every time.
//!
//! Without this, `futures::executor::block_on` inside an egui callback panics
//! the moment the future touches anything Tokio-backed (timers,
//! `tokio::sync::broadcast`, etc.) with:
//!
//!   "there is no reactor running, must be called from the context of a
//!    Tokio 1.x runtime".

use std::sync::Arc;

use bevy::prelude::*;
use tokio::runtime::{Builder, Runtime};

/// Handle to a long-lived multi-thread Tokio runtime.
///
/// Clone is cheap (it's `Arc`-backed). The runtime lives for as long as the
/// app does; dropping the last handle on shutdown will gracefully stop it.
#[derive(Resource, Clone)]
pub struct SharedTokio {
    rt: Arc<Runtime>,
}

impl SharedTokio {
    /// Spawn a future onto the shared runtime. Returns a `JoinHandle` the
    /// caller can inspect (`is_finished`) or drop for fire-and-forget use.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.rt.spawn(fut)
    }

    /// Block the calling thread on `fut` inside the runtime context. Useful
    /// when the caller genuinely wants a synchronous answer (e.g. a
    /// short-lived HTTP probe for a status indicator). Avoid for work that
    /// can exceed a few hundred milliseconds — the Bevy main thread will
    /// stall.
    #[allow(dead_code)]
    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    /// Raw handle if a caller needs to hand out an `&Handle` (reqwest-eventsource etc.).
    #[allow(dead_code)]
    pub fn handle(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }
}

pub struct SharedRuntimePlugin;

impl Plugin for SharedRuntimePlugin {
    fn build(&self, app: &mut App) {
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("jarvis-shared")
            .build()
            .expect("failed to build shared Tokio runtime");
        app.insert_resource(SharedTokio { rt: Arc::new(rt) });
    }
}
