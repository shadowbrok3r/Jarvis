# jarvis-avatar

Rust + Bevy VRM viewer that **hosts the IronClaw-style WebSocket hub** on port 6121.
It can stand in for AIRI’s Tamagotchi UI and AIRI’s `server-runtime` hub; IronClaw
itself stays the product backend. `server.mjs` (ha-voice-bridge) is unchanged on the
wire — point it at this process instead of an AIRI hub.

## Architecture at a glance

```
server.mjs (ws client)  ─┐
ironclaw-proxy (http/ws) ─┼──▶  jarvis-avatar  (axum :6121 hub + :6123 MCP)
curl / tests (http)       ─┘       ├─ /ws           ← IronClaw-protocol WS hub
                                   ├─ /broadcast    ← HTTP POST fan-out
                                   ├─ /health       ← peer roster
                                   └─ /mcp          ← streamable HTTP (RMCP tools)
                                        │
                                        ▼ (crossbeam)
                                   Bevy ECS
                                   ├─ VRM renderer (bevy_vrm1 0.7)
                                   ├─ Expressions (ACT tokens → SetExpressions)
                                   ├─ LookAt (vrm:set-look-at → LookAt::Target)
                                   ├─ TTS (Kokoro WAV → AudioPlayer)
                                   └─ Animation layers + native clip / idle drivers
```

Crate versions vs NewPlan's original pins: NewPlan suggested Bevy `0.16` /
`bevy_vrm1` `0.2`. Reality is **Bevy 0.18 + bevy_vrm1 0.7**; this repo follows the
ecosystem so `cargo build` resolves cleanly.

## Run

```bash
cd /path/to/this/repo                    # directory that contains Cargo.toml
cp .env.example .env                     # optional IRONCLAW_TOKEN
# Under assets/: models/airi.vrm (VRM) + models/idle_loop.vrma (idle VRMA)
cargo run
```

`config/default.toml` is the source of truth for every tunable. The egui menu bar
is always on; use **View** to open windows (Chat, Services, Animation Layers, and so
on) and **File** to save or reload settings into `config/user.toml`.

**Camera:** left-drag orbit · middle-drag pan · scroll zoom. Tune
`[camera]`/`[graphics]` for MSAA, HDR, exposure, lights, ground plane.

If `rustc` fails with `unknown proxy name: 'Cursor-...appimage'`, your
Cursor shim is in front of the real rustup on `PATH`; either fix
`~/.cargo/config.toml` / rustup shims so the real toolchain runs, or invoke
`~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo` directly.

## Channel hub protocol (same as AIRI)

Raw frames both directions:

```json
{ "type": "...",
  "data": { ... },
  "metadata": {
    "event":  { "id": "uuid" },
    "source": { "kind": "module", "id": "peer-name" }
  } }
```

The hub also accepts the superjson-wrapped form (`{"json": {...}, "meta": {}}`)
for parity with AIRI clients; replies are always raw.

Connect flow:

1. client opens `ws://<host>:6121/ws`
2. client → `module:authenticate { token }`
3. hub → `module:authenticated` (or `error {code:"invalid_token"}`)
4. client → `module:announce { name, identity }`
5. hub fans `module:announced` out to the other authenticated peers
6. any other envelope is fanned to every **other** authenticated peer **and**
  consumed by Bevy (`output:gen-ai:chat:complete` drives expressions + TTS,
   `vrm:set-look-at` drives gaze, etc.)

Heartbeat: hub responds to WebSocket `Ping` with `Pong`, and to
`transport:connection:heartbeat { kind: "ping" }` envelopes with the `pong`
counterpart.

### HTTP side-channel

```bash
# Broadcast a chat completion from ironclaw-proxy (or anything) without opening a WS:
curl -X POST http://localhost:6121/broadcast \
     -H 'Content-Type: application/json' \
     -d '{"type":"output:gen-ai:chat:complete",
          "data":{"message":{"role":"assistant",
                             "content":"<|ACT:{\"emotion\":\"happy\"}|>hi there."}}}'

# Peer roster:
curl -s http://localhost:6121/health | jq

# JarvisIOS profile manifest (optional Bearer if IRONCLAW_TOKEN / [ironclaw].auth_token is set):
curl -sS -H "Authorization: Bearer $IRONCLAW_TOKEN" http://localhost:6121/jarvis-ios/v1/manifest | jq .
```

See **`JarvisIOS/README.md`** (`Desktop hub: profile sync over HTTP`) for asset and spring-preset paths.

## `server.mjs` stays unchanged

`AIRI_WS_URL=ws://localhost:6121/ws` in `ha-voice-bridge`'s env; the wire
protocol it speaks is the same one jarvis-avatar now hosts. Set
`IRONCLAW_TOKEN` on both sides if you want auth.

## Gateway chat images

The desktop chat window decodes **markdown / data-URL images** and **`image_generated`** gateway SSE events into inline thumbnails. JarvisIOS parses the same SSE shape and strips embedded data URLs from assistant text.

ComfyUI from coding agents is handled outside this repo (e.g. [comfyui-mcp](https://github.com/artokun/comfyui-mcp) over SSH stdio to the GPU host). See `examples/cursor-mcp-comfyui.json` for a Cursor MCP snippet (uses `shadowbroker@100.102.254.81` — change user/IP to match your Tailscale node and ensure that machine’s SSH key is in `authorized_keys` on the Comfy host). Alternatively use an `~/.ssh/config` `Host` alias and put that host name in `args` instead of the raw address.

**ComfyUI Sentinel:** apply `patches/comfyui-mcp-sentinel-auth.patch` in a clone of artokun/comfyui-mcp (`git apply …`), or use the pre-built tree under `vendor/comfyui-mcp` after `npm install && npm run build`. Then set `COMFYUI_USERNAME` / `COMFYUI_PASSWORD` and/or `COMFYUI_TOKEN` in the MCP `env` (Ironclaw/Cursor) so HTTP and WebSocket calls carry a JWT.

## Debug UI

**View → Services (all)** (or the individual hub / gateway / TTS / MCP windows)
shows connection status, bind addresses, and health where applicable.

**Test → Open Live / Test bench** opens the hub test window: peer counts,
`input:text` broadcast to every peer, synthetic `ChatCompleteMessage` per emotion,
rig-local look-at plus reset to cursor, and a Kokoro TTS round-trip control.