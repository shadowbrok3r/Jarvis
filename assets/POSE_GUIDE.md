# VRM Pose Authoring Guide

Reference for MCP callers and humans. Poses live in **normalized pose space**: each bone’s value is a **unit quaternion** `pose_q = [x, y, z, w]` applied by `pose_driver` (see repo) so that `pose_q = [0,0,0,1]` leaves that bone at its VRM rest. This is **not** world space and **not** Euler angles unless you use the MCP tools below.

## MCP tools (use this order)

1. **`make_fist`** — `amount` from 0 (relaxed curl template) to 1 (full fist). Defaults to both hands. Use this instead of typing thirteen finger quaternions.
2. **`pose_bones`** — Preferred for everything else. You send **degrees** per bone: `pitch_deg`, `yaw_deg`, `roll_deg` (each optional; missing = 0). The server converts with **intrinsic local Euler order XYZ** (pitch around local X, then Y, then Z), **clamps** each angle to safe per-bone limits, **normalizes** the quaternion, then optionally clamps xyz again. The tool response lists **warnings** whenever something was clamped or normalized.
3. **`adjust_bone`** — Same Euler axes as `pose_bones`, but values are **small delta degrees** composed as `current_pose_q * delta_q`. Use about ±2° to ±8° per step.
4. **`get_current_bone_state` → `set_bones`** — Round-trip path: `set_bones` accepts the same quaternions the snapshot returns. The server still **normalizes** and **clamps** xyz so broken pasted values do not explode the rig.
5. **`create_pose`** — Saves quaternions to disk; unknown bone keys are dropped; each quaternion is sanitized like `set_bones`.

Do **not** “design” combined rotations by independently picking x, y, z components. Quaternion composition is **multiplication**, not addition. For any multi-axis pose on one bone, use **`pose_bones`** (or small steps with **`adjust_bone`**).

## Euler convention (pose_bones / adjust_bone)

- **pitch** — local **X** (flex / extend for many limb segments).
- **yaw** — local **Y** (twist around the bone’s length).
- **roll** — local **Z** (abduction / adduction–like motion in this order).

Per-bone degree limits are defined in `src/mcp/pose_authoring.rs` (`euler_limit_deg`). Finger segments allow large **roll** (curl) but tight pitch/yaw so fingers do not corkscrew.

### Named bends (signs are a guide; stay within small angles first)

- **Elbow (`*LowerArm`)** — flexion is usually **negative pitch** in this rig (check the viewport). Large positive pitch on the forearm often looks “bent backward.”
- **Knee (`*LowerLeg`)** — flexion is usually **negative pitch** relative to the thigh.
- **Finger curl** — mostly **roll**; keep pitch/yaw near 0 unless you need a tiny spread. Right-hand curl tends toward **positive roll**; left-hand toward **negative roll** for the same visual curl direction.

### Mirroring left and right

Do **not** mirror by negating quaternion Y and Z — that is unreliable once rest poses are non-trivial. Instead: build the right side with `pose_bones`, then repeat with **opposite signs on yaw and roll** (and sometimes pitch) on the left, **or** call `make_fist` / tune one side and mirror using small `adjust_bone` steps while watching the viewport.

## Quaternion rules (only if you touch `set_bones` or JSON files)

- Must be **unit length**: \(x^2 + y^2 + z^2 + w^2 = 1\). If you invent x,y,z, set \(w = \sqrt{\max(0, 1 - x^2 - y^2 - z^2)}\) and let the server renormalize.
- **`q` and `-q` are the same rotation**; the MCP layer may flip the sign for a stable hemisphere.
- After unit length, **|x|, |y|, |z|** are capped per bone class (see `max_xyz_component_for_bone` in `src/mcp/pose_authoring.rs`: hips / major limbs / feet allow higher caps for deep bends; hands / toes stay tighter; fingers higher). Oversized xyz is **scaled down** and a warning is returned.

## Floor sit (rotation-only)

`pose_bones` only drives **bone rotations**. It does **not** move the VRM scene root. If the character’s **hips look floating** above the ground plane while the legs are folded, lower the avatar root: use the Avatar window controls or set `[avatar].world_position` in `config/default.toml` (see comments there and `src/plugins/avatar.rs`). `lock_root_y` / `lock_vrm_root_y` interact with vertical locking — adjust if the root keeps snapping back.

**Knee direction (MMD-style rigs):** knee flex is usually **negative** `pitch_deg` on `*LowerLeg`. If the knee bends the wrong way, flip the sign (try **positive** `pitch_deg` on `*LowerLeg`) and keep `*UpperLeg` as the parent driver for the thigh fold.

**Torso vs head:** put most of the forward lean on **spine → chest → upperChest** (moderate positive pitch on each, parent-to-child). Keep **neck** and **head** closer to neutral (small angles) so the gaze stays forward instead of staring at the floor.

