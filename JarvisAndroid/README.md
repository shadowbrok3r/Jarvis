# JarvisAndroid

Native Android build of the Bevy VRM avatar. Sibling of `JarvisIOS/`, but unlike iOS it
**depends on the `jarvis-avatar` library crate** rather than re-implementing the avatar
stack — `src/plugins/` was promoted into `src/lib.rs` for exactly this.

There is no Java, Kotlin, or Gradle. Bevy owns the `NativeActivity` event loop; the APK is
packaged by `cargo-apk2` through the `cargo egui-mobile` wrapper from a local EguiMobile
checkout (`EGUI_MOBILE`).

## Why not EguiMobile's runtime

`egui-android` is `eframe::run_native` — it owns the winit event loop, and so does Bevy.
winit allows one `EventLoop` per process, Android gives the process one `ANativeWindow`, and
both `#[bevy_main]` and `egui_mobile::app!` emit `#[no_mangle] android_main`. Only one can
exist. What this app reuses from EguiMobile is the **toolchain**: `cargo egui-mobile`
resolves the SDK/NDK/JDK and drives `cargo apk2` + adb. Its `EguiNativeActivity` /
`EguiImeBridge` JNI sources become reusable when on-device text input is wanted — the tree
is on egui 0.35, which matches EguiMobile exactly.

## Layout

| Path | Purpose |
|------|---------|
| `rust/Cargo.toml` | cdylib + `[package.metadata.android]`. Own workspace, own lockfile. |
| `rust/src/lib.rs` | `#[bevy_main]` entry: asset source, settings, plugin registration. |
| `rust/src/android_boot.rs` | Logger, `ring` crypto provider, internal-storage paths. |
| `rust/src/asset_bootstrap.rs` | First-run APK `assets/` → internal storage extraction. |
| `rust/src/render_probe.rs` | Logs adapter + device limits at startup. |
| `rust/assets/` | **Generated** by `scripts/stage-assets.sh`. Not hand-edited. |
| `scripts/stage-assets.sh` | Curates the bootstrap asset set + writes `index.txt`. |

## Build

```bash
./JarvisAndroid/scripts/stage-assets.sh
```

```bash
cd JarvisAndroid/rust && cargo egui-mobile build -a --release
```

APK lands at `JarvisAndroid/rust/target/release/apk/jarvis_android.apk`.

Build, install, and launch on a USB-attached phone:

```bash
cd JarvisAndroid/rust && cargo egui-mobile run -a --release
```

