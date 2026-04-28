# JarvisIOS

Swift package that links a Rust `staticlib` (`jarvis_ios`) via **swift-bridge** and a small **BridgeFFI** C header target.

**Official iOS workflow:** [xtool](https://github.com/xtool-org/xtool) on Linux (or wherever your xtool setup runs) — not a standalone Xcode project. You build the Rust static library for `aarch64-apple-ios`, place it where `Package.swift` expects (`RustLibs/`), then **`xtool dev`** (or other `xtool` commands) produces and runs the `.app`. Raw Xcode-only builds are not the supported path.

## Layout

| Path | Purpose |
|------|---------|
| `xtool.yml` | xtool config: bundle id, main SwiftPM **library** product, optional Info merge. |
| `Info.override.plist` | Keys merged into the generated app `Info.plist` (e.g. file sharing). |
| `rust/` | Cargo crate: `crate-type = ["staticlib"]`, `swift-bridge`, `build.rs` → `rust/generated/` (gitignored). |
| `Sources/JarvisIOS/` | App entry (`@main`), UI, **Bevy** `JarvisBevyView` (UIView + `CADisplayLink`), and **checked-in** bridge files (`SwiftBridgeCore.swift`, `jarvis_ios.swift`). |
| `Sources/BridgeFFI/` | `dummy.c` + `include/*.h` (C declarations for the bridge). |
| `RustLibs/` | **Gitignored.** `libjarvis_ios.a` for `aarch64-apple-ios` from `scripts/build-rust.sh`. |
| `scripts/build-rust.sh` | Release staticlib for device; copies `.a`, generated Swift, and headers. |
| `xtool/` | **Gitignored** xtool output (`.app`, `.xtool-tmp`, etc.) |

**Generated file policy:** After changing `rust/src/lib.rs` FFI, run `cargo build` in `rust/` (host) or `./scripts/build-rust.sh` when your Linux iOS Rust toolchain is set up, then commit updated `Sources/JarvisIOS/*.swift` and `Sources/BridgeFFI/include/*.h` if you want Swift to parse without a local codegen step.

## Prerequisites (Linux + xtool)

1. Install and configure **xtool** per [xtool-org/xtool](https://github.com/xtool-org/xtool) (`xtool setup`, Darwin Swift SDK, signing as needed).
2. Install the **Rust iOS target** and any linker/SDK pieces your setup expects (often aligned with what xtool documents for Swift + Rust on Linux), e.g.:

   ```bash
   rustup target add aarch64-apple-ios
   ```

3. From this directory, build the Rust static library, then drive the app with xtool:

   ```bash
   cd JarvisIOS
   ./scripts/build-rust.sh
   xtool dev
   ```

Use **`xtool dev`** (and related `xtool` subcommands from upstream docs), not a raw Xcode workflow. `Package.swift` uses a **`.library`** product named `JarvisIOS` so xtool can wrap it into an `.app` bundle.

Do not use **`#Preview`** in SwiftUI sources: xtool’s Swift toolchain does not ship the `PreviewsMacros` plugin, so preview macros fail at compile time.

## Optional app icon

Uncomment `iconPath` in `xtool.yml` and add a **1024×1024** PNG at that path when you want a custom icon.

## Bevy on iOS (in `jarvis_ios`)

- **`rust/src/ios_bevy.rs`**: Bevy **0.18** with `DefaultPlugins` minus **`WinitPlugin`**; a small plugin runs **before** `RenderPlugin` and injects a UIKit [`RawWindowHandle`](https://docs.rs/raw-window-handle/) from the Swift-hosted `UIView` so wgpu can create a Metal swapchain (same idea as embedding a view, not owning the whole UIKit app from Rust).
- **Swift**: `JarvisBevyView` passes the view pointer into `jarvis_renderer_*` FFI and ticks `jarvis_renderer_render` from a **`CADisplayLink`** on the main thread (mirrors the TailscaleDrive / Metal host pattern, but with Bevy instead of raw wgpu+egui).
- **Cross-compiling** `aarch64-apple-ios` on Linux may require your xtool / osxcross environment (e.g. `xcrun`, iOS SDK) for C dependencies inside the Bevy graph; host `cargo check` uses non-iOS stubs for the renderer FFI.

## Still to do

- **`bevy_vrm1`**, bundled `.vrm`, spring preset TOML, and animation paths (likely `[patch.crates-io]` like the desktop crate).
- **IronClaw / chat** from Swift and **desktop “phone as device”** parity (separate from xtool).

The desktop **`jarvis-avatar` binary** is still not linked on iOS; only the **`jarvis_ios`** staticlib ships in the xtool-built app.
