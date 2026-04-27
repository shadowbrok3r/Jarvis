# Jarvis Avatar — External Integrations & Setup Reference

External services, config keys, env vars, and how each piece is expected to connect.

## Architecture at a glance

The app is both a **client** to some services and a **host** for others:

- **Hosts:** IronClaw channel hub (`:6121`), MCP streamable-HTTP server (`:6123`), Bevy VRM renderer
- **Client of:** IronClaw Gateway (chat/SSE), Audio2Face gRPC, Kokoro TTS
- **Peers that connect to us:** Kimodo motion service, `server.mjs` (HA voice bridge), `ironclaw-proxy`

All config lives in `config/default.toml`. Override per-machine with `config/user.toml` or hierarchical env vars prefixed `JARVIS__` (e.g. `JARVIS__TTS__KOKORO_URL=http://...`).

Two env vars are read directly in `src/main.rs`:

- `IRONCLAW_TOKEN` → overrides `[ironclaw].auth_token` (hub auth)
- `IRONCLAW_GATEWAY_TOKEN` → overrides `[gateway].auth_token`

---

## 1. Kimodo (NVIDIA motion generation)

**What it does:** Text-to-motion. You give it a prompt ("wave hello", "sit down crossed-legged"), it produces VRM bone rotations that the avatar plays back. Exposed as the MCP tool `generate_motion`.

**Files:**
- `src/kimodo/mod.rs` — client logic (timeouts, request IDs, envelope marshalling)
- `src/mcp/mod.rs` — MCP `generate_motion` handler
- Config: `[kimodo]` in `config/default.toml`

**How the app knows it's "set up":**
Kimodo does not have a URL or token in this app's config. It is a **peer** that connects *to* your channel hub at `ws://<host>:6121/ws`, announces itself with `module:announce { name: "kimodo" }`, and starts listening for `kimodo:generate` envelopes. So "set up" = Kimodo has authenticated and announced on the hub. The Services status row **Kimodo Peer (hub WS)** reads online when that has happened.

Config keys the app does use:
- `kimodo.generate_timeout_sec` (default 180) — how long we wait for a reply
- `kimodo.default_duration_sec` (default 3.0) — motion length when the caller omits it
- `kimodo.default_steps` (default 100) — denoising iterations

