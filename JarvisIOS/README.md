# JarvisIOS

Swift package that links a Rust `staticlib` (`jarvis_ios`) via **swift-bridge** and a small **BridgeFFI** C header target.

`Info.override.plist` already carries the standard camera/microphone usage strings for the merged app `Info.plist`. This package adds **`JarvisAudioSession`** (`.playAndRecord` + `defaultToSpeaker` + activate/deactivate) and **`JarvisMicLevelMonitor`** (input-node RMS → 0…1) wired on the **Logs** tab for debugging only; full capture, encoding, and streaming remain future work.

**Camera:** **`JarvisCameraSession`** + **`JarvisCameraPreviewRepresentable`** (`AVCaptureVideoPreviewLayer` via `UIViewRepresentable`) show a **debug-only** live preview on the Logs tab (front camera preferred, back wide-angle fallback). Permission is requested with `AVCaptureDevice.requestAccess(for: .video)`; denied state is shown in the UI. **No video frames are sent to the hub** in this build—transport would follow the same envelope style as **`IronclawHubWebSocket`** / hub text later, not raw binary WebSocket in this PR.

**Official iOS workflow:** [xtool](https://github.com/xtool-org/xtool) on Linux (or wherever your xtool setup runs) — not a standalone Xcode project. You build the Rust static library for `aarch64-apple-ios`, place it where `Package.swift` expects (`RustLibs/`), then **`xtool dev`** (or other `xtool` commands) produces and runs the `.app`. Raw Xcode-only builds are not the supported path.

## Layout

| Path | Purpose |
|------|---------|
| `xtool.yml` | xtool config: bundle id, main SwiftPM **library** product, optional Info merge. |
| `Package.swift` | SwiftPM **library** target, links `RustLibs/libjarvis_ios.a`, **`resources: [.copy("../assets")]`** so the desktop `assets/` tree (models, animations, …) ships in the app bundle for Bevy. |
| `Info.override.plist` | Keys merged into the generated app `Info.plist` (e.g. file sharing). |
| `rust/` | Cargo crate: `crate-type = ["staticlib"]`, `swift-bridge`, `build.rs` → `rust/generated/` (gitignored). |
| `Sources/JarvisIOS/` | App entry (`@main`), UI, **Bevy** `JarvisBevyView` (UIView + `CADisplayLink`), and **checked-in** bridge files (`SwiftBridgeCore.swift`, `jarvis_ios.swift`). |
| `Sources/BridgeFFI/` | `dummy.c` + `include/*.h` (C declarations for the bridge). |
| `RustLibs/` | **Gitignored.** `libjarvis_ios.a` for `aarch64-apple-ios` from `scripts/build-rust.sh`. |
| `scripts/build-rust.sh` | Release staticlib for device; copies `.a`, generated Swift, and headers. |
| `scripts/xcrun` | Optional Linux shim: implements `xcrun --show-sdk-path` (needs `SDKROOT`) and no-op `simctl` so `cc-rs` can probe an iOS SDK without a real Xcode `xcrun`. |
| `xtool/` | **Gitignored** xtool output (`.app`, `.xtool-tmp`, etc.) |

**Generated file policy:** After changing `rust/src/lib.rs` FFI, run `cargo build` in `rust/` (host) or `./scripts/build-rust.sh` when your Linux iOS Rust toolchain is set up, then commit updated `Sources/JarvisIOS/SwiftBridgeCore.swift`, `jarvis_ios.swift`, and `Sources/BridgeFFI/include/*.h` if you want Swift to parse without a local codegen step. **`build-rust.sh` only replaces those generated files** — other `Sources/JarvisIOS/*.swift` sources are never deleted by the script.

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

**Linker: `undefined symbol: __swift_bridge__$jarvis_renderer_…`** — Swift is newer than the Rust archive. From `JarvisIOS/`, run **`./scripts/build-rust.sh`** (aarch64-apple-ios **release** → copies `RustLibs/libjarvis_ios.a` + generated Swift/headers), then **`xtool dev`** again.

Do not use **`#Preview`** in SwiftUI sources: xtool’s Swift toolchain does not ship the `PreviewsMacros` plugin, so preview macros fail at compile time.

**Layout:** `MainShellView` wraps the tab `ZStack` in a **`GeometryReader`** and pins `JarvisBevyView` with an explicit `.frame(width:height:)` so the Metal `UIView` is not left at 0×0 (a common failure mode with `VStack` + `.frame(maxHeight: .infinity)` alone).

**Reload after hub sync:** use **`JarvisBevyView(sessionKey: bevySessionId, avatarTabVisible: …)`** only — do not apply **`.id(bevySessionId)`** on that view. Recreating the `UIView` on every session bump races two coordinators (0×0 vs real bounds) and cancels the bootstrap `Task` before `startRenderer`.

**Avatar tab only for Metal:** `JarvisBevyView` still receives a full-size layout behind About/Logs, but **bootstrap + `CADisplayLink` run only when the Avatar tab is selected**. Ticking Bevy/Metal while another tab is in front is brittle (e.g. **LiveContainer**) and can **`SIGABRT`** during the first `jarvis_renderer_render` after sync + switching tabs.

**Crash logs (SIGABRT on `CADisplayLink` → JarvisIOS):** Apple’s `.ips` JSON rarely symbols Rust frames. If in-app logs show **`render: app.update() enter`** without **`leave`**, the panic is inside Bevy’s first `update()`. The `jarvis_ios` crate avoids **`default_platform`** so **`bevy_gilrs`** and other desktop-only plugins are not linked (rebuild Rust after changing `Cargo.toml`).

**ATS / hub URL:** `Info.override.plist` enables **`NSAllowsLocalNetworking`** and **`NSAllowsArbitraryLoads`** so plain `http://` hub URLs (LAN, Tailscale IPs) are not blocked with `-1022`. Prefer **`https://`** to your hub when possible; remove or narrow arbitrary loads before a strict App Store submission if required.

## First-run greeting (Avatar tab)

**Who owns what:** **IronClaw (gateway)** owns persona, memory, and system prompts. The iOS app does **not** duplicate system prompts or embed a second “character brain” for chat.

**What runs locally on first successful Bevy bootstrap** (after `jarvis_renderer_new` and `CADisplayLink` attach, ~650 ms delay so the VRM root exists):

1. **`UserDefaults`** key `jarvis.avatar.did_first_greeting.<identity>` is checked. **`<identity>`** is derived from the device’s VRM choice: iOS model override (`jarvis.ios.overrideModelRelPath`) if set, else `avatar.model_path` from the synced hub `manifest.json`, else the literal `bundled-default`.
2. If the flag is **false**, the app tries **existing FFI only** — no new Rust in this path unless you add assets:
   - **`models/wave.vrma`** (one shot, `loop_forever = false`), else
   - **`models/idle_loop.vrma`** (one shot for the greet beat), else
   - **`animations/talk_nod.json`** (pose-library JSON via `jarvis_renderer_queue_anim_json`).
3. If a motion file is found and queued, the flag is set to **true** so the greet runs **once per avatar identity** (changing model or manifest path uses a new key).
4. If **no** candidate file exists under `JARVIS_ASSET_ROOT`, the flag is **not** set (hub sync or bundle can add assets later).

**Logging:** `JarvisIOSLog` category **`Greeting`** (in-app Logs tab + Console.app).

**IronClaw gateway:** The Swift client only has **`POST /api/chat/send`** with a **non-empty** user message or images (`GatewayChatViewModel.send`). There is **no** “greet without a user turn” API wired here; the app logs a **TODO** in the greeting flow instead of fabricating a fake user message. When the gateway exposes an explicit opening turn (or a documented empty-/system-message contract), hook it in one place next to `AvatarFirstRunGreeting` and stream via the existing **SSE** (`/api/chat/events`).

**TODOs**

- **TTS:** speak assistant lines on device or stream audio from gateway.
- **STT:** push-to-talk or continuous listen + send to gateway.
- **Barge-in:** cancel TTS / truncate SSE when the user starts speaking.
- **VRM expressions from Swift:** today’s FFI is VRMA + JSON pose clips only; a minimal `jarvis_ios_*` expression preset would reuse desktop expression paths if added later.

## Optional app icon

Uncomment `iconPath` in `xtool.yml` and add a **1024×1024** PNG at that path when you want a custom icon.

## Bevy on iOS (in `jarvis_ios`)

- **`rust/src/ios_bevy.rs`**: Bevy **0.18** with `DefaultPlugins` minus **`WinitPlugin`**; a small plugin runs **before** `RenderPlugin` and injects a UIKit [`RawWindowHandle`](https://docs.rs/raw-window-handle/) from the Swift-hosted `UIView` so wgpu can create a Metal swapchain (same idea as embedding a view, not owning the whole UIKit app from Rust).
- **VRM**: **`bevy_vrm1`** (`VrmPlugin` + `VrmaPlugin`) loads **`models/airi.vrm`** by default (same as desktop **`config/default.toml`**). Idle **VRMA** is optional: `IosAvatarSettings` starts with an empty idle path so you can ship a VRM alone; set `idle_vrma_path` in `ios_bevy.rs` (or a future config hook) when **`assets/models/idle_loop.vrma`** exists. Post-update systems match desktop **`avatar`**: hips root-motion lock + VRM root Y clamp.
- **Assets in the app bundle**: `Package.swift` copies the repo’s **`../assets`** tree into the SwiftPM resource bundle. **`JarvisBevyView`** sets **`JARVIS_ASSET_ROOT`** to that bundle’s **`assets`** directory before the first `jarvis_renderer_new`, so Bevy’s **`AssetPlugin`** reads files from disk like the desktop app. You still need the actual **`.vrm` / `.vrma`** files under `assets/models/` (they are not always committed—see repo **`assets/models/README.txt`**).
- **Swift**: `JarvisBevyView` passes the view pointer into `jarvis_renderer_*` FFI and ticks `jarvis_renderer_render` from a **`CADisplayLink`** on the main thread (mirrors the TailscaleDrive / Metal host pattern, but with Bevy instead of raw wgpu+egui).
- **Cross-compiling** `aarch64-apple-ios` on Linux: the crate trims **`bevy_audio`** (no `coreaudio-sys`), patches **`tracing-oslog`** to a stub, and forces **`blake3`** `pure` so most builds avoid Apple-only C tooling. A direct **`bevy_ecs`** dependency satisfies **`#[derive(Component)]` / `#[derive(Resource)]`** for `bevy` with `default-features = false`. If another dependency still runs `xcrun --show-sdk-path`, point **`SDKROOT`** at your iPhoneOS SDK and put this package’s **`scripts/`** first on **`PATH`** so the checked-in **`scripts/xcrun`** shim is used (same idea as a fake `xcrun` on a Linux box). Host `cargo check` uses non-iOS stubs for the renderer FFI.

## Desktop hub: profile sync over HTTP

The **`jarvis-avatar`** channel hub (same port as **`/ws`**, default **`6121`**) exposes:

| Method | Path | Purpose |
|--------|------|--------|
| `GET` | `/jarvis-ios/v1/manifest` | JSON **`schema`: `jarvis-ios.profile.v1`**, **`profile_id`**, **`revision`**, **`avatar` / `camera` / `graphics`** slices, **`assets`** (VRM + optional VRMA URLs), optional **`spring_preset`** (metadata + inlined TOML when the file exists). |
| `GET` | `/jarvis-ios/v1/asset/{*path}` | Raw file under desktop **`./assets/`** (e.g. `models/airi.vrm`). Rejects `..`. |
| `GET` | `/jarvis-ios/v1/config/spring-presets/{name}` | Preset file; **`name`** must be **`xxxxxxxxxxxxxxxx.toml`** (16 lowercase hex). |

When **`[ironclaw].auth_token`** is set in desktop config, send **`Authorization: Bearer <token>`** (same value as **`IRONCLAW_TOKEN`** / WS `module:authenticate`). Empty token → routes open (local dev).

Example (LAN or Tailscale IP):

```bash
export HUB=http://127.0.0.1:6121
curl -sS -H "Authorization: Bearer $IRONCLAW_TOKEN" "$HUB/jarvis-ios/v1/manifest" | head
```

The iOS app can use **`URLSession`** to fetch the manifest, download **`assets`** by URL, cache under Application Support, then refresh Bevy / Swift state. **`revision`** is currently **`1`** until the desktop bumps it when settings change.

## Still to do

- **Spring preset TOML** on iOS runtime (apply downloaded preset to Bevy; desktop already lists it in the manifest).
- **Swift client**: fetch manifest + assets, cache, drive `IosAvatarSettings` (or FFI) instead of bundle-only assets.
- **IronClaw / chat** from Swift and **desktop “phone as device”** parity (separate from xtool).

The desktop **`jarvis-avatar` binary** is still not linked on iOS; only the **`jarvis_ios`** staticlib ships in the xtool-built app.
