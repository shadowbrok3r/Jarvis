//! Minimal `staticlib` + swift-bridge surface for the JarvisIOS Swift package.
//! Full Bevy / jarvis-avatar integration is a later phase.

#[swift_bridge::bridge]
mod ffi {
    extern "Rust" {
        fn jarvis_ios_version() -> String;
    }
}

pub fn jarvis_ios_version() -> String {
    format!("jarvis_ios {}", env!("CARGO_PKG_VERSION"))
}
