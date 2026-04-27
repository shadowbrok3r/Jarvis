#!/usr/bin/env python3
"""Kimodo AI Motion Service — generates motion from text prompts and streams VRM poses to jarvis-avatar.

Connects as a peer to the jarvis-avatar channel hub (ws://localhost:6121/ws),
announces itself as a "kimodo" module, then listens for `kimodo:generate` /
`kimodo:play-animation` envelopes. Generated motion is streamed back as
`vrm:apply-pose` frames and/or saved to the shared animations directory.

Auth can be required depending on jarvis-avatar config. This client now always
attempts `module:authenticate` first (using `IRONCLAW_TOKEN`) and proceeds even
when auth is disabled.
"""

import asyncio
import json
import sys
import time
import uuid
from pathlib import Path
from typing import Optional

import numpy as np
import torch
import websockets

# ─── Config ──────────────────────────────────────────────────────────────────
# jarvis-avatar hosts the hub on :6121 by default. Override via env if needed.
import os

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11
    tomllib = None  # type: ignore[assignment, misc]

WS_URL = os.environ.get("JARVIS_WS_URL", "ws://localhost:6121/ws")
AUTH_TOKEN = os.environ.get("IRONCLAW_TOKEN", "")

# Directory containing this script = jarvis-avatar repo root (used to find config/*.toml).
_KIMODO_REPO_ROOT = Path(__file__).resolve().parent

# Shared animations directory — must match jarvis-avatar's effective
# `[pose_library].animations_dir` or saved clips won't show up in
# `list_generated_animations`. Precedence: JARVIS_ANIMATIONS_DIR, then
# config/user.toml / config/default.toml (same path rules as the Rust app:
# `~/` expanded; other relative paths use the *process cwd*), then the
# legacy XDG default path.
def _read_animations_dir_from_toml_nolib(p: Path) -> Optional[str]:
    """No-deps parse of [pose_library].animations_dir from a single TOML file (Python < 3.11)."""
    try:
        in_pose = False
        for line in p.read_text(encoding="utf-8").splitlines():
            body = line.split("#", 1)[0].strip()
            if body == "[pose_library]":
                in_pose = True
                continue
            if in_pose and body.startswith("["):
                in_pose = False
            if in_pose and body.startswith("animations_dir"):
                parts = body.split("=", 1)
                if len(parts) < 2:
                    continue
                val = parts[1].strip().strip('"').strip("'")
                if val:
                    return val
    except OSError:
        pass
    return None


def _read_animations_dir_from_toml() -> Optional[str]:
    for name in ("user.toml", "default.toml"):
        p = _KIMODO_REPO_ROOT / "config" / name
        if not p.is_file():
            continue
        if tomllib is not None:
            try:
                data = tomllib.loads(p.read_text(encoding="utf-8"))
            except Exception:
                continue
            pl = data.get("pose_library")
            if not isinstance(pl, dict):
                continue
            val = (pl.get("animations_dir") or "").strip()
            if val:
                return val
        else:
            val = _read_animations_dir_from_toml_nolib(p)
            if val:
                return val
    return None


def _absolute_like_jarvis_avatar(raw: str) -> Path:
    """Match `paths::expand_home` + relative cwd semantics in pose_library."""
    raw = raw.strip()
    p = Path(os.path.expanduser(raw))
    if p.is_absolute():
        return p.resolve()
    return (Path.cwd() / p).resolve()


def _resolve_animations_dir() -> Path:
    env = (os.environ.get("JARVIS_ANIMATIONS_DIR") or "").strip()
    if env:
        return _absolute_like_jarvis_avatar(env)
    toml_path = _read_animations_dir_from_toml()
    if toml_path:
        return _absolute_like_jarvis_avatar(toml_path)
    return (
        Path.home()
        / ".config/@proj-airi/stage-tamagotchi/plugins/v1/CustomPlugins/animations"
    ).resolve()


ANIMATIONS_DIR = _resolve_animations_dir()
if not (os.environ.get("KIMODO_QUIET") or "").strip():
    print(f"[kimodo-motion-service] ANIMATIONS_DIR={ANIMATIONS_DIR}", file=sys.stderr)

