# VRM pose authoring (MCP)

Poses use **normalized pose quaternions** on each bone: identity = VRM bind. Reach for the **semantic intent tools** before raw bone manipulation — they exist precisely because raw `pose_bones` is the most common failure mode.

> **Use MCP tools only.** If a call fails after a server restart, ask the user to **refresh the pose MCP** connection in their client. Do not substitute shell scripts or ad-hoc HTTP for missing tools — that hides real gaps.

> **Captures are ground truth.** After body or face changes, call **`capture_pose_views`** and **look at the images in the tool response** (inline `image/png` when embedding is on — default). Describe what you **see** before declaring success. Do not trust numeric recipes alone.

---

## Tool ladder (try in order)

1. **Semantic intents** — `raise_leg`, `bend_knee`, `arms_down_rest`, `make_fist`. Pick these first for any "lift / bend / drop arms / fist" intent. They compile to **bounded** Euler maps and almost never break the rig.
2. **Library poses** — `apply_pose` after `list_poses` for known baselines.
3. **Raw `pose_bones`** (Euler degrees) — only when no semantic tool fits. Pass `dry_run: true` first if you are unsure of an axis or sign.
4. **Raw `set_bones`** (quaternions) — last resort.

---

## Hybrid safety policy (applies to `pose_bones` and `set_bones`)

Both raw bone tools enforce a **hybrid** policy:

- **Catastrophic** (many bones at near-axis limits in one call) → **hard fail**, always.
- **Severe** (any single axis ≥ 80°) → **hard fail** unless you set `allow_large_angles: true`.
- **Warn** (single bone close to its per-axis limit) → returned in the response; `strict: true` escalates these to hard fails.
- **`preserve_omitted_bones: false`** → **rejected** on both tools (it resets every unlisted bone to bind/identity and almost always destroys the rig).
- **`dry_run: true`** → runs validation + sanitize and returns a `{ severity, sanitizedRotations, warnings, wouldApplyBones, verificationHint }` summary without dispatching anything.

The capture tool also enforces a **view coverage** policy: front-only captures get a `viewCoverageWarning` because knee direction, elbow inversion, and foot crossover are invisible from the front.

---

## Tool cheat sheet

| Goal | Tool |
|------|------|
| Lift / flex one leg from the hip | **`raise_leg`** — `side`, `amount` 0..1, optional `direction: "forward" | "outward"` |
| Bend one knee | **`bend_knee`** — `side`, `amount` 0..1 |
| Drop both arms to a natural rest | **`arms_down_rest`** — optional `amount` (default 0.85) |
| Hands | **`make_fist`** — `amount` 0..1 |
| Body / limbs in local Euler degrees (raw) | **`pose_bones`** — top-level **`bones`** object: bone name → `{ pitch_deg?, yaw_deg?, roll_deg? }`. Optional `strict`, `allow_large_angles`, `dry_run`. |
| Tiny quaternion nudges | **`adjust_bone`** — small `delta_x/y/z` on quaternion components only |
| Face partial blend | **`set_expression`** — requires top-level **`expressions`** map |
| Face replace-all | **`set_expressions_full`** |
| Short expression curves | **`animate_expressions`** |
| Verify silhouette | **`capture_pose_views`** — needs **`capture_id`** + **`views`**; **`output_dir` optional**. Prefer **`framing_preset`: `full_body`** or **`face_closeup`**. |
| Lists / discovery | **`get_bone_reference`**, **`list_expressions`**, **`get_pose_guide`** (this file) |

---

## Semantic intents — `raise_leg`, `bend_knee`, `arms_down_rest`

Each compiles to a tiny, bounded Euler map and goes through the same sanitize / safety pipeline as `pose_bones`. They never push beyond ~70° on the dominant axis (well below per-bone clamp limits), so they cannot produce catastrophic warnings.

**`raise_leg`** — `{ side: "left" | "right", amount: 0..=1, direction?: "forward" | "outward", dry_run?: bool }`. Default `direction` is `forward` (hip flex — knee comes forward and up). The compiled upper-leg pitch **sign comes from per-VRM calibration**, so `forward` flexes the hip forward even on rigs where raw positive pitch extends the thigh backward (see the calibration note + worked example below). `outward` uses mirrored `roll_deg` for clean hip abduction (avoids the thigh-yaw trap below).

