# VRM Pose Authoring Guide

Reference for MCP callers and humans. Poses live in **normalized pose space**: each bone’s value is a **unit quaternion** `pose_q = [x, y, z, w]` applied by `pose_driver` (see repo) so that `pose_q = [0,0,0,1]` leaves that bone at its VRM rest. This is **not** world space and **not** Euler angles unless you use the MCP tools below.

> **Always work through MCP tools, never around them.** If `CallMcpTool` fails ("Not connected" after a server restart, an unexpected schema mismatch, etc.), **stop and ask the user to refresh `user-pose-controller` in the Cursor MCP panel** — don't fall back to `curl` / Python / direct HTTP. Workarounds let real tooling gaps fester. Whenever you find yourself wishing for a "snapshot the live state and save it" or "build N layers in one call" helper, that's a missing tool — file it (e.g. `save_current_pose`, `set_layer_stack`) and use the new tool from the next session forward.

> **Capture is ground truth — describe what the camera shows, not what you intended.** Every `capture_pose_views` PNG must be read as evidence: enumerate concrete observations of the silhouette (`"hand near hip, elbow twisted outward, eyes closed"`) **before** comparing to the intent. If the silhouette does not match the intended shape, the pose is wrong regardless of how reasonable the recipe looked — **iterate and re-capture before presenting the pose for the user's review**. A self-critique that flags a structural problem ("scarecrow arms," "doesn't read as sofa") and then forwards the pose to the user anyway is a process failure: you should have fixed those before showing it. The user's role is taste and nuance, not "your right elbow is twisted." See [Self-corrective workflow (verify before "done")](#self-corrective-workflow-verify-before-done) for the full multi-view gate.

## MCP tools (use this order)

1. **`pose_bones`** — Preferred for **body and limbs**. You send **degrees** per bone: `pitch_deg`, `yaw_deg`, `roll_deg` (each optional; missing = 0). MCP argument **`bones` is a JSON object (map)** from bone name → `{ "pitch_deg"?, "yaw_deg"?, "roll_deg"? }` — **not** an array; a list shape will fail deserialization. The server converts with **intrinsic local Euler order XYZ** (pitch around local X, then Y, then Z), **clamps** each angle to safe per-bone limits, **normalizes** the quaternion, then optionally clamps xyz again. The tool response lists **warnings** whenever something was clamped or normalized.
2. **`make_fist`** — `amount` from 0 (relaxed curl template) to 1 (full fist). Defaults to both hands. Use this instead of typing many finger quaternions.
3. **`adjust_bone`** — **Not Euler degrees.** This tool adds `delta_x`, `delta_y`, `delta_z` to the bone’s **current pose quaternion x/y/z components**, then **renormalizes** the quaternion to unit length (the `w` component is scaled with the vector part so length stays 1). It does **not** multiply `current_pose_q * delta_q`. Use **very small** deltas on **one axis at a time** (often **±0.02 to ±0.05**) for tiny viewport fixes after `pose_bones` or after Kimodo playback. For anything larger than a micro-nudge, use **`pose_bones`** with `pitch_deg` / `yaw_deg` / `roll_deg` instead.
4. **`get_current_bone_state` → `set_bones`** — Round-trip path: `set_bones` accepts the same quaternions the snapshot returns. The server still **normalizes** and **clamps** xyz so broken pasted values do not explode the rig.
5. **`create_pose`** — Saves quaternions to disk; unknown bone keys are dropped; each quaternion is sanitized like `set_bones`.
6. **`save_current_pose`** — Snapshot the **live** rig directly into a saved pose; eliminates the `get_current_bone_state` → reconstruct `bones` map → `create_pose` round-trip. Pass `bones: ["leftShoulder", ..., "rightHand"]` to capture only an upper-body chain (foundation poses for layered idle).
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

### Upper arm: lateral “arms out” vs arms-behind (**yaw vs roll**)