# Module name MUST contain the substring "kimodo" so jarvis-avatar's Services
# panel classifies us as the Kimodo peer (service_status.rs:338).
SERVICE_ID = "kimodo-motion-service"
MODEL_FPS = 30.0


# ─── SOMA77 → VRM bone mapping ───────────────────────────────────────────────
# Index in SOMASkeleton77.bone_order_names_with_parents → VRM humanoid bone name.
# Joints without a VRM equivalent (HeadEnd, Jaw, eyes, finger *End joints, ToeEnd) are skipped.

SOMA77_BONE_ORDER = [
    "Hips", "Spine1", "Spine2", "Chest",
    "Neck1", "Neck2", "Head", "HeadEnd", "Jaw", "LeftEye", "RightEye",
    "LeftShoulder", "LeftArm", "LeftForeArm", "LeftHand",
    "LeftHandThumb1", "LeftHandThumb2", "LeftHandThumb3", "LeftHandThumbEnd",
    "LeftHandIndex1", "LeftHandIndex2", "LeftHandIndex3", "LeftHandIndex4", "LeftHandIndexEnd",
    "LeftHandMiddle1", "LeftHandMiddle2", "LeftHandMiddle3", "LeftHandMiddle4", "LeftHandMiddleEnd",
    "LeftHandRing1", "LeftHandRing2", "LeftHandRing3", "LeftHandRing4", "LeftHandRingEnd",
    "LeftHandPinky1", "LeftHandPinky2", "LeftHandPinky3", "LeftHandPinky4", "LeftHandPinkyEnd",
    "RightShoulder", "RightArm", "RightForeArm", "RightHand",
    "RightHandThumb1", "RightHandThumb2", "RightHandThumb3", "RightHandThumbEnd",
    "RightHandIndex1", "RightHandIndex2", "RightHandIndex3", "RightHandIndex4", "RightHandIndexEnd",
    "RightHandMiddle1", "RightHandMiddle2", "RightHandMiddle3", "RightHandMiddle4", "RightHandMiddleEnd",
    "RightHandRing1", "RightHandRing2", "RightHandRing3", "RightHandRing4", "RightHandRingEnd",
    "RightHandPinky1", "RightHandPinky2", "RightHandPinky3", "RightHandPinky4", "RightHandPinkyEnd",
    "LeftLeg", "LeftShin", "LeftFoot", "LeftToeBase", "LeftToeEnd",
    "RightLeg", "RightShin", "RightFoot", "RightToeBase", "RightToeEnd",
]

SOMA77_TO_VRM = {
    "Hips": "hips",
    "Spine1": "spine",
    "Spine2": "chest",
    "Chest": "upperChest",
    "Neck1": "neck",
    "Head": "head",
    "LeftShoulder": "leftShoulder",
    "LeftArm": "leftUpperArm",
    "LeftForeArm": "leftLowerArm",
    "LeftHand": "leftHand",
    "RightShoulder": "rightShoulder",
    "RightArm": "rightUpperArm",
    "RightForeArm": "rightLowerArm",
    "RightHand": "rightHand",
    "LeftLeg": "leftUpperLeg",
    "LeftShin": "leftLowerLeg",
    "LeftFoot": "leftFoot",
    "LeftToeBase": "leftToes",
    "RightLeg": "rightUpperLeg",
    "RightShin": "rightLowerLeg",
    "RightFoot": "rightFoot",
    "RightToeBase": "rightToes",
    # Fingers: SOMA has 4 joints per finger (1-4) + End. VRM has Metacarpal/Proximal/Intermediate/Distal.
    # Thumb: SOMA 1=Metacarpal, 2=Proximal, 3=Distal
    "LeftHandThumb1": "leftThumbMetacarpal",
    "LeftHandThumb2": "leftThumbProximal",
    "LeftHandThumb3": "leftThumbDistal",
    "RightHandThumb1": "rightThumbMetacarpal",
    "RightHandThumb2": "rightThumbProximal",
    "RightHandThumb3": "rightThumbDistal",
    # Index: SOMA 1=Proximal, 2=Intermediate, 3=Distal, 4 skipped (VRM has no 4th)
    "LeftHandIndex1": "leftIndexProximal",
    "LeftHandIndex2": "leftIndexIntermediate",
    "LeftHandIndex3": "leftIndexDistal",
    "RightHandIndex1": "rightIndexProximal",
    "RightHandIndex2": "rightIndexIntermediate",
    "RightHandIndex3": "rightIndexDistal",
    # Middle
    "LeftHandMiddle1": "leftMiddleProximal",
    "LeftHandMiddle2": "leftMiddleIntermediate",
    "LeftHandMiddle3": "leftMiddleDistal",
    "RightHandMiddle1": "rightMiddleProximal",
    "RightHandMiddle2": "rightMiddleIntermediate",
    "RightHandMiddle3": "rightMiddleDistal",
    # Ring
    "LeftHandRing1": "leftRingProximal",
    "LeftHandRing2": "leftRingIntermediate",
    "LeftHandRing3": "leftRingDistal",
    "RightHandRing1": "rightRingProximal",
    "RightHandRing2": "rightRingIntermediate",
    "RightHandRing3": "rightRingDistal",
    # Pinky → Little
    "LeftHandPinky1": "leftLittleProximal",
    "LeftHandPinky2": "leftLittleIntermediate",
    "LeftHandPinky3": "leftLittleDistal",
    "RightHandPinky1": "rightLittleProximal",
    "RightHandPinky2": "rightLittleIntermediate",
    "RightHandPinky3": "rightLittleDistal",
}