**Arms on knees:** after the legs read as a sit, add **shoulder / upperArm** outward rotation and **lowerArm** flex so forearms meet the thighs — tune in small steps and read MCP warnings.

## Bone hierarchy

```
hips
├── spine
│   └── chest
│       └── upperChest
│           ├── neck
│           │   └── head
│           ├── leftShoulder
│           │   └── leftUpperArm
│           │       └── leftLowerArm
│           │           └── leftHand
│           │               ├── leftThumbMetacarpal → leftThumbProximal → leftThumbDistal
│           │               ├── leftIndexProximal → leftIndexIntermediate → leftIndexDistal
│           │               ├── leftMiddleProximal → leftMiddleIntermediate → leftMiddleDistal
│           │               ├── leftRingProximal → leftRingIntermediate → leftRingDistal
│           │               └── leftLittleProximal → leftLittleIntermediate → leftLittleDistal
│           └── rightShoulder
│               └── rightUpperArm
│                   └── rightLowerArm
│                       └── rightHand
│                           ├── rightThumbMetacarpal → rightThumbProximal → rightThumbDistal
│                           ├── rightIndexProximal → rightIndexIntermediate → rightIndexDistal
│                           ├── rightMiddleProximal → rightMiddleIntermediate → rightMiddleDistal
│                           ├── rightRingProximal → rightRingIntermediate → rightRingDistal
│                           └── rightLittleProximal → rightLittleIntermediate → rightLittleDistal
├── leftUpperLeg
│   └── leftLowerLeg
│       └── leftFoot
│           └── leftToes
└── rightUpperLeg
    └── rightLowerLeg
        └── rightFoot
            └── rightToes
```

Work **parent to child** (shoulder before elbow before wrist).

## Quaternion examples (legacy / round-trip only)

These are **illustrative** single-axis-ish samples. Prefer `pose_bones` with degrees for new work.

| Movement | Quaternion `[x,y,z,w]` |
|----------|-------------------------|
| Slight forward lean (spine) | `[0.05, 0, 0, 0.999]` |
| Elbow bent ~30° (often negative pitch axis in quats) | `[-0.13, 0, 0, 0.992]` |
| Head turn right slight | `[0, -0.08, 0, 0.997]` |

## Relaxed hand and fist JSON (reference)

The **`make_fist`** MCP tool internally blends between the relaxed template and the fist set below (right hand keys). You normally **do not** paste this JSON by hand.

<details>
<summary>Relaxed right-hand template (excerpt)</summary>

```json
{
  "rightThumbMetacarpal": { "rotation": [0, -0.04, 0.02, 0.999] },
  "rightThumbProximal": { "rotation": [0, -0.06, 0, 0.998] },
  "rightIndexProximal": { "rotation": [0, 0, 0.1, 0.995] },
  "rightMiddleProximal": { "rotation": [0, 0, 0.12, 0.993] }
}
```

</details>

<details>
<summary>Full fist right-hand reference</summary>

```json
{
  "rightThumbProximal": { "rotation": [-0.21, -0.57, 0.40, 0.68] },
  "rightIndexProximal": { "rotation": [0, 0, 0.42, 0.908] },
  "rightIndexIntermediate": { "rotation": [0, 0, 0.68, 0.733] },
  "rightIndexDistal": { "rotation": [0, 0, 0.35, 0.937] },
  "rightMiddleProximal": { "rotation": [0, 0, 0.44, 0.898] },
  "rightMiddleIntermediate": { "rotation": [0, 0, 0.70, 0.714] },
  "rightMiddleDistal": { "rotation": [0, 0, 0.35, 0.937] },
  "rightRingProximal": { "rotation": [0, 0, 0.43, 0.903] },
  "rightRingIntermediate": { "rotation": [0, 0, 0.68, 0.733] },
  "rightRingDistal": { "rotation": [0, 0, 0.36, 0.933] },
  "rightLittleProximal": { "rotation": [0, 0, 0.45, 0.893] },
  "rightLittleIntermediate": { "rotation": [0, 0, 0.70, 0.714] },
  "rightLittleDistal": { "rotation": [0, 0, 0.42, 0.908] }
}
```

</details>

## Common mistakes

1. **Treating quaternion x/y/z as three independent sliders** — wrong for combined rotations; use **`pose_bones`**.
2. **Non-unit quaternions** — the server fixes them but you should still aim for unit length when authoring JSON.
3. **Skipping the parent chain** — rotating `rightLowerArm` without placing `rightUpperArm` first usually looks wrong.
4. **Huge finger values** — use **`make_fist`** instead of guessing quaternions.
5. **Ignoring MCP warnings** — if the server clamped something, the pose is not what you typed.