- **Symptom (front view):** elbows aim **aft**, forearms tuck **behind** the torso, or the silhouette matches **arms-behind** when you wanted a partial **T / soft fly**.
- **Cause:** On `leftUpperArm` / `rightUpperArm`, **`yaw_deg` twists about local Y (bone length)** — it is **not** “reach sideways.” Large ±`yaw_deg` there corkscrews the humerus and often sends the forearm chain **posterior** (the same axis family that **intentional** arms-behind poses use on this rig).
- **Fix:** Use **`roll_deg`** for the abduction / adduction–like spread (see the **roll** bullet in **Euler convention**). Add modest **`pitch_deg`** on the upper arm if you need a slight forward aim, and **negative `pitch_deg` on `*LowerArm`** for a soft elbow. Example correction: Comfy manifest **`c19_arms_out_soft`** was wrong while it used large **`yaw_deg`** on both upper arms; it should use **opposed `roll_deg`** instead.
- **Verify:** `capture_pose_views` with **`front`**, **`back`**, **`back_left`**, and **`back_right`** so “behind the back” mistakes show immediately.

### Crouch / squat / plié: avoid the “skyfall” (prone arch) silhouette

- **Symptom:** The body reads **horizontal / prone** (freefall, skydiver arch): **hips + spine** pitched forward as one plank while legs do not carry enough of the fold.
- **Cause:** Stacking **large `hips.pitch_deg`** with **large `spine` / `chest` / `upperChest` `pitch_deg`** rotates the root and column **together** forward. Knee-only tweaks do not fix the overall silhouette.
- **Fix:** Keep **`hips.pitch_deg` modest** for grounded squats (often **≤ ~8°** unless you deliberately want a strong athletic lean). Put most flexion in **`leftUpperLeg` / `rightUpperLeg`** and **`leftLowerLeg` / `rightLowerLeg`**, then **`leftFoot` / `rightFoot`**. Add **spine → chest → upperChest** in smaller steps **after** side/back captures show the **legs** folding, not “diving.” Use **`assets/poses/sit.json`** as a **rotation-only** reference: the seated shape is mostly **thigh–shin** with **relatively small** pelvis change vs an over-hip-pitched torso plank.
- **Comfy manifest:** **`c12_soft_plie`** and **`c24_mild_crouch`** were revised toward this split (see `captures/comfy_custom_pose_manifest.json` notes on each entry).

### Mirroring left and right

Do **not** mirror by negating quaternion Y and Z — that is unreliable once rest poses are non-trivial. Instead: build the right side with `pose_bones`, then repeat with **opposite signs on yaw and roll** (and sometimes pitch) on the left, **or** call `make_fist` / tune one side and mirror using small `adjust_bone` steps while watching the viewport.

### Building a custom arms-down rest pose (per VRM)

Different VRM exports ship in different bind poses (T-pose, A-pose, slight A-stance). For the layer-stack `pose_hold` foundation to look right, every VRM needs its own arms-down rest pose. The convention that works on **`airi.vrm`** (and most "T-pose" exports of the same family):

| Bone | `roll_deg` | `pitch_deg` | `yaw_deg` |
|------|------------|-------------|-----------|
| `leftUpperArm` | **−62** (max clamp) | +4–8 (slight forward) | +4 (slight inward) |
| `rightUpperArm` | **+62** (max clamp, mirror) | +4–8 | −4 |
| `leftLowerArm` | — | −8 to −10 (soft elbow flex) | −5 to −8 (hand bias inward) |
| `rightLowerArm` | — | −8 to −10 | +5 to +8 |
| `leftShoulder` / `rightShoulder` | ±3 to ±8 (small drop) | +2 (slight forward) | — |
| `leftHand` / `rightHand` | — | −3 (relaxed wrist) | — |

**Critical sign convention:** `*UpperArm.roll_deg` is **mirror-asymmetric** — `left = −62, right = +62` drops both arms; matching signs sends one up. The MCP server clamps `*UpperArm.roll_deg` to **±62°**, so you cannot drop arms further with roll alone — combine with shoulder roll and slight upper-arm pitch to tighten the silhouette against the body.