# Pre-build index lookup: SOMA77 joint index → VRM bone name (or None to skip)
SOMA77_INDEX_TO_VRM = []
for bone_name in SOMA77_BONE_ORDER:
    SOMA77_INDEX_TO_VRM.append(SOMA77_TO_VRM.get(bone_name))


def rotation_matrix_to_quaternion(mat: np.ndarray) -> tuple:
    """Convert a 3x3 rotation matrix to quaternion [x, y, z, w]."""
    m = mat
    trace = m[0, 0] + m[1, 1] + m[2, 2]

    if trace > 0:
        s = 0.5 / np.sqrt(trace + 1.0)
        w = 0.25 / s
        x = (m[2, 1] - m[1, 2]) * s
        y = (m[0, 2] - m[2, 0]) * s
        z = (m[1, 0] - m[0, 1]) * s
    elif m[0, 0] > m[1, 1] and m[0, 0] > m[2, 2]:
        s = 2.0 * np.sqrt(1.0 + m[0, 0] - m[1, 1] - m[2, 2])
        w = (m[2, 1] - m[1, 2]) / s
        x = 0.25 * s
        y = (m[0, 1] + m[1, 0]) / s
        z = (m[0, 2] + m[2, 0]) / s
    elif m[1, 1] > m[2, 2]:
        s = 2.0 * np.sqrt(1.0 + m[1, 1] - m[0, 0] - m[2, 2])
        w = (m[0, 2] - m[2, 0]) / s
        x = (m[0, 1] + m[1, 0]) / s
        y = 0.25 * s
        z = (m[1, 2] + m[2, 1]) / s
    else:
        s = 2.0 * np.sqrt(1.0 + m[2, 2] - m[0, 0] - m[1, 1])
        w = (m[1, 0] - m[0, 1]) / s
        x = (m[0, 2] + m[2, 0]) / s
        y = (m[1, 2] + m[2, 1]) / s
        z = 0.25 * s

    length = np.sqrt(x * x + y * y + z * z + w * w)
    if length > 0:
        x, y, z, w = x / length, y / length, z / length, w / length

    return (float(x), float(y), float(z), float(w))


def convert_frame(local_rot_mats_frame: np.ndarray) -> dict:
    """Convert one frame of (77, 3, 3) local rotation matrices to VRM bone quaternions."""
    bones = {}
    for joint_idx, vrm_name in enumerate(SOMA77_INDEX_TO_VRM):
        if vrm_name is None:
            continue
        if joint_idx >= local_rot_mats_frame.shape[0]:
            break
        mat = local_rot_mats_frame[joint_idx]
        # Skip identity rotations (no meaningful pose change)
        if np.allclose(mat, np.eye(3), atol=0.01):
            continue
        qx, qy, qz, qw = rotation_matrix_to_quaternion(mat)
        bones[vrm_name] = {"rotation": [qx, qy, qz, qw]}
    return bones


