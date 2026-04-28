# VRM Pose Authoring Guide

Reference for MCP callers and humans. Poses live in **normalized pose space**: each bone’s value is a **unit quaternion** `pose_q = [x, y, z, w]` applied by `pose_driver` (see repo) so that `pose_q = [0,0,0,1]` leaves that bone at its VRM rest. This is **not** world space and **not** Euler angles unless you use the MCP tools below.

## MCP tools (use this order)

1. **`pose_bones`** — Preferred for **body and limbs**. You send **degrees** per bone: `pitch_deg`, `yaw_deg`, `roll_deg` (each optional; missing = 0). MCP argument **`bones` is a JSON object (map)** from bone name → `{ "pitch_deg"?, "yaw_deg"?, "roll_deg"? }` — **not** an array; a list shape will fail deserialization. The server converts with **intrinsic local Euler order XYZ** (pitch around local X, then Y, then Z), **clamps** each angle to safe per-bone limits, **normalizes** the quaternion, then optionally clamps xyz again. The tool response lists **warnings** whenever something was clamped or normalized.
2. **`make_fist`** — `amount` from 0 (relaxed curl template) to 1 (full fist). Defaults to both hands. Use this instead of typing many finger quaternions.
3. **`adjust_bone`** — **Not Euler degrees.** This tool adds `delta_x`, `delta_y`, `delta_z` to the bone’s **current pose quaternion x/y/z components**, then **renormalizes** the quaternion to unit length (the `w` component is scaled with the vector part so length stays 1). It does **not** multiply `current_pose_q * delta_q`. Use **very small** deltas on **one axis at a time** (often **±0.02 to ±0.05**) for tiny viewport fixes after `pose_bones` or after Kimodo playback. For anything larger than a micro-nudge, use **`pose_bones`** with `pitch_deg` / `yaw_deg` / `roll_deg` instead.
4. **`get_current_bone_state` → `set_bones`** — Round-trip path: `set_bones` accepts the same quaternions the snapshot returns. The server still **normalizes** and **clamps** xyz so broken pasted values do not explode the rig.
5. **`create_pose`** — Saves quaternions to disk; unknown bone keys are dropped; each quaternion is sanitized like `set_bones`.
6. **`list_models`** / **`load_vrm`** — `list_models` scans `assets/models/*.vrm` from the process cwd (sorted basenames + `models/…` paths). `load_vrm` enqueues a runtime swap: the previous avatar root is despawned, `[avatar].model_path` in live `Settings` updates, bone index / snapshot / blend transitions / expression-animation state reset, then the new file loads with the same `[avatar].idle_vrma_path` child as at startup (empty string = no idle). Expressions and face overrides clear; spring/collider preset auto-apply runs again only if `[avatar].auto_load_spring_preset` is true when the new rig hits `Initialized`. `pose_bones` / `get_bone_reference` may return empty or stale until the new rig indexes — wait a frame or two after `load_vrm` before driving bones. The same scan + hot-swap queue is available in-app under **View → Avatar** (filter, list, **Load selected**, or double-click a row).

Do **not** “design” combined rotations by independently picking x, y, z quaternion components. Quaternion composition is **multiplication**, not addition. For any multi-degree pose on one bone, use **`pose_bones`**. Reserve **`adjust_bone`** for **micro** quaternion-component tweaks only.

## Euler convention (`pose_bones` only)

`adjust_bone` does **not** use this Euler convention — see the `adjust_bone` bullet in **MCP tools** above.

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

## Extra Rigify / skin bones (`DEF-toe*`, `DEF-ero*`, …)

When the loaded VRM includes **skin-only** joints not in the VRM humanoid map (indexed by glTF node `Name`, e.g. `DEF-toe_big.L`), they use the **same normalized pose quaternion space** as standard bones: identity means bind pose, and MCP **`pose_bones`** (intrinsic XYZ degrees), **`set_bones`**, **`adjust_bone`**, **`get_current_bone_state`**, and **`create_pose`** accept those names when they appear in **`get_bone_reference` → `extraBones`** (or match the `DEF-toe` / `DEF-ero` prefix before the first snapshot). **Saved pose JSON** from older builds may store legacy per-bone deltas for such joints; re-capture or re-author those entries if rotations look wrong after an upgrade.