**Authoring loop (use the MCP tools, not Python):**

1. **`load_vrm`** → wait one frame → **`reset_pose`**. Confirm the rig is at its bind T-pose via `capture_pose_views(front)`.
2. **`pose_bones`** with the table above.
3. **`capture_pose_views`** with `front`, `front_left`, `left`. Read **MCP warnings** for clamps; if `*UpperArm.roll_deg` was clamped to ±62, that is expected.
4. Iterate small adjustments (1–4° at a time) on lower-arm pitch/yaw and shoulder roll until the side profile reads natural — arms hang beside the thigh line with a soft elbow.
5. **`save_current_pose`** `{ name: "<vrm_short>_natural_rest", category: "idle", bones: ["leftShoulder", "rightShoulder", "leftUpperArm", "rightUpperArm", "leftLowerArm", "rightLowerArm", "leftHand", "rightHand"] }`. Restricting `bones` to the upper-body chain means the foundation `pose_hold` layer won't freeze the legs / hips when overlaid in the layer stack.
6. Reference the saved pose from a `pose_hold` layer (see `LAYER_AUTHORING_GUIDE.md → Pose-hold layer foundation`).

**Anti-patterns (caught this session — don't repeat):**
- `leftUpperArm.roll_deg = +62, rightUpperArm.roll_deg = −62` — both arms go **up**, not down. Mirror sign matters.
- Asking for `roll_deg = −90`/`+90` and ignoring the clamp warning; the rig caps at ±62, the rest of the rotation silently disappears.
- Using `yaw_deg` on `*UpperArm` to "spread" arms — this **twists** the humerus around its length and often sends the forearm chain behind the body. Use `roll_deg` for lateral spread (see [Upper arm: lateral "arms out" vs arms-behind](#upper-arm-lateral-arms-out-vs-arms-behind-yaw-vs-roll)).

### Reclined / sitting / lying poses — `hips` rotates the whole body

**Critical lever for any reclined or sitting pose: `hips.pitch_deg` rotates the entire upper body around the pelvis.** Don't try to recline by pitching `spine` / `chest` / `upperChest` — that just bends the torso forward at the waist while the pelvis stays vertical (i.e. she still reads as "standing while bent over"). The pelvis must rotate first, then the spine compensates.

**Pattern that works (validated on `Belka1-mtoon.vrm`, generalizes to other MMD-style exports):**

| Bone | Value | Effect |
|------|-------|--------|
| `hips` | `pitch_deg: -20` | Entire upper body tilts BACKWARD (recline) |
| `spine` | `pitch_deg: +12` | Compensate forward so the head doesn't fall back too far |
| `chest` | `pitch_deg: +5` | Small additional forward bend, chin up but not skyward |
| `leftUpperLeg` / `rightUpperLeg` | `pitch_deg: +75` | Forward hip flexion — knees come up to chest level (the "sitting" lever) |
| `leftLowerLeg.roll_deg: +48` / `rightLowerLeg.roll_deg: -48` | (mirror signs) | Knee flex with calves dropping vertically |

Sign of `hips.pitch_deg`:
- **Negative** = body leans backward (recline)
- **Positive** = body leans forward (looking down, slumping, prayer pose)

**Anti-patterns caught (don't repeat):**

- **"Recline by pitching spine negative"** — bends torso back at waist while pelvis stays vertical. Reads as "standing with arched back," not "reclining." Use `hips.pitch_deg` instead.
- **"Sit by abducting one leg laterally with the other planted"** — reads as "standing with one leg out" because one foot is still anchored. To look seated, BOTH legs must be lifted forward (both `*UpperLeg.pitch_deg` strongly positive).
- **"Reclined sit with arms at chest"** — the "stretching / yawning / display" silhouette of an actual reclined lounge usually has both arms ABOVE the head, not at chest. If the user describes a "lounged" or "sprawled" pose, default both `*UpperArm.roll_deg` to mirror-±55–60 (overhead Y pose) before chest-level positions.

**Verification:** the right-side `capture_pose_views` tells you immediately whether the body is actually leaning back or just bending at the waist. If the spine line + the standing-leg line is still straight-vertical from hip to head, you're not reclining — you're bending. Re-pitch the hips, not the spine.

### Per-rig axis discovery (MMD/Japanese-bone exports differ from airi)

Different VRM exports rotate the bone-local axes differently relative to the world. The "elbow is `*LowerArm.pitch_deg`" / "knee is `*LowerLeg.pitch_deg`" conventions in this guide were calibrated against `airi.vrm`. Other rigs — particularly **MMD-style exports** with Japanese bone names (腕 = arm, ひじ = elbow, 足 = leg, ひざ = knee, often visible inside `extraBones` / parent chain) — bake those axes differently. **Don't assume; probe.**

**Fast probe protocol (do this once when you load a new VRM):**

1. `reset_pose`, then `pose_bones` with **one bone, one axis, one extreme value** (e.g. `rightLowerArm: { roll_deg: -55 }`).
2. `capture_pose_views(front, right)` and read what moved. Note which world-space motion that single rotation produced.
3. Repeat for the other two axes on the same bone, and for the other side.
4. Tabulate the result; reuse it for the rest of the session.

**Concrete example — the green-haired bat-winged "Biyoca Phantom Dreams" / Girls' Frontline 2-style rig in this workspace** (probed 2026-05-02; differs from `airi.vrm`):

| Bone | Axis | Effect on this rig |
|------|------|-------------------|
| `rightUpperArm` | `pitch_deg` | TWIST around humerus length (NOT forward sweep) |
| `rightUpperArm` | `yaw_deg` | Forward swing around vertical (positive = arm forward across body) |
| `rightUpperArm` | `roll_deg` | Drop arm from T toward forward-down (positive = down/forward) |
| `rightLowerArm` | `roll_deg` | Elbow flexion (negative = forearm rotates up/toward face) |
| `rightLowerArm` | `pitch_deg` | Mostly twist; weak flex contribution |
| `rightUpperLeg` | `roll_deg` | Lateral hip abduction (positive = thigh out to her right) |
| `rightUpperLeg` | `pitch_deg` | Hip flexion (positive = thigh forward/up) |
| `rightUpperLeg` | `yaw_deg` | Couples with twist + small forward/back; not clean abduction |
| `rightLowerLeg` | `roll_deg` | Knee flex such that **calf hangs DOWN** when thigh is laterally abducted (negative ≈ -50 = calf drops naturally below knee) |
| `rightLowerLeg` | `pitch_deg` | Knee flex such that **calf folds UP** toward thigh (positive = calf back up toward hip) |

Mirror-side bones use opposite signs on `roll_deg` and (often) `yaw_deg`; verify with the same probe protocol.

**Lessons baked into this:**

- The airi.vrm "elbow = `*LowerArm.pitch_deg`" rule is **NOT universal**. On MMD-style exports the elbow flex axis is often `roll_deg`. If `pitch_deg ±85` on the lower arm produces a barely-visible bend, switch to `roll_deg`.
- "Knee bends down vs up" depends on which axis you use on `*LowerLeg`. If your laterally-abducted thigh produces a calf that folds back up onto the thigh (foot above knee), flip from `pitch_deg` to `roll_deg` (or vice versa) for the knee — they bend in different planes after the upper-leg's lateral roll.
- For lateral hip abduction on this rig family, `roll_deg` on `*UpperLeg` is the right axis. `yaw_deg` on `*UpperLeg` couples with hip flexion in a confusing way (sign depends on `pitch_deg`); avoid it for clean abduction.
- Build a mental table from the probe captures **before** trying to compose a complex pose. Two minutes of probing saves 20 minutes of trial-and-error.

### Lounging / reclined arms — there is no "behind head"

**The rig will not let you put a hand behind the head.** `*UpperArm.pitch_deg` is **clamped at ±90°** (warning: `pitch_deg 95.0 clamped to ±90.0`). Combined with the `*UpperArm.roll_deg ±62°` clamp and the `*LowerArm` xyz cap (~0.68, ≈ ±85° elbow flex), the practical reachable envelope for a hand peaks **at shoulder height**, not above it. Stop fighting the rig and design lounging poses where the arms are at or below shoulder height.

**Recipes that read as "lounged" without overhead reach (validated on `airi.vrm`):**

| Intent | UpperArm | LowerArm | Hand |
|--------|----------|----------|------|
| **Forward-and-down soft drape** (right arm) | `pitch_deg +55, roll_deg +25, yaw_deg 0` | `pitch_deg −65, yaw_deg 0, roll_deg −8` | `pitch_deg −15, roll_deg −8` |
| **Side-and-down lazy reach** (left arm, mirrored) | `pitch_deg +25, roll_deg −45, yaw_deg 0` | `pitch_deg −55, yaw_deg 0, roll_deg +8` | `pitch_deg −10, roll_deg +12` |
| **"Hand near temple" pseudo-overhead** (closest you can get) | `pitch_deg +88, roll_deg +35, yaw_deg 0` | `pitch_deg −85, yaw_deg 0, roll_deg 0` | `pitch_deg −8, roll_deg −4` |

The third row is the maximum overhead reach the rig allows — the elbow ends up roughly at shoulder height with the forearm angled across, hand near the cheek. There is **no recipe** that reads as "hand cradling the back of the head."

**Forearm direction lever:** With `*UpperArm.yaw_deg` locked at `0` (don't twist the humerus), the only way to bias the forearm direction after elbow flex is `*UpperArm.pitch_deg` (lifts forward) and `*UpperArm.roll_deg` (sweeps lateral). Once the elbow flexes via negative `*LowerArm.pitch_deg`, the forearm trails along the same plane the upper arm is rotated in. Plan the hand landing zone by aiming the upper arm there with pitch+roll, then flex the elbow.

### Capture framing for non-standing poses

`framing_preset: "face_closeup"` assumes the avatar's face is **near where it would be in a standing T-pose** (centered, upright, ~head-height). For a reclined / sitting / floor pose, the head is in a totally different world position and `face_closeup` shoots **empty space** (you'll get a blank PNG and a wasted round-trip).

**Workaround:** Use `framing_preset: "full_body"` with a square `width: 768, height: 768` and read the resulting image — the head will be visible inside it. Alternatively, capture full_body at 1024×1024 for both pose and face verification in one shot. There is no head-tracking framing preset today; consider adding one (`face_closeup_dynamic` that reads the head bone's world transform) when this becomes a frequent need.

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

**Knee direction (MMD-style rigs):** knee flex is often documented as **negative** `pitch_deg` on `*LowerLeg` (shin relative to thigh). On **some** exports—including the repo’s default **`airi.vrm`** used for Comfy reference captures—that convention **hyperextends the knee backward** (the crease faces posterior while the shin should sit anterior). **Fix:** flip to **positive** `pitch_deg` on **both** `leftLowerLeg` and `rightLowerLeg`, and drive the thigh with **positive** `pitch_deg` on `*UpperLeg` for a shallow squat/plie before adding more knee. Tune in **±4–8°** steps and always **look at profile PNGs** after `capture_pose_views`.

**How to verify (mandatory for leg-heavy poses):** use `capture_pose_views` with at least **`left`**, **`right`**, **`back`**, **`back_left`**, and **`back_right`** (see MCP tool; `rear` is an alias for `back`). A correct forward knee bend shows the patella region facing **forward** in the side views and the shins **in front of** the thigh line—not a concave “inverted” knee.

**Torso vs head:** put most of the forward lean on **spine → chest → upperChest** (moderate positive pitch on each, parent-to-child). Keep **neck** and **head** closer to neutral (small angles) so the gaze stays forward instead of staring at the floor.

**Arms on knees:** after the legs read as a sit, add **shoulder / upperArm** outward rotation and **lowerArm** flex so forearms meet the thighs — tune in small steps and read MCP warnings.

### Forward leg raise (knee + thigh direction)

- **Symptom → fix (leg behind body):** If the raised leg extends **behind** the torso instead of **in front**, the primary lever is usually **`leftUpperLeg` / `rightUpperLeg`** — try **flipping the sign on `pitch_deg` first** (often +↔−), then adjust **`yaw_deg` in ±10–20°** steps. Do **not** only crank `*LowerLeg` when the thigh aim is wrong.
- **Knee bend wrong way:** On MMD-style rigs, knee flex is usually **negative `pitch_deg` on `*LowerLeg`**; if the knee collapses backward, **flip the sign on `*LowerLeg` `pitch_deg`** before touching the foot.
- **Order of operations:** (a) thigh aim forward, (b) slight `*LowerLeg` bend, (c) `*Foot` — toe forward, (d) `*Toes` then `DEF-toe_*` for fan — small steps; read MCP **`warnings`** for clamps.
- **Verification:** `capture_pose_views` with **`framing_preset: "full_body"`** and at least **left**, **right**, **back**, **back_left**, and **back_right** to catch “leg behind” and **backward-knee** mistakes early. For the full multi-view **done** gate (front/sides/back 3/4), see [**Self-corrective workflow (verify before “done”)**](#self-corrective-workflow-verify-before-done).
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
6. **Large `yaw_deg` on `*UpperArm` for “arms out”** — often reads **arms-behind**; use **`roll_deg`** for lateral spread (see **Upper arm: lateral “arms out” vs arms-behind (yaw vs roll)** above).
7. **Deep squat from hips+spine pitch only** — reads **prone / skyfall**; fold **thigh–shin** first and cap **`hips.pitch_deg`** (see **Crouch / squat / plié: avoid the “skyfall” (prone arch) silhouette** above).
8. **Matching `roll_deg` signs on `*UpperArm` for arms-down** — drops one arm and lifts the other. Mirror the sign: `leftUpperArm.roll_deg = -62`, `rightUpperArm.roll_deg = +62`. See [**Building a custom arms-down rest pose (per VRM)**](#building-a-custom-arms-down-rest-pose-per-vrm).
9. **Reusing a saved pose from one VRM as the foundation for another** — `hands_on_hips` authored on `Belka1-mtoon.vrm` may leave `airi.vrm` arms in T-pose because the rigs' bind orientations differ. Author per-VRM rest poses (`<vrm_short>_natural_rest`) and reference those.
10. **Falling back to Python / curl when an MCP call fails.** That's a tooling-gap signal — fix the missing batch tool (e.g. `save_current_pose` instead of `get_current_bone_state` + `create_pose`) and ask the user to refresh the MCP server connection before retrying.

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

**Bind rebase (was the cause of "VRMA clip lays the avatar flat" bugs):** VRMA channel rotations are **absolute** glTF node rotations in the VRMA file's own bind frame; per glTF spec they REPLACE `node.rotation` rather than multiplying it. Many VRMA exporters (e.g. the `vrm-studio` collection-1 pack) bake non-identity bind rotations into humanoid nodes — the `VRMA_07_Squat.vrma` hip node has bind ≈ 120° around (1,1,1) (a coordinate-axis cycle) and `*UpperLeg` bind ≈ 180°. The loaded VRM expects the **normalized humanoid pose quaternion** (delta from bind), so the converter must do `bind_q.inverse() * channel_q` per frame for every humanoid bone — without that rebase, frame 0 gets a ~113° hip rotation and the avatar lies horizontal at clip start. The current converter does the rebase; if you regenerate JSON with an older copy of the script, expect the "lying on the floor" symptom and re-run the latest. The same rebase keeps fingers from corkscrewing — finger nodes often have small non-identity bind rotations (≈10–15° per knuckle) that, if not removed, stack with the channel data and look like jitter.

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

**After material body changes** — `pose_bones`, leg chains, `make_fist`, Kimodo apply, hub leg/pose applies, or similar — always run **`capture_pose_views`** with **`framing_preset: "full_body"`** and `views` including at least **`front`**, **`left`**, **`right`**, **`back`**, **`back_left`**, and **`back_right`**. For shoulders, arms, and torso silhouette, add **`front_left`** and **`front_right`**. **Arms:** if every shot reads as a rigid **T-pose**, add **moderate negative `pitch_deg` on `*LowerArm`** (and/or tune **`roll_deg`** on `*UpperArm` for lateral spread — **do not** use large **`yaw_deg`** on upper arms for “wingspan”; see **Upper arm: lateral “arms out” vs arms-behind (yaw vs roll)** above) before re-capturing.

**Capture vs. main viewport:** offscreen capture cameras aim at the **loaded VRM root’s world position** (after transform propagation), not the orbit gameplay camera — you still get valid transparent PNGs if she is panned off-screen, as long as the model stays in the scene.

**After face work** — `set_expression`, `animate_expressions`, or expression-heavy poses — use **`framing_preset: "face_closeup"`** with **`front`**, **`front_left`**, **`front_right`**.

**Iteration:** adjust only what **failed** the last captures — small **`adjust_bone`** (quaternion deltas) or **targeted** `pose_bones` / `set_expression` on those bones or weights — then **re-capture** with the same framing and view set. Repeat until acceptable or you hit diminishing returns (avoid re-driving unrelated bones).

**Read the captures as evidence (mandatory two-step):**

1. **Describe what's actually in the picture** — limb directions, joint angles, eye/face state, body orientation, contact with imagined surfaces — using language that does **not** reference the recipe you typed. Example: `"front view: both arms extend horizontally outward parallel to the floor; eyes appear closed; right leg points up and to camera-left; no contact with imagined chair surface"`.
2. **Then** compare that description against the intent. If anything is structurally wrong — limb in the wrong direction, eyes wrong, body floating instead of sitting, twisted joint — that is a **real defect** the user can see at a glance, **not** a nuance for them to direct. Iterate (`pose_bones` adjustment + re-capture) **before** presenting the pose for review.

The single most common authoring failure is **recipe-anchoring**: trusting that the silhouette matches what the recipe says, instead of trusting the picture. Cover the recipe in your head and ask "would a stranger looking only at this PNG describe it the way I'd describe my intent?" If no, fix it before showing it.

**Completion gate:** do **not** declare the task finished until captures are **reviewed** (humans: open the PNGs; **agents: read the image files** at paths returned in the MCP response). Skipping review is an incomplete run; reviewing without describing-then-comparing is incomplete review.

Concrete payloads and camera overrides: **Visual verification loop** below.

## Visual verification loop (recommended)

1. Apply a coarse body pose with `pose_bones` (and `make_fist` if needed).
2. Capture validation images with `capture_pose_views`:
   - `output_dir` required.
   - Use explicit `views`: `["front","left","right","front_left","front_right","back","back_left","back_right"]`.
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

## Animation layer stack (layered motion)

For **stacked** motion — procedural idle polish (breathing, blink, fidgets) **plus** optional library **`clip`** or **`pose_hold`** layers with bone masks — read **`assets/LAYER_AUTHORING_GUIDE.md`** or call MCP **`get_layer_authoring_guide`**. Workflow tools include **`list_layers`**, **`add_layer`**, **`install_default_layers`**, **`set_master_enabled`**, **`save_layer_set`** / **`load_layer_set`**, then **`capture_pose_views`** to verify.
