#!/usr/bin/env python3
"""Convert VRMA 1.0 (glTF + VRMC_vrm_animation humanoid) clips to jarvis / Kimodo animation JSON.

Output shape matches `AnimationFile` / `AnimationFrame` in `src/pose_library.rs`
(name, prompt, fps, frameCount, frames[{ bones, duration_ms? }])).

Dependencies: Python 3.10+ stdlib only.

Coordinate-space note (important — this is the difference between a working clip and a "lying on the
floor" clip): VRMA channel rotations are **absolute** glTF node rotations in the VRMA file's own bind
frame; they REPLACE `node.rotation` per the glTF spec, not multiply it. Many VRMA exporters bake
non-identity bind rotations into the humanoid nodes (the "VRMA_07_Squat.vrma" pack has hips at a 120°
(1,1,1) cyclic permutation and `*UpperLeg` at 180° flips). To produce the **normalized humanoid pose
quaternion** that the loaded VRM expects (which is the "delta from bind"), each channel value must be
rebased by `bind_rotation.inverse() * channel_value`. Without this rebase the avatar lies horizontal at
frame 0 because the bind rotation gets re-applied as if it were a pose. The jarvis engine's
`local_from_normalized` then maps the normalized delta into the loaded rig's raw-local space.

Limitations:
- Reads **humanoid rotation** channels only (same bone names as VRM humanoid: hips, leftUpperArm, …).
- **Root translation** (e.g. hips translation) is ignored — jarvis pose JSON is rotation-only; vertical drift
  should be handled in-app (`lock_root_*`, world position) like other body clips.
- **VRMC expression** tracks are not present in typical packs; this script does not invent expression weights.
- Assumes one animation per file and **aligned** input time arrays across channels (true for vrm-studio samples).
"""

from __future__ import annotations

import argparse
import json
import struct
import sys
from pathlib import Path
from typing import Any


def _slugify_animation_stem(name: str) -> str:
    """Match Rust `pose_library::slugify` / Kimodo `_slugify_animation_stem`."""
    out: list[str] = []
    for c in name.strip():
        if c.isascii() and (c.isalnum() or c in "-_"):
            out.append(c.lower())
        else:
            out.append("_")
    s = "".join(out).strip("_")
    return s or "unnamed"


def _read_glb_chunks(path: Path) -> tuple[dict[str, Any], bytes]:
    data = path.read_bytes()
    if len(data) < 12 or data[:4] != b"glTF":
        raise ValueError(f"not a GLB: {path}")
    off = 12
    json_bytes = b""
    bin_bytes = b""
    while off + 8 <= len(data):
        clen, ctype = struct.unpack_from("<I4s", data, off)
        off += 8
        chunk = data[off : off + clen]
        off += clen
        while off % 4 and off < len(data):
            off += 1
        if ctype == b"JSON":
            json_bytes = chunk
        elif ctype == b"BIN\x00":
            bin_bytes = chunk
        if off >= len(data):
            break
    if not json_bytes:
        raise ValueError(f"missing JSON chunk: {path}")
    return json.loads(json_bytes.decode()), bin_bytes


def _component_unpack(ctype: int, comps: int) -> str:
    if ctype == 5126:  # FLOAT
        return "<" + "f" * comps
    if ctype == 5123:  # UNSIGNED_SHORT
        return "<" + "H" * comps
    if ctype == 5121:  # UNSIGNED_BYTE
        return "<" + "B" * comps
    if ctype == 5125:  # UNSIGNED_INT
        return "<" + "I" * comps
    raise ValueError(f"unsupported componentType {ctype}")


def _read_accessor(gltf: dict, bin_chunk: bytes, accessor_idx: int) -> list:
    acc = gltf["accessors"][accessor_idx]
    bv_idx = acc["bufferView"]
    bv = gltf["bufferViews"][bv_idx]
    start = bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
    ctype = acc["componentType"]
    count = acc["count"]
    atype = acc["type"]
    comps = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}[atype]
    fmt = _component_unpack(ctype, comps)
    el_size = struct.calcsize(fmt)
    stride = bv.get("byteStride", el_size)
    out: list = []
    for i in range(count):
        o = start + i * stride
        row = struct.unpack_from(fmt, bin_chunk, o)
        out.append(row[0] if comps == 1 else row)
    return out


def _times_close(a: list[float], b: list[float], tol: float = 1e-4) -> bool:
    if len(a) != len(b):
        return False
    return all(abs(x - y) <= tol for x, y in zip(a, b))


def _quat_identity(q: tuple[float, ...], eps: float = 1e-3) -> bool:
    x, y, z, w = q
    # Treat ± identity as identity
    d0 = x * x + y * y + z * z + (w - 1.0) * (w - 1.0)
    d1 = x * x + y * y + z * z + (w + 1.0) * (w + 1.0)
    return min(d0, d1) <= eps * eps


def _quat_inverse(q: tuple[float, float, float, float]) -> tuple[float, float, float, float]:
    """Conjugate / inverse for unit quaternions (xyzw). Falls back to true inverse if non-unit."""
    x, y, z, w = q
    n2 = x * x + y * y + z * z + w * w
    if n2 == 0.0:
        return (0.0, 0.0, 0.0, 1.0)
    return (-x / n2, -y / n2, -z / n2, w / n2)


def _quat_mul(
    a: tuple[float, float, float, float],
    b: tuple[float, float, float, float],
) -> tuple[float, float, float, float]:
    """Hamilton product (xyzw)."""
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return (
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    )