**`bend_knee`** — `{ side, amount, dry_run? }`. Bends the named lower leg via positive `pitch_deg` (the airi-family safe flex direction — no backward hyperextension).

**`arms_down_rest`** — `{ amount?: 0..=1, dry_run? }` (default 0.85). Mirror-symmetric: `leftUpperArm.roll_deg = -k`, `rightUpperArm.roll_deg = +k`, plus a soft elbow pitch and mild shoulder lift on both sides.

**Per-VRM calibration:** bind pose / bone roll in the `.vrm` changes which way “positive pitch” points. The shipped defaults match airi-style rigs; if **`raise_leg` forward** moves the thigh the wrong way on your export, open the in-app **Pose Controller → Intent Lab** tab, flip the **forward pitch** sign (or dial the slider negative), **Save for this VRM**, then retry MCP — calibration files live under `config/semantic_intent_calibration/<key>.toml` (same hex key scheme as spring presets). MCP semantic tools load those signs automatically.

**Per-VRM calibration — probing axes.** Bind-pose roll and bone orientation differ per export, so axis signs are **not portable across rigs**. Probe on each new VRM before composing any leg pose:

1. Call **`set_master_enabled false`** to isolate the rig from procedural layers.
2. Apply **`pose_bones`** with a single bone, single axis, moderate value (e.g. `leftUpperLeg roll_deg +20`).
3. **`capture_pose_views`** — front **and** back — and note the world-space result (does the thigh fan out or cross inward?).
4. Repeat for each axis of interest. Build a local sign table and store it in a **VRM-specific notes file**, not in this guide.

**Key probe targets (most rigs):** `*UpperLeg.pitch_deg` (forward vs backward hip flex), `*UpperLeg.roll_deg` (abduction vs adduction direction), `*LowerLeg.pitch_deg` (knee flex direction), `*Foot.yaw_deg` (toe turnout direction), `hips.pitch_deg` (torso bow forward vs back).

**Calibration TOML:** if **`raise_leg forward`** moves the thigh the wrong way, edit `config/semantic_intent_calibration/<vrm_key>.toml` → `raise_leg_forward_pitch_sign = -1.0`, then **restart the pose MCP** (calibration loads at server start). Use the **Intent Lab** tab in-app to flip signs interactively.

**Widen stance:** prefer **`raise_leg direction: “outward”`** for hip abduction — it compiles to **`roll_deg`** (not yaw) and avoids the yaw-couples-flex trap. Calibrate the outward-roll sign in Intent Lab if needed.

**Turnout trap (grounded wide stance):** combining upper-leg `yaw_deg` (hip turnout) with `roll_deg` (abduction) in one Euler triple can produce an unwanted **forward kick** (yaw reorients the roll axis). For a grounded wide stance, consider getting turnout from the **feet** (`*Foot.yaw_deg`) rather than hip yaw — then verify with a side capture.

**Knee-on-abducted-thigh trap:** flexing the knee (`*LowerLeg.pitch_deg`) while the thigh is abducted (rolled out) may swing the shin **forward** instead of straight down (roll reorients the knee hinge). Add hip turnout (`*UpperLeg.yaw_deg`, mirrored per side) so the knee tracks over the toes, or compensate with a small backward thigh pitch adjustment.

> **⚠ `pose_bones` Euler and saved-pose / `pose_hold` / clip quaternions are the same space** — no conversion needed between them. However, which roll sign abducts vs adducts the thigh is **rig-dependent** (bind-pose roll varies per export). Always verify front **and** back after any leg-abduction pose — leg crossing and backward knees are invisible from a single front view. **Safest authoring for complex lower-body poses = no sign reasoning:** SLERP between *captured, visually-verified* poses using `tools/author_pose_clip.py`.

All three return a `compiledEuler` map so you can inspect (or copy/tweak) the underlying values. Use `dry_run: true` when you want the map without applying it.

After a semantic intent, **always** capture `left`, `right`, and `back` to confirm the silhouette.

---

## `pose_bones` Euler convention

Intrinsic order **`XYZ`**: **pitch** → **yaw** → **roll** (degrees). Each axis optional; omitted = 0.