def convert_motion(local_rot_mats: np.ndarray) -> list:
    """Convert (T, J, 3, 3) motion data to list of VRM frame dicts."""
    frames = []
    for t in range(local_rot_mats.shape[0]):
        bones = convert_frame(local_rot_mats[t])
        frames.append(bones)
    return frames


# ─── WebSocket messaging ─────────────────────────────────────────────────────

def make_message(msg_type: str, data: dict) -> str:
    """Build a raw IronClaw envelope. The hub emits and accepts this shape directly
    (channel_server.rs:578-586 / 430). We dropped the legacy {json, meta} superjson
    wrapper since jarvis-avatar's hub sends raw envelopes on the wire."""
    return json.dumps({
        "type": msg_type,
        "data": data,
        "metadata": {
            "event": {"id": str(uuid.uuid4())},
            "source": {"kind": "service", "id": SERVICE_ID},
        },
    })


# ─── Model loading ───────────────────────────────────────────────────────────

model = None
model_name = None


def load_kimodo():
    global model, model_name
    from kimodo import load_model
    log("Loading Kimodo model (kimodo-soma-rp)...")
    model = load_model("kimodo-soma-rp", device="cuda" if torch.cuda.is_available() else "cpu")
    model_name = "kimodo-soma-rp"
    log(f"Model loaded. FPS={model.fps}, device={'cuda' if torch.cuda.is_available() else 'cpu'}")


def log(msg: str):
    sys.stderr.write(f"[{SERVICE_ID}] {msg}\n")
    sys.stderr.flush()


# ─── Generation ──────────────────────────────────────────────────────────────

def generate_motion(prompt: str, duration: float, steps: int = 100) -> tuple:
    """Generate motion and return (vrm_frames, fps)."""
    if model is None:
        raise RuntimeError("Model not loaded")

    num_frames = int(duration * model.fps)
    num_frames = max(30, min(num_frames, 600))

    log(f"Generating: prompt='{prompt}', frames={num_frames}, steps={steps}")
    t0 = time.time()

    output = model(
        [prompt + "."],
        [num_frames],
        num_denoising_steps=steps,
        num_samples=1,
        multi_prompt=True,
        post_processing=True,
        return_numpy=True,
    )

    elapsed = time.time() - t0
    log(f"Generation done in {elapsed:.1f}s")

    local_rot_mats = output["local_rot_mats"]
    if local_rot_mats.ndim == 5:
        local_rot_mats = local_rot_mats[0]  # Remove batch dim

    if isinstance(local_rot_mats, torch.Tensor):
        local_rot_mats = local_rot_mats.cpu().numpy()

    vrm_frames = convert_motion(local_rot_mats)
    return vrm_frames, float(model.fps)


def _slugify_animation_stem(name: str) -> str:
    """Match Rust / Node: [^a-z0-9_-] → underscore, then lowercase (ASCII alnum)."""
    out: list[str] = []
    for c in name.strip():
        if c.isascii() and (c.isalnum() or c in "-_"):
            out.append(c.lower())
        else:
            out.append("_")
    s = "".join(out).strip("_")
    return s or "unnamed"


def save_animation(name: str, prompt: str, fps: float, vrm_frames: list):
    """Save generated animation to disk."""
    ANIMATIONS_DIR.mkdir(parents=True, exist_ok=True)
    filename = _slugify_animation_stem(name) + ".json"
    frame_duration_ms = 1000.0 / fps

    animation_data = {
        "name": name,
        "prompt": prompt,
        "fps": fps,
        "frameCount": len(vrm_frames),
        "frames": [
            {"bones": frame, "duration_ms": frame_duration_ms}
            for frame in vrm_frames
        ],
    }

    path = ANIMATIONS_DIR / filename
    path.write_text(json.dumps(animation_data, indent=2))
    log(f"Saved animation: {path}")
    return str(path)