def convert_vrma_to_animation_dict(
    path: Path,
    *,
    skip_identity: bool = True,
    identity_eps: float = 1e-3,
) -> dict[str, Any]:
    gltf, buf = _read_glb_chunks(path)
    ext_root = gltf.get("extensions") or {}
    vrma = ext_root.get("VRMC_vrm_animation")
    if not isinstance(vrma, dict):
        raise ValueError(f"missing VRMC_vrm_animation extension: {path}")
    humanoid = vrma.get("humanoid") or {}
    human_bones = humanoid.get("humanBones")
    if not isinstance(human_bones, dict):
        raise ValueError(f"missing humanBones: {path}")

    anims = gltf.get("animations") or []
    if not anims:
        raise ValueError(f"no animations[] in glTF: {path}")
    anim0 = anims[0]

    # Capture each humanoid node's bind rotation so we can rebase channel values
    # to the standardized normalized humanoid frame (`bind.inverse() * channel`).
    # See module docstring for why this is required — without it the avatar lies
    # horizontal at frame 0 because the bind rotation gets re-applied as a pose.
    bind_by_node: dict[int, tuple[float, float, float, float]] = {}
    for spec in human_bones.values():
        try:
            ni = int(spec["node"])
        except (KeyError, TypeError, ValueError):
            continue
        node = gltf["nodes"][ni]
        rest = node.get("rotation")
        if rest is None or len(rest) != 4:
            bind_by_node[ni] = (0.0, 0.0, 0.0, 1.0)
        else:
            bind_by_node[ni] = (
                float(rest[0]),
                float(rest[1]),
                float(rest[2]),
                float(rest[3]),
            )

    rot_by_node: dict[int, tuple[list[float], list[tuple]]] = {}
    master_times: list[float] | None = None

    for ch in anim0.get("channels", []):
        tgt = ch.get("target") or {}
        node = tgt.get("node")
        path_kind = tgt.get("path")
        if path_kind != "rotation" or node is None:
            continue
        samp = anim0["samplers"][ch["sampler"]]
        times = _read_accessor(gltf, buf, samp["input"])
        rots = _read_accessor(gltf, buf, samp["output"])
        if not isinstance(times[0], (int, float)):
            times = [float(t) for t in times]
        else:
            times = [float(t) for t in times]
        rot_by_node[int(node)] = (times, rots)
        if master_times is None:
            master_times = times

    if master_times is None or not rot_by_node:
        raise ValueError(f"no rotation channels found: {path}")

    for ni, (t, _) in rot_by_node.items():
        if not _times_close(t, master_times):
            raise ValueError(
                f"mismatched input times on node {ni} vs master — "
                f"resample not implemented ({path})"
            )

    n = len(master_times)
    frames_out: list[dict] = []
    for i in range(n):
        bones: dict[str, dict] = {}
        for bone_name, spec in human_bones.items():
            ni = int(spec["node"])
            if ni not in rot_by_node:
                continue
            _, rots = rot_by_node[ni]
            channel_q = tuple(float(x) for x in rots[i])
            if len(channel_q) != 4:
                continue
            # Rebase: VRMA channel values are absolute glTF node rotations; the
            # loaded VRM expects the normalized humanoid delta from bind.
            bind_q = bind_by_node.get(ni, (0.0, 0.0, 0.0, 1.0))
            q = _quat_mul(_quat_inverse(bind_q), channel_q)
            if skip_identity and _quat_identity(q, identity_eps):
                continue
            bones[str(bone_name)] = {"rotation": [q[0], q[1], q[2], q[3]]}

        if i + 1 < n:
            duration_ms = (master_times[i + 1] - master_times[i]) * 1000.0
        else:
            duration_ms = (
                (master_times[-1] - master_times[-2]) * 1000.0 if n > 1 else 1000.0 / 30.0
            )

        # Match `AnimationFrame` serde camelCase (`durationMs`); `duration_ms` also accepted by jarvis.
        frames_out.append({"bones": bones, "durationMs": duration_ms})

    span = master_times[-1] - master_times[0]
    if n > 1 and span > 1e-8:
        fps = (n - 1) / span
    else:
        fps = 30.0

    stem = path.stem
    return {
        "name": stem,
        "prompt": f"imported from VRMA: {path.name}",
        "fps": round(fps, 6),
        "frameCount": n,
        "frames": frames_out,
        "category": "vrma_import",
        "looping": False,
        "holdDuration": 0.0,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "inputs",
        nargs="*",
        help="VRMA files or directories (recursive .vrma)",
    )
    ap.add_argument(
        "-o",
        "--out-dir",
        type=Path,
        default=Path("assets/animations/imported_vrma"),
        help="Output directory for .json (created if missing)",
    )
    ap.add_argument(
        "--no-skip-identity",
        action="store_true",
        help="Emit identity rotations for every bone each frame (large files)",
    )
    args = ap.parse_args()
    paths: list[Path] = []
    for raw in args.inputs:
        p = Path(raw).expanduser().resolve()
        if p.is_dir():
            paths.extend(sorted(p.rglob("*.vrma")))
        elif p.suffix.lower() == ".vrma":
            paths.append(p)
        else:
            print(f"skip (not .vrma): {p}", file=sys.stderr)

    if not paths:
        print("no input .vrma files", file=sys.stderr)
        return 2

    out_dir: Path = args.out_dir.expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    ok = 0
    for src in paths:
        try:
            data = convert_vrma_to_animation_dict(
                src, skip_identity=not args.no_skip_identity
            )
        except Exception as e:
            print(f"FAIL {src}: {e}", file=sys.stderr)
            continue
        slug = _slugify_animation_stem(src.stem) + ".json"
        dest = out_dir / slug
        dest.write_text(json.dumps(data, indent=2), encoding="utf-8")
        print(f"wrote {dest} ({data['frameCount']} frames, fps={data['fps']})")
        ok += 1
    print(f"done: {ok}/{len(paths)}")
    return 0 if ok == len(paths) else 1


if __name__ == "__main__":
    raise SystemExit(main())