Summary labels (local axes — **per-rig interpretation differs**; probe below):

- **pitch** — often flex / extend for limb segments  
- **yaw** — often twist along bone length  
- **roll** — often “spread”-like motion **depending on bone** (see arms vs legs)

The server **clamps** per bone and returns **`warnings`** — read them.

### Upper arm: “arms out” vs arms-behind

On **`leftUpperArm` / `rightUpperArm`**, large **`yaw_deg`** twists the humerus and often sends forearms **behind** the torso. For a lateral “arms out” / soft fly, prefer **`roll_deg`** for sweep and modest **`pitch_deg`**, not huge yaw. Verify with **`front`**, **`back`**, **`back_left`**, **`back_right`**.

### Arms-down rest (quick reference)

Many exports need **mirror-opposite `roll_deg`** on **`leftUpperArm` / `rightUpperArm`** to drop both arms (e.g. left **−62**, right **+62** on rigs that cap near ±62). **Matching signs** often raises one arm. Add **`leftLowerArm` / `rightLowerArm`** negative **`pitch_deg`** for a soft elbow. Author per-VRM; use **`save_current_pose`** with a **`bones`** filter for upper-body-only rest poses when layering.

### Upper leg: wide stance / abduction (typical airi-style export)

Rigs differ. On many exports in the **airi / MMD family**, **lateral thigh motion** is driven with **`roll_deg` on `*UpperLeg`**, while **`yaw_deg` on the thigh** couples with flex and is easy to get wrong. **Probe** (one bone, one axis, one value) on a new VRM before composing big poses.

**Fast probe:** `reset_pose`, then **`pose_bones`** with **one** bone and **one** of `pitch_deg` / `yaw_deg` / `roll_deg` at a moderate value, **`capture_pose_views`** (`left`, `right` or `front`), note world-space motion. Repeat for each axis. Build a tiny mental table before layering rotations.

**Reference table (one probed MMD-style rig — yours may differ):**

| Bone | Useful axis | Typical visual |
|------|----------------|----------------|
| `rightUpperArm` | `roll_deg` | Drop / sweep arm laterally |
| `rightUpperArm` | `yaw_deg` | Forward swing — easy to confuse with “spread” |
| `rightUpperLeg` | `roll_deg` | Lateral hip motion (sign + mirror for left) |
| `rightUpperLeg` | `pitch_deg` | Hip flex / thigh forward |
| `rightLowerLeg` | `pitch_deg` or `roll_deg` | Knee — **which axis reads “bend” depends on export**; flip sign if knee inverts |

Mirror left/right with **opposite signs** on paired limbs where appropriate; **DEF-toe*** digits may **not** mirror like humanoid limbs.

---

## Leg-heavy poses

- **Verification views:** at least **`left`**, **`right`**, **`back`**, **`back_left`**, **`back_right`** (`rear` = `back`). Front-only misses crossed legs and backward knees.  
- **Knee direction:** if the knee reads backward on **`airi.vrm`**, try **flipping the sign** of **`*LowerLeg.pitch_deg`** in small steps (see also knee notes in older sessions — **profile captures decide**).  
- **”Leg behind body”:** fix **`*UpperLeg`** aim (`pitch` / `yaw` in small steps) before only bending **`*LowerLeg`**. Note some rigs **invert** the forward/back pitch sign — probe the axis and check the calibration TOML (see *Per-VRM calibration* above).

---

## Seated / recline / floor (short)

- Recline: prefer **`hips.pitch_deg`** to tilt the whole upper body, not only spine.  
- Sit: both thighs usually need strong **forward** flex (`*UpperLeg.pitch_deg` positive) for a true sit, not one leg abducted while the other plants.  
- **Face closeup** framing assumes a standing head height; for floor/recline, use **`full_body`** and a square size, or the head may leave the frame.

---

## Extra skin bones (`DEF-toe*`, `DEF-ero*`, …)

If **`get_bone_reference` → `extraBones`** lists them, **`pose_bones`** accepts the same names. **Per-digit toes** are not simple left/right mirrors; tune one side, copy patterns carefully, keep **`yaw_deg`** near 0 unless you want twist along the toe bone. For deep toe work, use the in-app **Bones** tab or **`get_current_bone_state`**.