In the **Pose Controller → Bones** tab, **Snapshot → sliders** uses a geodesic near-identity check so Euler readouts do not stick on `(±180°, ε, ∓180°)` aliases for tiny rotations. The per-bone **↺** control restores the **full** bind `Transform` from `RestTransform` (translation + rotation + scale), not rotation-only — Rigify toes often need that so zeroing sliders matches the mesh you see after a full **Reset pose**.

For **`DEF-toe_{big,index,middle,ring,little}.{L,R}`** (standard Rigify-style per-digit skin toe names), the Bones tab and **`pose_bones`** apply the same fixed **±180° yaw** offset (**L** = `+180°`, **R** = `−180°`) between **display Y** (sliders / MCP `yaw_deg`, ~0 at bind) and the intrinsic Euler passed to `from_euler`, so `(0°,0°,0°)` matches neutral pad-facing the way the viewport already does for the big toe. **Snapshot → sliders** subtracts that offset when seeding readouts — **only when** the snapshot Euler is not already the all‑zero near‑bind alias (so a real `Y≈180°` pose is still shown). Raw **`set_bones`** quaternions remain true normalized `pose_q` space (use **`get_current_bone_state`** for round‑trip quats).

Other **`DEF-toe*`** joints use a **wider geodesic snap** (~34° from identity) when seeding sliders so Euler “±180° on X/Z, tiny Y” aliases for **small** skin twists still display as ~`0°` after a clean export.

**`DEF-toe_little` / `ring` / `middle` / `index` (extra children under the parent `DEF-toe.*` metatarsal):** Some Rigify exports add per-digit toe children aligned with the parent metatarsal toe bone’s local axis — commonly **head→tail along local Y** (toe length), matching the parent `DEF-toe.*` **y_axis**. In `pose_bones`’ intrinsic **XYZ** order, **local Y is the bone’s length axis**, so **`yaw_deg` is an axial twist**. Use **`pitch_deg` / `roll_deg`** for lateral fanning in small steps; **left vs right `DEF-toe_*` are not naive mirrors of each other** (see **`DEF-toe.L` vs `.R`** below). The shared **±180° display-yaw** rebasing above keeps MCP **absolute** Euler in the same frame as the Bones tab so small pitch/roll edits do not land ~180° off the visible bind alias.

**Example — 10 named toes (fan test payload for `pose_bones`):**

```json
{
  "bones": {
    "DEF-toe_index.L": { "pitch_deg": 6, "roll_deg": -4 },
    "DEF-toe_middle.L": { "pitch_deg": 8, "roll_deg": -3 },
    "DEF-toe_ring.L": { "pitch_deg": 8, "roll_deg": 2 },
    "DEF-toe_little.L": { "pitch_deg": 6, "roll_deg": 4 },
    "DEF-toe_big.L": { "pitch_deg": 4 },
    "DEF-toe_index.R": { "pitch_deg": -6, "roll_deg": 4 },
    "DEF-toe_middle.R": { "pitch_deg": -8, "roll_deg": 3 },
    "DEF-toe_ring.R": { "pitch_deg": -8, "roll_deg": -2 },
    "DEF-toe_little.R": { "pitch_deg": -6, "roll_deg": -4 },
    "DEF-toe_big.R": { "pitch_deg": -4 }
  },
  "preserve_omitted_bones": true
```

### `DEF-toe.L` vs `.R`: same fan, different numeric signs

