# JarvisIOS

Swift package that links a Rust `staticlib` (`jarvis_ios`) via **swift-bridge** and a small **BridgeFFI** C header target. **iOS builds are done on Linux with [xtool](https://github.com/xtool-org/xtool)**

## Layout

| Path | Purpose |
|------|---------|
| `xtool.yml` | xtool config: bundle id, main SwiftPM **library** product, optional Info merge. |
| `Info.override.plist` | Keys merged into the generated app `Info.plist` (e.g. file sharing). |
| `rust/` | Cargo crate: `crate-type = ["staticlib"]`, `swift-bridge`, `build.rs` → `rust/generated/` (gitignored). |
| `Sources/JarvisIOS/` | App entry (`@main`), UI, and **checked-in** generated `SwiftBridgeCore.swift` / `jarvis_ios.swift`. |
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

## Phase 2 (not implemented here)

- Metal / `CAMetalLayer` host in Rust.
- **Bevy** / `bevy_vrm1` in `JarvisIOS/rust` or a workspace member, with `[patch.crates-io]` for `vendor/bevy_vrm1` when required.
- Split desktop-only hub/MCP/`rfd` from the mobile crate with `cfg` / features.

The desktop **`jarvis-avatar` binary** is not linked on iOS as-is; `jarvis_ios` stays a separate crate until that split lands.
