# Animation layer authoring (MCP)

The in-engine **`LayerStack`** composes multiple **drivers** each frame into `ApplyBones` / `ApplyExpression` after VRMA / animation sampling. Use MCP tools: **`list_layers`**, **`set_master_enabled`**, **`add_layer`**, **`set_layer_stack`** (batch), **`update_layer`**, **`remove_layer`**, **`clear_layers`**, **`install_default_layers`**, **`list_layer_sets`**, **`save_layer_set`**, **`load_layer_set`**, **`delete_layer_set`**.

> **Always prefer MCP tool calls over external HTTP / Python scripts.** If `CallMcpTool` returns "Not connected" right after a binary restart, ask the user to **refresh `user-pose-controller`** in the Cursor MCP panel; the client doesn't auto-reconnect when the server's session id changes. Reaching for `curl` / Python masks tooling gaps that should be fixed in the server itself — when you find one, file it as a `set_*` / `save_*` batch tool instead of working around it.

> **Capture is ground truth — don't describe what you intended, describe what the camera shows.** When you read a `capture_pose_views` PNG, force yourself to enumerate concrete observations (`"both arms extend horizontally outward, eyes are closed, neither leg touches the other"`) **before** comparing to intent. If the silhouette doesn't read as the intended shape (sofa pose, lounge, wave, …), the pose is wrong **regardless of how plausible the recipe looked** — iterate and re-capture **before** presenting it for the user's review. The user's input is for taste and nuance, not "the elbow is twisted." Surfacing a structural problem in your own self-critique and then handing it to the user anyway is a process failure, not a successful review.

## Blend modes

| MCP `blend_mode` | Internal | Use for |
|------------------|----------|---------|
| `override` (default) | `BlendMode::Override` | **Absolute** local rotations — clips (`kind: clip`), pose holds, blink (expressions). |
| `additive` or `rest_relative` | `BlendMode::RestRelative` | **Deltas** vs bind/rest — breathing, weight-shift, finger/toe fidgets riding on top of earlier layers. |

Rule of thumb: **gesture / clip / face** → override. **Micro sway / breath / fidget** → additive.

## Driver kinds (`add_layer.driver`)

JSON **`driver`** is a **tagged union** (`kind` + fields):

| `kind` | Fields | Notes |
|--------|--------|-------|
| `clip` | `filename` | Use **`list_generated_animations`** / filenames from disk (e.g. `wave.json`). Resolution tries filename then display name. |
| `pose_hold` | `pose_ref` | Pose slug file or display name (see **`list_poses`**). |
| `breathing` | optional `rate_hz`, `pitch_deg`, `roll_deg` | Defaults match built-in preset if omitted. Use **`additive`** blend. |
| `blink` | optional `mean_interval`, `double_blink_chance` | Expression-only; default **`override`**. |
| `weight_shift` | optional `rate_hz`, `hip_roll_deg`, `spine_counter_deg` | Additive. |
| `finger_fidget` | optional `amplitude_deg`, `frequency_hz`, `seed` | Additive; random **`seed`** if omitted. |
| `toe_fidget` | optional `amplitude_deg`, `frequency_hz`, `seed` | Additive. |

Changing **clip** vs **pose_hold** on an existing layer is **not** supported — **`remove_layer`** then **`add_layer`**.

## Bone masks (`mask_include` / `mask_exclude`)

- Both empty → layer may touch **all** bones (still subject to blend order).
- **`mask_include`** non-empty → only listed bone names (VRM humanoid names, e.g. `rightUpperArm`, `rightLowerArm`, `rightHand`).
- **`mask_exclude`** → subtract from the effective set.

### Recipes

| Goal | Suggestion |
|------|------------|
| Wave with **right arm only** | `blend_mode: override`, `mask_include: ["rightUpperArm","rightLowerArm","rightHand"]` |
| Torso emphasis | Include `hips`, `spine`, `chest`, `upperChest` |
| Keep face on clip | Exclude eye/ mouth bones if the clip encodes them; often pair clip with separate **`set_expression`** / **`animate_expressions`** instead |

Use **`get_bone_reference`** for the live rig’s names.

## Persistence (`config/anim_layer_sets.json`)

- **`save_layer_set`** `{ name, persist? }` — snapshot current stack; **`persist: true`** (default) writes JSON.
- **`load_layer_set`** `{ name }` — replaces the live stack; clips/pose layers reload from **`[pose_library]`** paths.
- **`delete_layer_set`** — removes a named set from the store (+ optional disk write).

Layer-set files store **references** to animations/poses, not embedded keyframes.

## Recommended workflow

1. **`list_layers`** — confirm **`masterEnabled`** (use **`set_master_enabled`** if needed).
2. Optional baseline: **`install_default_layers`** (five procedural layers; clears existing layers first).
3. Add motion: **`generate_motion`** or pick a saved clip → **`add_layer`** with `kind: clip` + **`mask_include`** as needed.
4. **`capture_pose_views`** (`full_body` / `face_closeup`) after each substantive change.
5. **`save_layer_set`** `{ name: "idle_plus_wave_v1", persist: true }`.