**What Kimodo itself needs:**
- The Python Kimodo motion service running (NVIDIA's motion LLM behind it)
- Hub URL: `ws://<jarvis-avatar-host>:6121/ws`
- Module name containing `"kimodo"` (e.g. `kimodo`, `kimodo-peer`)
- Matching `IRONCLAW_TOKEN` if you set one on the hub
- It must handle these envelope types:
  - `kimodo:generate` → `{ prompt, duration, steps, stream, saveName }`
  - `kimodo:play-animation` → `{ filename }`
- And emit:
  - `kimodo:status` — progress updates
  - `vrm:apply-pose` — streaming frames when `stream: true`
  - `kimodo:generate:result` — final motion (non-streaming path)

**Start-to-finish (your example):**
1. Start jarvis-avatar (the hub binds to `0.0.0.0:6121` automatically).
2. Decide whether to require auth. If yes, `export IRONCLAW_TOKEN=<secret>` before launch, or set `[ironclaw].auth_token` in config.
3. Launch the Kimodo Python service with env/args pointing it at:
   - `AIRI_WS_URL=ws://<jarvis-avatar-host>:6121/ws`
   - `IRONCLAW_TOKEN=<same secret>` (if auth is on)
4. Confirm under **View → Services (all)** (or the Channel hub window) that the Kimodo peer is online.
5. Trigger generation one of two ways:
  - Via an MCP client: call the `generate_motion` tool at `http://<host>:6123/mcp` with `{ prompt, duration, steps, stream, save_name, timeout_sec }` (`timeout_sec` optional override for slow runs).
   - Via any hub peer: publish `kimodo:generate` directly to the hub.
6. If `save_name` is set, the animation is persisted to `[pose_library].animations_dir` and replayable via `play_saved_animation`.

**Fallback:** if Kimodo never connects, `generate_motion` calls simply time out after 180 s. Nothing else breaks — pose/expression/bone tools all continue to work.

---

## 2. Audio2Face-3D (NVIDIA A2F)

**What it does:** Streams PCM-16 audio in and gets back ARKit-style blendshape keyframes, which drive the avatar's lipsync and facial expressions.

**Files:**
- `src/a2f/mod.rs` — gRPC streaming client
- `src/a2f/pb.rs` — generated protobuf types
- `build.rs` — compiles the `.proto` files at build time
- `proto/nvidia_ace.*.proto` — vendored NVIDIA ACE proto files
- `src/mcp/mod.rs` — MCP tools `a2f_status`, `a2f_configure`
- Config: `[a2f]` in `config/default.toml`

**How the app knows it's "set up":**
- `a2f.enabled = true` (default)
- `a2f.endpoint = "localhost:52000"` — gRPC endpoint; accepts bare `host:port` or `http://host:port`
- `a2f.health_url = "http://localhost:8000/v1/health/ready"` — HTTP GET; the app expects JSON with a `"status": "ready"` field

When both gRPC dials and the health probe returns "ready", the `a2f_status` MCP tool returns `{ enabled: true, health: "READY" }` and the Services panel shows A2F as online.

**What A2F itself needs:**
- NVIDIA Audio2Face-3D Docker container running.
- gRPC service `A2FControllerService` exposed on the configured port, implementing `ProcessAudioStream(stream AudioStream) → stream AnimationDataStream` with the bidirectional flow: `AudioStreamHeader` → PCM chunks → `EndOfAudio` → keyframes stream back.
- HTTP health endpoint returning `{"status":"ready"}` when warm.
- The client sends sane defaults (upper/lower face strength, eyelid offsets, ~55 ARKit blendshape multipliers, joy 0.5, emotion strength 0.6) — you can tune these at runtime via the `a2f_configure` MCP tool.

**Start-to-finish:**
1. Follow NVIDIA's docs to pull and run the A2F-3D container. Make sure it exposes gRPC on 52000 and the health endpoint on 8000 (or pick your own ports).
2. Edit `[a2f]` in your config to match the ports/hostnames you used.
3. Start jarvis-avatar.
4. Call the MCP tool `a2f_status` — it should report `READY`.
5. Audio coming out of the TTS path (Kokoro) flows into A2F, which emits blendshape frames to the avatar.

**Fallback:** with A2F disabled or offline, lipsync is gone but TTS, chat, and everything else still works. Nothing else depends on A2F.

---

## 3. Kokoro TTS

**What it does:** Converts text to WAV audio over plain HTTP. The avatar speaks chat responses through this.

**Files:**
- `src/plugins/tts.rs` — Bevy plugin + Tokio HTTP worker thread
- Config: `[tts]` in `config/default.toml`

**How the app knows it's "set up":**
- `tts.enabled = true` (default)
- `tts.kokoro_url = "http://192.168.4.8:8880"` — base URL, no trailing slash
- `tts.voice = "af_heart"` — voice ID

The app probes `GET {kokoro_url}/v1/models` as a health check. If it 200s, the Services panel shows TTS online.

**What Kokoro itself needs:**
- A Kokoro FastAPI server (or any OpenAI-compatible TTS) reachable at the configured URL.
- It must accept `POST /v1/audio/speech` with body:
  ```json
  { "model": "kokoro", "voice": "af_heart", "input": "...", "response_format": "wav" }
  ```
  and return raw WAV (PCM-16 mono) bytes.
- `GET /v1/models` must list available models (used for the health probe).

**Start-to-finish:**
1. Deploy Kokoro-FastAPI (the common OSS build) or equivalent.
2. Pick a voice from the available list (e.g. `af_heart`, `bf_emma`).
3. Update `[tts]` with `kokoro_url` and `voice`.
4. Restart jarvis-avatar. Send any chat message — the reply should speak.

**Fallback:** if disabled or unreachable, TTS requests are silently dropped. Chat keeps working; there's just no audio. No error spam, just a warning log on first failure.

---

## 4. IronClaw Gateway (chat backend)

**What it does:** The actual chat/LLM backend. Provides thread management and streams assistant responses over Server-Sent Events.

**Files:**
- `src/ironclaw/client.rs` — HTTP client
- `src/plugins/ironclaw_chat.rs` — Bevy plugin that drives the SSE stream
- `src/config.rs` lines 182–202 — `GatewaySettings`
- Config: `[gateway]` in `config/default.toml`

**How the app knows it's "set up":**
- `gateway.base_url = "http://192.168.4.8:3000"`
- `gateway.auth_token` — populated from the `IRONCLAW_GATEWAY_TOKEN` env var at startup (leave empty in the file)
- `gateway.default_thread_id` — written back to config after first-run thread creation
- `gateway.request_timeout_ms` (15000), `gateway.history_limit` (50)

Gateway endpoints the app calls:
- `GET /api/chat/threads`
- `POST /api/chat/thread/new`
- `GET /api/chat/history`
- `POST /api/chat/send`
- `GET /api/chat/events` (SSE, with `Last-Event-ID` for resume)
- `GET /api/health` or `/api/chat/health`

**Start-to-finish:**
1. Deploy the IronClaw gateway (out of scope for this repo).
2. Get its URL and a bearer token from the IronClaw console.
3. Point `[gateway].base_url` at it.
4. `export IRONCLAW_GATEWAY_TOKEN=<token>` before launching jarvis-avatar.
5. First run auto-creates a thread named `jarvis-avatar` and writes the ID back to config.
6. Chat panel is alive when the Services entry "IronClaw Gateway" reads "online".

**Fallback:** no gateway = no chat. The SSE client reconnects with exponential backoff and doesn't spam logs. Everything else (rendering, pose library, MCP tools) is unaffected.

---

## 5. IronClaw Channel Hub (hosted by this app)

**What it does:** The message bus between every peer — server.mjs, ironclaw-proxy, Kimodo, anything else you plug in.

**Files:**
- `src/plugins/channel_server.rs` — hub + HTTP broadcast endpoint
- Config: `[ironclaw]` in `config/default.toml`

**How the app knows it's "set up":**
- `ironclaw.bind_address = "0.0.0.0:6121"` — bind socket
- `ironclaw.auth_token = ""` — shared secret; empty means open. `IRONCLAW_TOKEN` env overrides.
- `ironclaw.module_name = "jarvis-avatar"` — identity for envelopes the avatar itself publishes

Endpoints exposed:
- `GET /ws` — WebSocket for peers
- `POST /broadcast` — HTTP fan-out (used by ironclaw-proxy when it can't hold a WS)
- `GET /health` — JSON peer roster

Peer handshake flow: connect WS → `module:authenticate { token }` → `module:announce { name }` → listen.

**Setup:** nothing extra — the hub boots with the app. Just make sure `6121` is free and that peers use a matching token.

---

## 6. MCP Streamable-HTTP Server (hosted by this app)

**What it does:** Exposes all the pose/bone/expression/animation/Kimodo/A2F tools to any MCP client (IronClaw, Claude Desktop, a custom script, etc.).

**Files:**
- `src/mcp/mod.rs` — MCP tool handlers (see `#[tool]` methods on `JarvisMcpServer`)
- `src/mcp/plugin.rs` — Bevy plugin that spawns the HTTP server
- `assets/POSE_GUIDE.md` — full authoring manual returned by `get_pose_guide`
- `docs/MCP_POSE_ANIMATION_GUIDE.md` — operator index (Kimodo paths, layering, refresh)
- Config: `[mcp]` in `config/default.toml`

**How the app knows it's "set up":**
- `mcp.enabled = true`
- `mcp.bind_address = "0.0.0.0:6123"`
- `mcp.path = "/mcp"`
- `mcp.auth_token = ""` — if set, clients must send `Authorization: Bearer <token>`

Tools exposed today (25): `list_poses`, `apply_pose`, `create_pose`, `rename_pose`, `delete_pose`, `update_pose_category`, `list_all_content`, `set_bones`, `pose_bones`, `make_fist`, `adjust_bone`, `get_current_bone_state`, `reset_pose`, `set_expression`, `get_bone_reference`, `get_pose_guide`, `list_generated_animations`, `play_saved_animation`, `delete_animation`, `rename_animation`, `update_animation_metadata`, `generate_motion`, `capture_pose_views`, `a2f_status`, `a2f_configure`.

**Cursor / IDE clients:** tool JSON schemas are often cached under `~/.cursor/projects/<id>/mcps/user-pose-controller/tools/`. After changing Rust `#[tool(description = ...)]` text, restart jarvis-avatar and refresh/re-add the MCP server so descriptors match the live server.

**Setup:** nothing external. Point your MCP client at `http://<host>:6123/mcp` (with a bearer token if you set one).

---

## 7. Pose / Animation Library (filesystem)

**What it does:** Persistent `.pose.json` and `.animation.json` files on disk that this app reads and writes (MCP, UI, and Kimodo persistence).

**Files:**
- `src/pose_library.rs` — read/write
- `src/paths.rs` — `~` home expansion
- Config: `[pose_library]`

**How the app knows it's "set up":**
- `pose_library.poses_dir` — default `~/.config/@proj-airi/stage-tamagotchi/plugins/v1/CustomPlugins/poses`
- `pose_library.animations_dir` — default `~/.config/@proj-airi/stage-tamagotchi/plugins/v1/CustomPlugins/animations`

**Setup:**
```bash
mkdir -p ~/.config/@proj-airi/stage-tamagotchi/plugins/v1/CustomPlugins/{poses,animations}
```
Not a service — just directories. If they don't exist, reads return empty; writes fail.

---

## 8. server.mjs (Home Assistant voice bridge) — peer

**What it does:** Sits outside this repo, runs as a Node service, takes voice input from Home Assistant and publishes `input:text` envelopes to the hub. Also consumes `output:gen-ai:chat:complete`, `vrm:set-look-at`, TTS messages.

**Setup:** run server.mjs with `AIRI_WS_URL=ws://<jarvis-avatar-host>:6121/ws` and `IRONCLAW_TOKEN=<same as hub>`. Services panel shows it when it authenticates.

**Fallback:** no voice input — chat via the gateway still works.

---

## 9. ironclaw-proxy (optional peer)

**What it does:** HTTP-to-hub bridge. Lets services that can't hold a WebSocket POST JSON envelopes into the hub via `POST http://<host>:6121/broadcast`.

**Setup:** nothing to configure in this app; just have the proxy POST properly-formed envelopes (`{type, data, metadata}`) to `/broadcast`.

---

## 10. Bevy VRM rendering (local, file-based)

**What it does:** Local 3D rendering via `bevy_vrm1`. Not a service, but it has file dependencies worth listing.

**Files:**
- `src/plugins/avatar.rs`
- Config: `[avatar]`

**Needs on disk:**
- `assets/models/airi.vrm` (or whatever `avatar.model_path` points to)
- `assets/models/idle_loop.vrma` (`avatar.idle_vrma_path`)

Paths are relative to the working directory (where `assets/` lives). Missing model = fail to start; missing idle VRMA = avatar loads but doesn't idle-animate.

---

## Summary table

| Service | Type | Host? | Key config | Env var |
|---|---|---|---|---|
| Kimodo | Motion LLM | peer → us | `[kimodo]` timeouts only | `IRONCLAW_TOKEN` (shared) |
| Audio2Face | gRPC | we dial | `[a2f]` endpoint + health_url | — |
| Kokoro TTS | HTTP | we dial | `[tts]` kokoro_url + voice | — |
| IronClaw Gateway | HTTP/SSE | we dial | `[gateway]` base_url | `IRONCLAW_GATEWAY_TOKEN` |
| Channel Hub | WS/HTTP | **we host** on 6121 | `[ironclaw]` bind_address | `IRONCLAW_TOKEN` |
| MCP server | HTTP | **we host** on 6123 | `[mcp]` bind_address + path | — |
| Pose library | filesystem | — | `[pose_library]` paths | — |
| server.mjs | WS client | peer → us | (external) | `IRONCLAW_TOKEN`, `AIRI_WS_URL` |
| ironclaw-proxy | HTTP client | peer → us | (external) | — |
| Bevy VRM | local | local | `[avatar]` paths | — |