def list_animations() -> list:
    """List saved animations."""
    if not ANIMATIONS_DIR.exists():
        return []
    result = []
    for f in sorted(ANIMATIONS_DIR.glob("*.json")):
        try:
            data = json.loads(f.read_text())
            result.append({
                "name": data.get("name", f.stem),
                "prompt": data.get("prompt", ""),
                "fps": data.get("fps", 30),
                "frameCount": data.get("frameCount", 0),
                "filename": f.name,
            })
        except Exception:
            pass
    return result


def load_animation(filename: str) -> dict | None:
    """Load a saved animation by filename."""
    path = ANIMATIONS_DIR / filename
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text())
    except Exception:
        return None


# ─── Main WebSocket loop ─────────────────────────────────────────────────────

async def ws_main():
    load_kimodo()

    while True:
        try:
            log(f"Connecting to {WS_URL}...")
            async with websockets.connect(
                WS_URL,
                ping_interval=None,
                ping_timeout=None,
                close_timeout=5,
            ) as ws:
                log("Connected — authenticating/announcing module...")

                # Always attempt auth. Hubs without auth enabled still answer
                # `module:authenticated`, so this works in either mode.
                await ws.send(make_message("module:authenticate", {
                    "token": AUTH_TOKEN,
                }))

                authed = False
                while True:
                    raw = await ws.recv()
                    try:
                        auth_msg = json.loads(raw)
                    except Exception:
                        continue
                    if "json" in auth_msg and isinstance(auth_msg["json"], dict):
                        auth_msg = auth_msg["json"]
                    auth_type = auth_msg.get("type", "")
                    if auth_type == "module:authenticated":
                        authed = True
                        break
                    if auth_type == "error":
                        code = auth_msg.get("data", {}).get("code")
                        detail = auth_msg.get("data", {}).get("message", "unknown error")
                        raise RuntimeError(f"auth failed: code={code} detail={detail}")
                if not authed:
                    raise RuntimeError("auth failed: no module:authenticated response")

                await ws.send(make_message("module:announce", {
                    "name": SERVICE_ID,
                    "identity": {
                        "kind": "service",
                        "id": SERVICE_ID,
                        "version": "1.1.0",
                        "capabilities": ["kimodo:generate", "kimodo:play-animation",
                                         "kimodo:list-animations", "kimodo:load-animation"],
                    },
                }))
                log(f"Announced as '{SERVICE_ID}' — listening for events")

                async for raw in ws:
                    try:
                        msg = json.loads(raw)
                    except Exception:
                        continue

                    # Hub sends raw envelopes, but accept the legacy superjson
                    # wrapper too in case something upstream still emits it.
                    if "json" in msg and isinstance(msg["json"], dict):
                        msg = msg["json"]

                    msg_type = msg.get("type", "")

                    if msg_type == "transport:connection:heartbeat":
                        ping = msg.get("data", {}).get("ping")
                        if ping:
                            await ws.send(make_message("transport:connection:heartbeat", {"pong": ping}))
                        continue

                    if msg_type.startswith("kimodo:"):
                        log(f"Received: {msg_type}")

                    if msg_type == "kimodo:generate":
                        asyncio.create_task(handle_generate(ws, msg))
                    elif msg_type == "kimodo:list-animations":
                        await handle_list_animations(ws, msg)
                    elif msg_type == "kimodo:load-animation":
                        await handle_load_animation(ws, msg)
                    elif msg_type == "kimodo:play-animation":
                        asyncio.create_task(handle_play_animation(ws, msg))

        except (websockets.ConnectionClosed, ConnectionRefusedError, OSError) as e:
            log(f"Connection lost ({e}), reconnecting in 3s...")
            await asyncio.sleep(3)
        except Exception as e:
            log(f"Unexpected error: {e}, reconnecting in 5s...")
            await asyncio.sleep(5)