Wireless (one-time phone setup in EguiMobile's `ANDROID_SETUP.md`):

```bash
cd JarvisAndroid/rust && cargo egui-mobile run -a --release --tcp <phone-lan-ip>
```

Fast compile-check without linking or a device:

```bash
cd JarvisAndroid/rust && cargo ndk -t arm64-v8a check
```

Logs — the CLI's default logcat tags are ComfyUI-specific, so pass them explicitly:

```bash
adb logcat -s jarvis:V bevy:V wgpu:V RustStdoutStderr:V
```

## Bundled files

The desktop `assets/` tree is large (full VRM models can be hundreds of MB); it is never
packaged. `stage-assets.sh` copies a small bootstrap set into `rust/assets/` and writes
`index.txt`, because `AAssetDir` cannot enumerate subdirectories.

Index paths are relative to **internal storage**, not the asset root, so `config/` and a
top-level `user.toml` ship alongside `assets/`. On first launch everything is extracted into
`/data/data/com.kingsofalchemy.jarvis/files/`, and a marker named after a hash of `index.txt`
suppresses re-extraction. Bevy then reads `assets/` through a `FileAssetReader` registered as
the default `AssetSource`, necessary because Bevy's `AndroidAssetReader` ignores
`AssetPlugin.file_path` and copies whole files into RAM.

`index.txt` starts with a `# content <sha256>` line covering every staged file's **contents**.
Without it the marker only tracked the file *list*, so editing a staged file left the stale
copy on the device and the next run silently used old settings.

**Per-model overrides must ship.** `config/ModelOverrides/<stem>/` is where
`avatar_defaults.json` lives, and that file is what selects layer-stack idle over VRMA idle —
without it the avatar stands in bind pose. `<stem>` drops a trailing `.ios`
(`3.ios.vrm` → `3`), so `stage-assets.sh` derives the directory from the chosen model rather
than hardcoding it.

Larger models are meant to arrive at runtime over the desktop hub's existing
`/jarvis-ios/v1/manifest` + `/jarvis-ios/v1/asset/{path}` endpoints into that same directory
(see [src/plugins/jarvis_ios_hub.rs](../src/plugins/jarvis_ios_hub.rs)). **That client is not
written yet** — see Status.

## Config

`config/default.toml` is compiled in with `include_str!`, then two overlays apply in order
(`Settings::load_layered`), then `JARVIS__*` env vars. `Settings::load()` is untouched and
remains the desktop path.

1. `<internal>/user.toml` — extracted from the APK. All the desktop tuning, **credentials
   blanked** by `stage-assets.sh`.
2. `<external>/user.toml` — the credentials, kept out of the APK and out of git.

App-specific external storage is `adb push`-able without root or a debuggable build, so the
tokens reach the phone in one command:

```bash
adb push config/user.toml /sdcard/Android/data/com.kingsofalchemy.jarvis/files/user.toml
```

Without that push, `ironclaw_chat`, `zeroclaw_*`, and `home_assistant*` are registered but
early-return on their empty tokens.

## Hub client

`src/hub_client.rs` is the mirror of the desktop's `ChannelHubPlugin`: the desktop *hosts* the
hub, a phone is a peer. It connects to `ws://<host>/ws`, sends `module:authenticate` then
`module:announce`, and republishes inbound frames as `WsIncomingMessage` — the same message
`hub_pose_apply`, `expressions`, and `look_at` already consume on desktop. It accepts both raw
`{type,data,metadata}` and superjson `{json:{…},meta:{}}`, matching the hub's own
`handle_peer_text`, and reconnects every 5 s because a phone roams and sleeps.

Enabled by a new setting, empty on desktop so nothing changes there:

```toml
[ironclaw]
client_url = "http://<hub-host>:6121"
```

Put it in the external `user.toml` alongside the tokens. The menu bar shows a `• hub` dot,
green once frames arrive.

**Not yet done:** manifest fetch and asset download over `/jarvis-ios/v1/`. Until that lands
the bootstrap set stays baked into the APK, which is why it is 126 MB rather than ~20 MB.

`config/user.toml` ships as the device overlay. **Its credential values are blanked by
default** — `auth_token`, `webhook_secret`, `ha_token`, `api_key`, `password` — because an APK
is trivially unzipped. Everything else (camera, graphics, light rig) is copied byte-for-byte.
Set `INCLUDE_SECRETS=1` to ship them verbatim; the plugins that would use them
(`ironclaw_chat`, `zeroclaw_*`, `home_assistant*`) are not registered on Android yet, so the
blanks cost nothing today.

Three boot-time fixups handle values that only make sense on a desktop:

- **cwd** is set to internal storage. Plugins resolve `config/...` relatively and
  `vrm_model_overrides_dir` calls `current_dir()`, which is `/` on Android — every such read
  otherwise fails with `EACCES`.
- **pose library** is repointed at `<internal>/assets/{poses,animations}`. `default.toml`
  aims it at `~/.config/@proj-airi/...`, and Android has no `$HOME`, so `expand_home` leaves
  a literal `~/` that never resolves and `idle_clip` never finds its clip by name.
- **`msaa_samples` is clamped to 4.** Overlay config may ask for 8; Adreno supports `[1, 2, 4]` for
  `Depth32Float` (WebGPU only guarantees `[1, 4]`), and Bevy treats the failed
  `create_texture` as fatal — the app quits on the first frame.

## Overlay

`rust/src/ui.rs` draws a touch-first egui overlay on top of the 3D scene, themed with the
desktop `egui_jarvis_theme.json` and phosphor icons (`jarvis_avatar::{egui_theme, icons, theme}`
are reused directly — no copies).

- **Menu bar** — an `egui::Area`, not a panel. egui 0.35 removed root-level `TopBottomPanel`;
  `Panel` now only nests inside a `Ui`, and a `CentralPanel` would paint over the avatar.
- **Layers** — master toggle plus per-layer enable/weight, driving `LayerStackHandle` live.
- **Console** — a `TextEdit` whose submit writes a `TtsSpeakMessage`, so it doubles as the
  text-input and the audio test surface.

Hit targets are widened at startup (`button_padding`, `interact_size.y = 34`) because the
desktop style is tuned for a mouse.

**Safe area is provisional.** `AndroidApp::content_rect()` reports the full window under
edge-to-edge (the app really is drawing behind the status bar), so the overlay applies a
26pt floor instead. Real per-edge insets need a JNI `WindowInsets` read.

## Text input

**Ordinary typing already works with no bridge.** Verified on device: tapping a `TextEdit`
raises the soft keyboard (`dumpsys input_method` → `mInputShown=true`), and typing, space,
backspace, and Enter-to-submit all reach egui. `bevy_egui`'s `process_ime_system`
(`input.rs:818-821`) forwards `platform_output.ime.is_some()` to winit's `set_ime_allowed`,
which on Android maps to an implicit `show_soft_input` / `hide_soft_input`
(`winit-0.30.13/src/platform_impl/android/mod.rs:918-924`).

That path has no `InputConnection`, so it has no swipe typing, no Gboard spacebar-trackpad
cursor, and no IME composition for CJK. EguiMobile's hidden-`EditText` bridge supplies those,
and is wired up behind an **opt-in feature** because it *replaces* a working path:

```bash
cd JarvisAndroid/rust && cargo egui-mobile build -a --release --features ime-bridge
```

The two are mutually exclusive. With `ime-bridge` on, `EguiGlobalSettings::enable_ime` is set
to `false` — an implicit `hide_soft_input` on the DecorView token kills a keyboard served by
the `EditText`, and the follow-up implicit show is then ignored ("view is not served").

### How the bridge is vendored

`egui-android` cannot be used as a dependency: it pulls `eframe` unconditionally (a second
winit + wgpu in a binary where Bevy already owns both), its `host::set_android_app` is
`pub(crate)` and only called from `egui_android::run()`, and `host.rs` calls
`android_activity::input::pointer_probe()`, which exists only in EguiMobile's
`android-activity` fork.

But `ime_bridge.rs` touches exactly one thing from `host` — `with_native_activity`, at nine
call sites — and that function needs only a JavaVM and Activity pointer, both of which
`bevy::android::ANDROID_APP` already provides. So `vendor/egui-android-ime/` is a byte copy of
`ime_bridge.rs` plus a ~40-line `activity.rs` shim, with `egui` + `jni` + `log` as its only
dependencies. **EguiMobile needs no changes, and its `android-activity` fork is not required.**

Re-sync after pulling EguiMobile (it fails loudly if upstream adds a new `crate::host` use):

```bash
./JarvisAndroid/scripts/sync-ime-bridge.sh
```

### The driver, and the mistake that made v1 silent

`ime_bridge::apply_pending` does **not** deliver events to egui — it fills a `Vec<egui::Event>`
the caller must inject. Upstream does that at `egui-android/src/lib.rs:153-158`:

```rust
ui.ctx().input_mut(|i| {
    i.raw.events.extend(events.iter().cloned());
    i.events.extend(events);
});
```

Both queues matter: `raw` is what egui replays, `events` is what the already-begun pass reads.
The first driver called `apply_pending` and then dropped the vector on the floor, so the
keyboard appeared and nothing typed.

Three other pieces are load-bearing and were also missing:

- **Focus pinning.** While the keyboard is hot, `surrender_focus_on` is forced to `Never` and
  focus is re-requested before *and* after the UI runs. Without it the field blurs between
  characters — upstream's comment is "first letter, then silence until retap".
- **`request_repaint_after(100 ms)`** while hot, as a backstop for a missed `nativeImeWake`.
- **Seed-on-open only.** `sync_focused_text_edit` pushes the whole document to the `EditText`
  when the keyboard opens or the field changes, never per keystroke — `setText` resets the
  caret and triggers `invalidateInput`. It retries until the undoer has a stable snapshot,
  because seeding early pushes `""` and every later IME op edits an empty mirror.

Deliberately **not** ported: the `last_ime` pinning and the open/hidden recovery counters
(`ime_recover_arm`, `ime_seen_open`). Those exist to survive `egui-winit` calling
`set_ime_allowed(false)`, and `enable_ime = false` removes that adversary entirely.

`bridge.rs` ships with `TRACE = true`, so bring-up is visible under `adb logcat -s EguiIme:V`.

### The one hard requirement

`bridge.rs` compares the running activity's class name to
`com.github.egui_mobile.EguiNativeActivity` by exact string equality and every entry point
bails when it does not match — a subclass fails. So `has_code = true`, `java_sources = "java"`,
and that activity name are mandatory. They ship in **both** builds; `EguiNativeActivity` only
subclasses `android.app.NativeActivity` to pre-load the native lib and add a 1×1 alpha-0
`EditText`, so it is inert when the feature is off.

## Audio

`plugins::tts::TtsPlugin` is registered. It listens for `TtsSpeakMessage`, POSTs to
`settings.tts.kokoro_url`, and plays through `bevy_audio`; the Console's send button emits that
message. AAudio already initialises on device (`AAudioStreamBuilder_openStream() ... AAUDIO_OK`).

`uses_cleartext_traffic = true` is required: the hub, Kokoro, and
Home Assistant are typically plain `http://` on the LAN, and targetSdk 28+ blocks cleartext by
default. This is the same role `NSAllowsLocalNetworking` plays on iOS.

**Untested** — the phone must be on the same LAN as the Kokoro host for this to do anything.

## Icon

`icon/{legacy,foreground,background}.svg` → `scripts/render-icon.sh` → `rust/res/`, giving
adaptive icons (`mipmap-anydpi-v26`) plus legacy PNGs at all five densities. Re-run the
script after editing an SVG.

```bash
./JarvisAndroid/scripts/render-icon.sh
```

## What is compiled out on Android

Gated `#[cfg(not(target_os = "android"))]` in `src/plugins/mod.rs` and `src/lib.rs`:

| Module | Reason |
|--------|--------|
| `plugins::debug_ui` | `rfd` has no Android backend; the UI is hover- and dock-driven. |
| `plugins::rig_editor::rig_editor_draw_gizmo` | Reads `DebugUiState`. The rest of `rig_editor` compiles. |
| `plugins::intent_calibration` | Drives the MCP tool surface. |
| `plugins::undo_history` | Reads `DebugUiState`. |
| `mcp` | rmcp server; a phone is a hub client, not a host. |

`channel_server` **is** compiled — it owns the hub message types core plugins import — but
`ChannelHubPlugin` (the axum listener) is never registered.

## Status

**Runs on hardware.** Verified on a physical Android device (Vulkan): the VRM renders with
MToon shading and shadows, the layer-stack idle clip animates, the material/MToon overrides
apply, and the app survives a home-button suspend/resume with the same PID and no panics.

The full desktop config is ported: `user.toml`, all `ModelOverrides`, spring presets,
`anim_layer_sets`, `pose_graph`, `color_scheme`, the pose/animation libraries, and the IBL
environment map. The APK is ~124 MB as a result.

Known issues:

- **High-joint skins can exceed Bevy's 256-joint limit.** `bevy_gltf` warns when a skin has
  more joints than `bevy_pbr::render::skin::MAX_JOINTS`. This is a property of the model, not
  of Android, so the desktop hits it too — worth confirming nothing deforms.
- **If `camera.initial_radius` exceeds `camera.max_radius`** in overlay config, opening
  framing on a phone-shaped viewport is clipped.
- **`bevy_log` logs `Could not set global logger`** because `android_logger` claims the `log`
  global during boot, before `LogPlugin` exists. Both sinks still work (`I/jarvis` and
  `I/RustStdoutStderr`); the line is noise.
- **`bevy_gilrs` errors on startup** — no gamepad support on Android. Harmless.

Verified on device: avatar render, layer-stack idle animation, material/MToon overrides, IBL
environment map, suspend/resume, launcher icon, egui overlay (menu bar, Layers, Console), and
soft-keyboard typing via the default path. Touch orbit and pinch-zoom work; pan may need
`camera.focus_follow_vrm = false` in overlay config.

Also verified with `--features ime-bridge`: `onCreateInputConnection` fires, the document
mirrors (`setImeState restart=true => "bridge works" sel=12..12`), and typing, space, backspace
and Enter-to-submit all land. TTS round-trips — Kokoro returned 57 KB and the WAV validated at
24 kHz mono — and the credentialed plugins load once the external `user.toml` is pushed.

Known issues:

- **A2F visemes fail on device**: `A2F after Kokoro failed: gRPC transport: transport error`.
  Audio still plays (`tx_ready` is sent regardless of A2F), but the mouth does not move. The
  A2F gRPC host is presumably not reachable from the phone.
- **High-joint skins can exceed Bevy's 256-joint limit** — a property of the model, so desktop hits
  it too.
- **If `camera.initial_radius` exceeds `camera.max_radius`** in overlay config, opening framing
  on a phone-shaped viewport is clipped.
- **`bevy_log` logs `Could not set global logger`** — `android_logger` claims the `log` global
  during boot. Both sinks work; the line is noise.
- **`bevy_gilrs` errors on startup** — no gamepad support on Android. Harmless.

Not started:

- **Asset sync** — the hub client carries messages but does not yet fetch the manifest or
  download models, which is why 127 MB is baked into the APK instead of synced.
- **Real safe-area insets** — see the Overlay section.

### Why a hub *peer* needs `HubBroadcast`

`ironclaw_chat` and `zeroclaw_chat` take `Res<HubBroadcast>` **un-optioned**, and only
`spawn_hub_thread` (the axum server) ever inserts one — so on Android those systems failed
parameter validation and Bevy's default error handler panicked the app at startup.

`HubBroadcast::detached(module_name)` returns a handle with no server behind it plus a
`HubOutbox` that yields each published frame as wire-ready JSON. `HubClientPlugin` inserts both
at **plugin-build time** (the chat systems run at `PostStartup`, so a Startup system would be
too late) and forwards the queue over the WebSocket. Desktop is untouched: it never constructs
either type, and `spawn_hub_thread` still installs the real `HubBroadcast` at Startup.