- The **fan test** JSON above is intentional: **`.L` digits use mostly positive `pitch_deg`** while **`.R` use negative `pitch_deg`** for a similar *lateral* spread. **`roll_deg` patterns also differ by side**. This is **not** the same “mirror” rule as humanoid limbs (where you often negate **yaw** and **roll** only).
- **Anti-pattern:** copy a working **`.R`** `pitch_deg` / `roll_deg` recipe onto **`.L`** (even after flipping `yaw_deg`). A common failure is **all digits curling toward `DEF-toe_big`** — visually “toes point at the big toe” instead of fanning across the pad. See also [**DEF-toe squeeze toward big toe**](#def-toe-squeeze-toward-big-toe) for aggregate `leftToes` / `rightToes` fighting per-toe fans.
- **Recovery ladder:** (1) **Flip the sign of `pitch_deg` on every `.L` digit** relative to the `.R` recipe you started from (compare to the **fan test** block). (2) If digits still converge, **swap or invert `roll_deg` polarity** between **index/middle** and **ring/little** in **1–3°** steps. (3) Keep **`yaw_deg` at `0`** unless you deliberately want twist along the toe length (local **Y** is the bone axis).
- **Best practice:** tune **`.R` first**, then author **`.L` from the fan test template** or from **Snapshot → sliders** / `get_current_bone_state` readouts — not by blind sign mirroring from `.R`.

### DEF-toe squeeze toward big toe

**Symptom:** after MCP apply, digits visually **converge on `DEF-toe_big`** (pinched fan, hallux “attractor”) instead of spreading across the pad.

**Causes:**

- **Wrong L recipe** — copying **`.R`** `pitch_deg` / `roll_deg` signs onto **`.L`**; fix per [**`DEF-toe.L` vs `.R`**](#def-toel-vs-r-same-fan-different-numeric-signs).
- **Aggregate `leftToes` / `rightToes` fighting per-toe `DEF-toe_*`** — parent chain rotation stacks on per-digit fans and can collapse the read toward the big toe. **Neutralize** aggregate toes (`pitch_deg` / `yaw_deg` / `roll_deg` near **0** or small) **before** or **while** tuning per-digit fans, then re-apply the DEF fan block.

**Recovery order:** (1) reset aggregate **`leftToes`** / **`rightToes`** toward neutral; (2) re-apply the **fan-test** `.L` block from [**`DEF-toe.L` vs `.R`**](#def-toel-vs-r-same-fan-different-numeric-signs); (3) micro-**`roll_deg`** on **ring/little** vs **index/middle** in **1–3°** steps until the pad reads even.

## Quaternion rules (only if you touch `set_bones` or JSON files)

- Must be **unit length**: \(x^2 + y^2 + z^2 + w^2 = 1\). If you invent x,y,z, set \(w = \sqrt{\max(0, 1 - x^2 - y^2 - z^2)}\) and let the server renormalize.
- **`q` and `-q` are the same rotation**; the MCP layer may flip the sign for a stable hemisphere.
- After unit length, **|x|, |y|, |z|** are capped per bone class (see `max_xyz_component_for_bone` in `src/mcp/pose_authoring.rs`: hips / major limbs / feet allow higher caps for deep bends; hands / toes stay tighter; fingers higher; **`DEF-toe*`** uses cap **1.0** so bind-aligning yaw is not crushed). Oversized xyz is **scaled down** and a warning is returned.

## Floor sit (rotation-only)

`pose_bones` only drives **bone rotations**. It does **not** move the VRM scene root. If the character’s **hips look floating** above the ground plane while the legs are folded, lower the avatar root: use the Avatar window controls or set `[avatar].world_position` in `config/default.toml` (see comments there and `src/plugins/avatar.rs`). `lock_root_y` / `lock_vrm_root_y` interact with vertical locking — adjust if the root keeps snapping back.

**Knee direction (MMD-style rigs):** knee flex is usually **negative** `pitch_deg` on `*LowerLeg`. If the knee bends the wrong way, flip the sign (try **positive** `pitch_deg` on `*LowerLeg`) and keep `*UpperLeg` as the parent driver for the thigh fold.

**Torso vs head:** put most of the forward lean on **spine → chest → upperChest** (moderate positive pitch on each, parent-to-child). Keep **neck** and **head** closer to neutral (small angles) so the gaze stays forward instead of staring at the floor.

**Arms on knees:** after the legs read as a sit, add **shoulder / upperArm** outward rotation and **lowerArm** flex so forearms meet the thighs — tune in small steps and read MCP warnings.

### Forward leg raise (knee + thigh direction)

- **Symptom → fix (leg behind body):** If the raised leg extends **behind** the torso instead of **in front**, the primary lever is usually **`leftUpperLeg` / `rightUpperLeg`** — try **flipping the sign on `pitch_deg` first** (often +↔−), then adjust **`yaw_deg` in ±10–20°** steps. Do **not** only crank `*LowerLeg` when the thigh aim is wrong.
- **Knee bend wrong way:** On MMD-style rigs, knee flex is usually **negative `pitch_deg` on `*LowerLeg`**; if the knee collapses backward, **flip the sign on `*LowerLeg` `pitch_deg`** before touching the foot.
- **Order of operations:** (a) thigh aim forward, (b) slight `*LowerLeg` bend, (c) `*Foot` — toe forward, (d) `*Toes` then `DEF-toe_*` for fan — small steps; read MCP **`warnings`** for clamps.
- **Verification:** `capture_pose_views` with **`framing_preset: "full_body"`** and at least **left** and **right** views to catch “leg behind” mistakes early. For the full multi-view **done** gate (front/sides, optional 3/4), see [**Self-corrective workflow (verify before “done”)**](#self-corrective-workflow-verify-before-done).
- **Toe fan:** Set aggregate **`leftToes` / `rightToes`** first; add per-toe **`DEF-toe_*`** only if **`get_bone_reference`** lists them — prefer **yaw/roll** before **pitch** for fan.

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

### Time-varying expressions (MCP `animate_expressions`)

Use **`animate_expressions`** when you need a **short in-engine curve** on VRM expression presets (smile ramp, blink-style on/off on `blinkLeft` / `blinkRight`, viseme-like fades on `aa` / `ih` / …). It is **not** a saved Kimodo clip and **does not** add expression tracks to `generate_motion` JSON — it samples **piecewise-linear** paths in Bevy each frame and applies them via `ModifyExpressions` (same family as `set_expression`, but time-varying).

**Behavior**

- **`keyframes`**: each entry is `{ "time_s": number, "weights": { "<preset>": 0..1, ... } }`. The server **sorts** by `time_s`. Between two keyframes, every preset name that appears in **either** weight map is lerped; a missing name on one side is treated as weight **0** for that segment.
- **`duration_seconds`**: optional. If omitted, the clip length is `max(time_s)` (floored to at least **0.05s**, capped at **120s**). If your explicit duration is shorter than the last keyframe’s `time_s`, it is **extended** to that last time so the curve is not truncated.
- **`looping`**: optional, default `false`. When `true`, sample time wraps with `duration_seconds` as the period.
- **Limits**: at most **256** keyframes per call; weights are clamped to **0..1** on ingest.
- **Cancellation**: `reset_pose`, `set_expression`, **`apply_pose`** when the saved pose includes expressions, and hub `vrm:apply-pose` / `vrm:apply-expression` with expression data all **stop** the clip so static faces stick. **Layered** in-app expression output (`anim_layers` blink, idle pose expressions, etc.) still runs **first** each frame; animated presets from this tool **override** those keys **last** for that frame (so you can keep blink if you do not animate the blink presets).
- **After a one-shot clip ends**, the **last sampled** expression weights stay on the face until you change them (`reset_pose`, `set_expression`, another clip, etc.).
- **Chat / ACT decay** uses a full neutral `SetExpressions` pass and can **override** the face independently of this tool.

**Verification loop (face_closeup)**

1. `reset_pose` or `apply_pose` baseline if needed.
2. Call `animate_expressions` (one-shot or loop).
3. Wait at least **`duration_seconds`** wall time (plus a small buffer) before captures for one-shot clips so the final frame is visible.
4. `capture_pose_views` with `framing_preset: "face_closeup"` (see **Facial expression verification loop** below). For mid-clip shapes, trigger captures from a parallel client while the clip plays, or use a **looping** clip and capture after it has run long enough to reach the phase you care about.

**Example — happy ramp + blink pulse**

```json
{
  "keyframes": [
    { "time_s": 0.0, "weights": { "happy": 0.0, "relaxed": 0.15 } },
    { "time_s": 0.35, "weights": { "happy": 0.55, "relaxed": 0.2 } },
    { "time_s": 0.55, "weights": { "happy": 0.45, "blinkLeft": 1.0, "blinkRight": 1.0 } },
    { "time_s": 0.62, "weights": { "happy": 0.45, "blinkLeft": 0.0, "blinkRight": 0.0 } }
  ],
  "duration_seconds": 1.2,
  "looping": false
}
```

**What is still out of scope**

- **Kimodo `generate_motion`** remains body-focused from text; it does not receive expression keyframe tracks in this build.
- **No MCP tool** yet for stacked **animation layer sets** (`config/anim_layer_sets.json`); that stays in the debug UI.
- **Easing**: only linear segments between keyframes; use denser keyframes to approximate ease curves.

## Iterative workflow (MCP)

1. `reset_pose` or `apply_pose` for a baseline.
2. `pose_bones` for body — a few degrees per bone, then read **warnings**.
3. `make_fist` with a small `amount` if you only need believable hands (can come before or after body depending on the shot).
4. `capture_pose_views` with a deterministic `capture_id` and either `framing_preset: "full_body"` or `framing_preset: "face_closeup"`.
5. **`set_expression`** for a **static** face mix, or **`animate_expressions`** for short **time-varying** curves (ramps, blinks, viseme fades — see **Time-varying expressions** above); then **`adjust_bone`** only for **micro** quaternion deltas if something is still a hair off.
6. Re-capture and compare against the previous pass.
7. `get_current_bone_state` if you must switch to quaternion tools.
8. `create_pose` to persist a static pose; use `generate_motion` + `save_name` for clips (see **Complex choreography** below).

## Complex choreography (example: stand → floor sit → legs )

This is **hard** on a single prompt. Treat it as **phased work**: coarse motion from Kimodo where it behaves, then **MCP** for anatomy-safe detail (knees, toes, fingers, face).

### What Kimodo is good at

- **Large** translation of the whole skeleton over time: walking, waving, rough sit transitions, leg swings, if the prompt is explicit about timing (“over 2 seconds”, “hold for 1 second”, “return to standing smoothly”).
- **One clear intent per generation** works better than stuffing every requirement into one paragraph.

### What to do in MCP (or split Kimodo clips)

- **Knees / elbows:** use `pose_bones` with **small** degree steps; fix backward knees by flipping `*LowerLeg` `pitch_deg` sign (see **Seated pose tuning** below).
- **Toes (`leftToes`, `rightToes`):** small `pitch_deg` / `roll_deg` only — caps in `pose_authoring` are **tight** so toes curl a little, not cartoon spiral.
- **Finger curls beyond the fist template:** either repeat `make_fist` at intermediate `amount` values, or carefully add `roll_deg` on proximal/intermediate phalanges with **warnings** enabled.
- **Face:** Kimodo may or may not hit blendshapes you like. Plan on **`set_expression`** or **`animate_expressions`** passes while using `capture_pose_views` with **`face_closeup`**.

### Animation layering (in-app today)

Stacked / layered playback is driven from the **debug UI** and persisted **`config/anim_layer_sets.json`** (`anim_layer_sets` plugin) — **not** separate MCP tools yet. Workflow: save discrete `.json` clips with `generate_motion` (`save_name`), verify with `list_generated_animations`, tag with `update_animation_metadata`, then combine layers in the UI for toe/finger overlays you do not want Kimodo to own.

### Kimodo save path (do not skip)

If `generate_motion` returns `librarySaveMissing`, Kimodo wrote the file somewhere other than this application’s `[pose_library].animations_dir`. Align **`JARVIS_ANIMATIONS_DIR`** on the Kimodo process with that directory (Kimodo reads `config/user.toml` when unset — keep **the same cwd** as the pose MCP host for relative paths like `./assets/animations`). See `docs/MCP_POSE_ANIMATION_GUIDE.md`.

## Animation + expression (layering)

### VRMA → library JSON (offline)

Many VRMA packs only carry **humanoid rotations** (no VRM expression curves). To turn them into the same **`AnimationFile`** JSON Kimodo and the pose library use, run from the repository root the VRMA→JSON converter in `scripts/` (list that directory for the `vrma_to*animation_json.py` entry point; exact basename may differ by checkout):

`python3 "$(ls scripts/vrma_to*animation_json.py | head -n1)" <dirs-or-.vrma-files> -o <output_dir>`

Defaults write under **`assets/animations/imported_vrma/`** in this repo (git-tracked samples). For live layering, **copy or symlink** those `.json` files into your effective **`[pose_library].animations_dir`** (see `config/default.toml` / `config/user.toml`; paths use the same `~/` expansion as the app), or change `-o` to that directory directly.

**Converter limitations:** ignores **root translation** (hips position) — same rotation-only contract as Kimodo clips; **no VRMC expression** tracks; assumes **aligned** glTF sampler times across bones. Per-frame **`durationMs`** is preserved in JSON for tooling; **native playback and layer clip sampling** still advance by uniform `fps` (they do not yet integrate variable per-frame spacing).

### Per-frame expressions inside `AnimationFile`

Each **`frames[]`** entry may include an **`expressions`** object: VRM expression name → weight in **0..1** (same names as `get_bone_reference` / `set_expression`).

- **Animation layer stack** (`anim_layers`): clip drivers now **sum** those weights into the layer pass (with layer weight), same as bone blending — you can stack a **body VRMA import** clip with a second clip that only carries **`expressions`** (empty **`bones`**) if you author one.
- **Native JSON playback** (`NativeAnimPlayer` / Kimodo streaming): when a frame’s **`expressions`** map is non-empty, the engine issues **`ApplyExpression`** after **`ApplyBones`** for that tick (`cancel_expression_animation: false`). **Omitted keys are not cleared** — if you ramp a morph down, put an explicit **`0.0`** for that name on the next keyframe (or call **`reset_pose`** / **`set_expression`** between clips).

VRMA imports from the script above omit **`expressions`**; add them by editing JSON or with a small external merge tool. **Future MCP (not implemented here)** could include: `patch_animation_expressions` (filename + parallel arrays of frame indices and weight maps), or a documented **sidecar** `{stem}.expressions.json` with `{ "frames": [ { "t": number, "weights": { } }, ... ] }` resolved against **`fps`** / frame count — pick one format per pipeline and keep Kimodo’s **`JARVIS_ANIMATIONS_DIR`** aligned with **`animations_dir`** when testing.

### MCP vs baked face motion

- **`animate_expressions`** — short **in-engine** curves (one-off, not saved as part of `AnimationFile` unless you copy weights into JSON by hand).
- **`set_expression`** — static snapshot; good between captures.
- **Baked `frames[].expressions`** — best when the **layer stack** should replay face and body together from disk **without** extra MCP traffic.

## Troubleshooting

| Symptom | What to check |
|--------|----------------|
| `librarySaveMissing` after `generate_motion` | Kimodo `ANIMATIONS_DIR` vs `[pose_library].animations_dir`; `JARVIS_ANIMATIONS_DIR`; restart Kimodo after env changes. |
| `generate_motion` times out | Kimodo peer offline on hub; increase `timeout_sec`; shorter `duration` / fewer `steps` for tests. |
| Knee bends backward | Flip `*LowerLeg` `pitch_deg` sign in `pose_bones`; keep thigh (`*UpperLeg`) adjustments smaller. |
| Fingers corkscrew | Use **`make_fist`** or reduce pitch/yaw on fingers — curl is mostly **roll**. |
| Pose MCP tools no-op | Avatar app running with MCP server; `PoseDriver` / hub loaded. |
| Stale tool descriptions in Cursor | Restart the avatar app and **refresh** the MCP server in Cursor so cached `tools/*.json` updates. |

## Self-corrective workflow (verify before “done”)

Applies to **all** body-part and face work: treat authoring as **closed-loop**—tool success alone is not sufficient.

**After material body changes** — `pose_bones`, leg chains, `make_fist`, Kimodo apply, hub leg/pose applies, or similar — always run **`capture_pose_views`** with **`framing_preset: "full_body"`** and `views` including at least **`front`**, **`left`**, **`right`**. For shoulders, arms, and torso silhouette, add **`front_left`** and **`front_right`**.

**Capture vs. main viewport:** offscreen capture cameras aim at the **loaded VRM root’s world position** (after transform propagation), not the orbit gameplay camera — you still get valid transparent PNGs if she is panned off-screen, as long as the model stays in the scene.

**After face work** — `set_expression`, `animate_expressions`, or expression-heavy poses — use **`framing_preset: "face_closeup"`** with **`front`**, **`front_left`**, **`front_right`**.

**Iteration:** adjust only what **failed** the last captures — small **`adjust_bone`** (quaternion deltas) or **targeted** `pose_bones` / `set_expression` on those bones or weights — then **re-capture** with the same framing and view set. Repeat until acceptable or you hit diminishing returns (avoid re-driving unrelated bones).

**Completion gate:** do **not** declare the task finished until captures are **reviewed** (humans: open the PNGs; **agents: read the image files** at paths returned in the MCP response). Skipping review is an incomplete run.

Concrete payloads and camera overrides: **Visual verification loop** below.

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
3. Inspect all requested views, then run only minimal **`adjust_bone`** changes (**quaternion component** deltas, often ±0.02–0.05 on **one** of `delta_x` / `delta_y` / `delta_z`) or small `set_expression` changes (about ±0.05 to ±0.15).
4. Re-capture and stop when the change is clearly better; if a pass regresses silhouette, facial readability, or comfort, revert direction and reduce deltas.

`capture_pose_views` output is a deterministic list of generated PNG paths per requested view (filename pattern: `<capture_id>_<view>_<WxH>.png`) plus an `errors` array.

### Full-body pose verification loop

Use this while tuning body balance, heel contact, head tilt, and wave silhouettes:

```json
{
  "output_dir": "~/pose_captures",
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
  "output_dir": "~/pose_captures",
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
3. **Adjust**: small **`adjust_bone`** quaternion deltas only (not degrees); one intent per pass (for example, heel contact or smile intensity).
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