### Batch authoring (preferred for multi-layer presets)

For anything beyond a single ad-hoc layer add, use **`set_layer_stack`** instead of chaining `clear_layers` + N×`add_layer`. The batch tool is atomic (you can't end up with a half-built stack on validation failure), avoids the round-trip latency of N MCP calls, and accepts an inline `save_as` so the new preset is persisted in the same call.

```jsonc
// set_layer_stack — clears, builds, optionally saves in one call
{
  "layers": [
    { "slug": "pose-foundation", "label": "Pose Foundation",
      "driver": { "kind": "pose_hold", "pose_ref": "airi_natural_rest" },
      "blend_mode": "override", "weight": 1.0 },
    { "slug": "breathing",
      "driver": { "kind": "breathing", "pitch_deg": 0.9, "rate_hz": 0.25 },
      "blend_mode": "additive", "weight": 1.0, "looping": true },
    { "slug": "auto-blink",
      "driver": { "kind": "blink", "mean_interval": 4.0 },
      "blend_mode": "override", "weight": 1.0, "looping": true },
    { "slug": "idle-head-wander",
      "driver": { "kind": "clip", "filename": "idle_look_around.json" },
      "blend_mode": "additive", "weight": 0.25, "looping": true, "speed": 0.7,
      "mask_include": ["head", "neck"] }
  ],
  "master_enabled": true,
  "save_as": "jarvis_natural_idle",
  "persist": true
}
```

### Pose-hold layer foundation (custom rest pose per VRM)

The `pose_hold` driver overlays a saved pose at full weight every frame, so it's the natural way to give every VRM a sane "arms relaxed at sides" foundation under the procedural layers. Authoring loop:

1. **`load_vrm`** + wait one frame, then **`reset_pose`** to start from the rig's bind.
2. **`pose_bones`** to sculpt arms-down (see [POSE_GUIDE → Building a custom arms-down rest pose](./POSE_GUIDE.md#building-a-custom-arms-down-rest-pose-per-vrm) for the mirror sign convention).
3. **`capture_pose_views`** with `front`, `left`, `front_left` to verify the silhouette.
4. **`save_current_pose`** `{ name: "<vrm>_natural_rest", bones: ["leftShoulder","rightShoulder","leftUpperArm","rightUpperArm","leftLowerArm","rightLowerArm","leftHand","rightHand"], category: "idle" }` — only the arm chain, so the foundation doesn't freeze legs / hips when overlaid.
5. **`set_layer_stack`** with `pose_hold(<vrm>_natural_rest)` as the bottom override layer + procedural deltas on top.

> **Coordinate space:** `save_current_pose` (and the older `get_current_bone_state` → `create_pose` round trip) stores rotations in normalized humanoid space. `pose_hold` and clip layers convert this to raw bone-local before composing. If you load a layer-set from before 2026-05-02 and the rig snaps to bind, that pre-dates the conversion fix in `sample_pose_hold` / `sample_clip` — re-save the pose and re-load the set.

### Preset switching for conversation states

Build a small library of layer-sets that match the avatar's conversational state, then flip between them with a single **`load_layer_set`** call. The switch is sub-second and atomic. Reference presets in `config/anim_layer_sets.json`:

| Preset | When | Distinguishing layers |
|--------|------|------------------------|
| `jarvis_natural_idle` | default standing | `pose_hold(<vrm>_natural_rest)` + breathing/blink/sway/fidgets + slow head wander |
| `jarvis_relaxed_idle` | extended idle / lounging | deeper slower breath, longer blink interval, dialed-down fidgets |
| `jarvis_thinking_idle` | thinking / no immediate response | overlay `idle_chin_rest.json` clip on upper body, lighter procedurals |
| `jarvis_listening_idle` | user is speaking | faster blink (engagement), pre-loaded `talk_nod.json` at weight 0 for instant promotion |
| `jarvis_talking_idle` | TTS playing | `talk_explain_hands.json` override on upper body + `talk_nod.json` head/neck additive + livelier breath |
| `jarvis_confident_idle` | assertive / hands-on-hips | `pose_hold(<vrm>_confident_rest)` + lighter sway and slower breath |

Promotion pattern (when you don't want a full set swap, just to wake one ready-loaded layer):

```jsonc
// "the user just paused — start nodding while we respond"
{ "id_or_slug": "talk-nod-ready", "weight": 0.5 }   // update_layer
```

## Boot defaults (`config/default.toml`)

Section **`[anim_layers]`**: `auto_install_procedural` installs the same five procedural layers when the stack is **empty** at startup; **`master_enabled_default`** turns composing on.

## Conflicts and tuning

- Layer stack runs **every frame** with **`preserve_omitted_bones: true`** on `ApplyBones` — masked bones still receive upstream pose; unmasked bones from higher layers override.
- **VRMA idle** and **layer stack** can fight visually; disable stack **`masterEnabled`** while debugging pure VRMA, or stop idle VRMA when authoring (see pose controller **`auto_stop_idle_vrma`**).
- **`update_layer`** **`driver_params`** only applies to **procedural** drivers; clip/pose require remove+add.

## Notes for future MCP authors (lessons learned)

- **Always sanity-check connectivity first.** `list_layers` is a free probe; if it returns "Not connected", don't waste tool calls — ask the user to refresh the MCP server in Cursor's MCP panel and resume from a known-good state.
- **Capture every substantive change.** A successful tool response is *not* enough. Use `capture_pose_views` (`front`, `front_left`, `left` minimum; add `back_*` for arm/wing work) after building each preset and after any pose_hold layer change. Saved poses authored against one VRM can land subtly off on another rig — only the picture tells you.
- **Read each capture as evidence, not as confirmation.** When reviewing a `capture_pose_views` PNG: (1) describe the silhouette in plain language **before** consulting your recipe ("right arm extends forward and down, hand near hip" — not "right arm in lounging drape position"). (2) Compare that description to the **intended** shape. (3) If they diverge on something structural — wrong limb direction, eyes wrong, no chair shape, twisted elbow, leg behind body — **stop, fix, re-capture** before moving on. The user shouldn't have to point out "your right elbow is twisted" or "this doesn't look like a sofa pose"; those are visible in the front view you already have. Recipe-anchoring (assuming the pose looks like the recipe says it should) is the single most common source of "I thought it was good but it wasn't." A useful test: cover the recipe and ask "would a stranger looking only at this PNG describe it the way I'd describe my intent?" If no, iterate.
- **Avoid duplicate layer slugs.** `add_layer` doesn't dedupe, so a partially-failed batch can leave two layers with the same slug, and `update_layer` / `remove_layer` then refuses ("ambiguous slug — use numeric id"). Recover by listing layers, removing duplicates by numeric id, then `clear_layers` + `set_layer_stack` to rebuild from a clean state.
- **`pose_hold` layers need the foundation pose to exist *for the loaded VRM*.** A pose saved against one rig (e.g. `hands_on_hips` for `Belka1-mtoon`) can leave arms in T-pose on another rig because the bone rest orientations differ. Author per-VRM rest poses (`<vrm_short_name>_natural_rest`) and reference them by name.
- **Hot-swap timing.** `load_vrm` returns immediately; the new rig is not fully indexed for a frame or two. Wait 1-2 seconds before sending pose_bones / save_current_pose against the new skeleton. (TODO: a future `load_vrm` `wait_for_indexed: true` option would eliminate this race.)
- **Don't write Python scripts to chain MCP calls.** If you ever feel the urge: that's a missing batch tool. Use `set_layer_stack` for stacks, `save_current_pose` for poses, and file the next batch tool you wish existed in this guide.
- **Pause the stack while sculpting a `pose_hold` foundation.** Procedural deltas (breathing, weight_shift, finger_fidget) ride additively on top of the live rig, so they're constantly perturbing the bones you're trying to inspect with `capture_pose_views`. Call **`set_master_enabled false`** before iterating with `pose_bones`, then re-enable it via the next `set_layer_stack` (`master_enabled: true`) or an explicit `set_master_enabled true`. Side effect to remember: with the stack off, the `auto-blink` layer stops driving the eye expressions and pupils render white in captures. That's not a pose bug — it goes away the instant the stack composes again.
- **Match `save_current_pose` `bones` to the layer's role.** A `pose_hold` foundation that's meant to leave room for procedural drivers (e.g. arms-down rest) should snapshot **only the bones the foundation owns** (typically the upper-body chain), so legs / hips stay free for `weight_shift` and other additive layers to move. A `pose_hold` foundation that's meant to fully define a non-standing posture (sit, kneel, prone) needs **every bone you sculpted** in the snapshot — any bone you sculpted but omitted will revert to bind the moment the pose layer reapplies, snapping the posture apart. Decide upfront which role the foundation is playing and pick `bones` to match.
- **Hold a multi-expression face with `animate_expressions`.** `set_expression` only takes one preset. To pin a sustained combination (e.g. `happy: 0.4` + `relaxed: 0.2` for a soft pleased look), call `animate_expressions` with two keyframes: `t=0` with the *current* weights (or zeros) and `t=1.5` with your target weights. After the last keyframe the weights **hold indefinitely** (the tool description: *"After one-shot playback, last sampled weights remain until changed"*), so you get a sustained mood without looping. The `auto-blink` layer still runs on top and momentarily fully closes the eyes — that's the intended interaction (held expression + natural blinks). To revert, fire another `animate_expressions` with `t=0` your held weights and `t=0.4` `{ neutral: 1, ...your_other_keys: 0 }`.
- **`framing_preset: "face_closeup"` is for *standing-pose* faces only.** Sitting / kneeling / reclined / floor poses move the head outside the closeup window and you'll get a blank PNG. Use `framing_preset: "full_body"` with a square aspect (`width: 768, height: 768`) for faces in non-standing poses until a head-tracking framing preset exists.