---

## `set_bones` / JSON quats (only if needed)

Quaternions must be **unit** length; server renormalizes. **Prefer `pose_bones`** for new work.

---

## Face

Use **`list_expressions`** for valid preset names. **`set_expression`** requires **`expressions`**: `{ "happy": 0.6, ... }`.  
**`animate_expressions`:** `keyframes: [{ "time_s", "weights" }, ...]`, optional `duration_seconds`, `looping`. Linear segments only.

---

## `capture_pose_views` (agents)

**Minimum:** `capture_id`, `views` (array of slugs: `front`, `left`, `right`, `front_left`, `front_right`, `back`, `back_left`, `back_right`).

**`output_dir`:** optional. If omitted, the host writes under a default folder (`pose_captures` relative to the app). **You do not need disk access** to review: default **`embed_images`** returns **inline PNGs** in the tool result. Use paths only if the user asked for files on disk.

**Framing:** `full_body` for body; `face_closeup` for expression QA.

**`settle_before_capture_ms`:** default 120 — allows the previous `pose_bones` to apply; use `0` if you need an instant grab.

---

## Iterative loop (closed loop)

1. **`reset_pose`** or **`apply_pose`** baseline.
2. **Semantic intent** first (`raise_leg` / `bend_knee` / `arms_down_rest` / `make_fist`) — fall back to **`pose_bones`** with `dry_run: true` only when no intent fits.
3. **`set_expression`** for face changes (always after bones).
4. **`capture_pose_views`** with `framing_preset` and enough **views** (always include `left`, `right`, and add `back` for body or upper-body changes).
5. **Inspect embedded images** — if wrong, adjust **only what failed**, re-capture. Read the response `warnings` and `safety` block; if `severity` is `severe` or `catastrophic`, ease off the angles instead of forcing `allow_large_angles`.
6. Persist with **`save_current_pose`** (requires **`name`**) or **`create_pose`** when ready.

Do **not** mark done until captures **look** right, not until JSON says success.

---

## Kimodo / motion generation

**`generate_motion`** writes clips via the Kimodo peer; if listing fails, the **human** should align Kimodo’s working directory / env with the avatar host — not something an agent on another machine can fix blindly.

---

## Layer stack (idle + clips + masks)

See **`get_layer_authoring_guide`** or **`assets/LAYER_AUTHORING_GUIDE.md`**.

---

## Common mistakes

1. **Reaching for `pose_bones` first.** Try the semantic intents (`raise_leg`, `bend_knee`, `arms_down_rest`, `make_fist`). They compile a guaranteed-safe map for the most common requests. Only fall back to raw `pose_bones` when the intent does not exist.
2. **`preserve_omitted_bones: false`** on `pose_bones` or `set_bones` — **rejected by MCP**. It resets every unlisted bone to bind/identity. Use **`reset_pose`** first if you need a clean slate.
3. **Catastrophic / severe pose calls** — many bones at near-axis limits, or any single axis ≥ 80°. The hybrid policy will hard-fail. Author the pose in 2–3 phases (torso → limbs → face) with `capture_pose_views` between each apply, or use `dry_run: true` to inspect compiled angles first.
4. **`bones` not an object** — deserialization fails.
5. **`set_expression` without `expressions`** — fails.
6. **`save_current_pose` without `name`** — fails.
7. **`capture_pose_views` missing `capture_id` or `views`** — fails. **`output_dir` is optional.** **Front-only captures emit a `viewCoverageWarning`** — always include `left` / `right` and add `back` for upper-body changes.
8. Large **`yaw_deg` on upper arms** for "spread" — often arms-behind; use **`roll_deg`** per guide above. Huge **thigh `yaw_deg`** — often couples into twisted legs; probe and use small steps.
9. Ignoring **MCP warnings** after clamping (strict mode escalates these to hard fails).
10. **All morphs at 1.0 + extreme bones** — mesh explosion; tune skeleton first, then expressions in a second call.

---

## Offline VRMA → JSON (operators only)

The repo may ship a **Python** converter under `scripts/` for **local** VRMA-to-library-JSON conversion. That is **not** an MCP step and **not** for remote agents. Operators run it on a machine that has the repo and Blender/export toolchain if needed.