## VRM expressions

| Expression | Effect |
|-----------|--------|
| happy | Smile, raised cheeks |
| angry | Furrowed brows, tense jaw |
| sad | Drooping corners, raised inner brows |
| relaxed | Soft smile, half-closed eyes |
| surprised | Wide eyes, raised brows, open mouth |
| neutral | Default resting face |

Weights are 0..1; natural range is often about 0.2–0.6.

## Iterative workflow (MCP)

1. `reset_pose` or `apply_pose` for a baseline.
2. `make_fist` with a small `amount` if you only need believable hands.
3. `pose_bones` for body — a few degrees per bone, then read **warnings**.
4. `capture_pose_views` with a deterministic `capture_id` and either `framing_preset: "full_body"` or `framing_preset: "face_closeup"`.
5. `adjust_bone` / `set_expression` for tiny corrections.
6. Re-capture and compare against the previous pass.
7. `get_current_bone_state` if you must switch to quaternion tools.
8. `create_pose` to persist.

## Visual verification loop (recommended)

1. Apply a coarse body pose with `pose_bones` (and `make_fist` if needed).
2. Capture validation images with `capture_pose_views`:
   - `output_dir` required.
   - Use explicit `views`: `["front","left","right","front_left","front_right"]`.
   - Use higher resolution while tuning (for example `width=1536`, `height=1536`).
   - Use a deterministic `capture_id` so files are easy to diff across passes.
   - `framing_preset: "full_body"` pulls camera framing back so lower legs + heels stay visible.
   - `framing_preset: "face_closeup"` focuses on head/face for expression QA.
   - Optional `camera_overrides` (`focus_y_offset`, `radius`, `height_lift`) let you fine-tune framing without changing view lists.
3. Inspect all requested views, then run only minimal `adjust_bone` changes (about ±2° to ±8° per call) or small `set_expression` changes (about ±0.05 to ±0.15).
4. Re-capture and stop when the change is clearly better; if a pass regresses silhouette, facial readability, or comfort, revert direction and reduce deltas.

`capture_pose_views` output is a deterministic list of generated PNG paths per requested view (filename pattern: `<capture_id>_<view>_<WxH>.png`) plus an `errors` array.

### Full-body pose verification loop

Use this while tuning body balance, heel contact, head tilt, and wave silhouettes:

```json
{
  "output_dir": "~/Desktop/JarvisAvatar/pose_captures",
  "capture_id": "idle_wave_pass03",
  "views": ["front", "left", "right", "front_left", "front_right"],
  "framing_preset": "full_body",
  "width": 1536,
  "height": 1536
}
```

### Facial expression verification loop

Use this while dialing eyes, brows, mouth, and expression blend:

```json
{
  "output_dir": "~/Desktop/JarvisAvatar/pose_captures",
  "capture_id": "smile_mix_pass07",
  "views": ["front", "front_left", "front_right"],
  "framing_preset": "face_closeup",
  "camera_overrides": {
    "focus_y_offset": 1.62,
    "radius": 0.68,
    "height_lift": 0.05
  },
  "width": 1280,
  "height": 1280
}
```

### Apply -> capture -> adjust (repeat)

1. **Apply**: `pose_bones` / `set_expression` / `make_fist`.
2. **Capture**: `capture_pose_views` with deterministic `capture_id` (`..._pass01`, `..._pass02`, ...).
3. **Adjust**: small deltas only; one intent per pass (for example, heel contact or smile intensity).
4. Repeat until front + side + 3/4 views all read correctly.

## Seated pose tuning notes (this rig)

- **Backward knees:** if lower legs appear to fold the wrong way, flip `*LowerLeg.pitch_deg` sign first, then retune `*UpperLeg.pitch_deg`. On this rig, knee flex often reads best with negative lower-leg pitch during seated refinement.
- **Elbows to knees (priority order):**
  1. Place torso lean (`spine` → `chest` → `upperChest`).
  2. Move `*UpperArm` to set elbow position near the thigh line.
  3. Flex `*LowerArm` last to rest forearm/hand onto the knee zone.
  Keep per-pass elbow changes small; large forearm deltas clamp quickly.
- **Feet / heel contact:** tune `*LowerLeg` before `*Foot`. Use `*Foot.pitch_deg` to seat heel contact and `*Foot.yaw_deg` for slight outward toe angle. Use small opposite left/right yaw signs for symmetry.
- **Safe step sizes:** start around ±3° to ±5° for torso/upper limbs and ±2° to ±4° for wrists/feet; only push toward ±8° when a view confirms the previous delta was too small.
- **Bone-name casing:** always use canonical names from `get_bone_reference` (for example `leftLowerLeg`, not lowercase aliases) before bulk edits.