async def handle_generate(ws, msg):
    """Handle a kimodo:generate request — generate motion and optionally stream/save."""
    data = msg.get("data", {})
    prompt = data.get("prompt", "A person stands still")
    duration = data.get("duration", 3.0)
    steps = data.get("steps", 100)
    stream = data.get("stream", True)
    save_name = data.get("saveName")
    request_id = msg.get("metadata", {}).get("event", {}).get("id", str(uuid.uuid4()))

    try:
        await ws.send(make_message("kimodo:status", {
            "requestId": request_id,
            "status": "generating",
            "message": f"Generating motion for: {prompt} ({duration}s, {steps} steps)...",
        }))

        vrm_frames, fps = await asyncio.get_event_loop().run_in_executor(
            None, generate_motion, prompt, duration, steps
        )

        save_path = None
        if save_name:
            save_path = await asyncio.get_event_loop().run_in_executor(
                None, save_animation, save_name, prompt, fps, vrm_frames
            )

        ready_data = {
            "requestId": request_id,
            "status": "ready",
            "message": f"Generated {len(vrm_frames)} frames at {fps} FPS",
            "frameCount": len(vrm_frames),
            "fps": fps,
        }
        if save_path:
            ready_data["savedPath"] = save_path
        await ws.send(make_message("kimodo:status", ready_data))

        if stream:
            await stream_frames(ws, vrm_frames, fps, request_id, save_path)
        else:
            await ws.send(make_message("kimodo:generate:result", {
                "requestId": request_id,
                "prompt": prompt,
                "fps": fps,
                "frameCount": len(vrm_frames),
                "frames": [{"bones": f} for f in vrm_frames],
                "savedPath": save_path,
            }))

    except Exception as e:
        log(f"Generation error: {e}")
        await ws.send(make_message("kimodo:status", {
            "requestId": request_id,
            "status": "error",
            "message": str(e),
        }))


async def stream_frames(
    ws, vrm_frames: list, fps: float, request_id: str, save_path: str | None = None
):
    """Stream VRM frames as vrm:apply-pose events at the target FPS."""
    frame_interval = 1.0 / fps
    transition_duration = frame_interval * 1.1  # Slightly longer than interval for overlap smoothing

    log(f"Streaming {len(vrm_frames)} frames at {fps} FPS...")
    await ws.send(make_message("kimodo:status", {
        "requestId": request_id,
        "status": "streaming",
        "message": f"Playing {len(vrm_frames)} frames...",
    }))

    t0 = time.time()
    for i, bones in enumerate(vrm_frames):
        target_time = t0 + i * frame_interval
        now = time.time()
        if target_time > now:
            await asyncio.sleep(target_time - now)

        await ws.send(make_message("vrm:apply-pose", {
            "bones": bones,
            "transitionDuration": transition_duration,
            "blendWeight": 1.0,
            "requestId": request_id,
        }))

    done_data = {
        "requestId": request_id,
        "status": "done",
        "message": f"Playback complete ({len(vrm_frames)} frames)",
    }
    if save_path:
        done_data["savedPath"] = save_path
    await ws.send(make_message("kimodo:status", done_data))


async def handle_list_animations(ws, msg):
    anims = list_animations()
    await ws.send(make_message("kimodo:list-animations:result", {"animations": anims}))


async def handle_load_animation(ws, msg):
    filename = msg.get("data", {}).get("filename", "")
    anim = load_animation(filename)
    if anim:
        await ws.send(make_message("kimodo:load-animation:result", anim))
    else:
        await ws.send(make_message("kimodo:load-animation:result", {
            "error": f"Animation '{filename}' not found",
        }))


async def handle_play_animation(ws, msg):
    """Load and stream a saved animation."""
    data = msg.get("data", {})
    filename = data.get("filename", "")
    request_id = msg.get("metadata", {}).get("event", {}).get("id", str(uuid.uuid4()))

    anim = load_animation(filename)
    if not anim or "frames" not in anim:
        await ws.send(make_message("kimodo:status", {
            "requestId": request_id,
            "status": "error",
            "message": f"Animation '{filename}' not found or invalid",
        }))
        return

    fps = anim.get("fps", 30)
    vrm_frames = [f["bones"] for f in anim["frames"]]
    await stream_frames(ws, vrm_frames, fps, request_id)


if __name__ == "__main__":
    log("Starting Kimodo Motion Service...")
    asyncio.run(ws_main())
