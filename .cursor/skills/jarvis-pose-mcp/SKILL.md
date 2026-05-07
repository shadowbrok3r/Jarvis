---
name: jarvis-pose-mcp
description: Uses jarvis-avatar pose MCP (`user-pose-controller` / `pose-controller`) per tool JSON schemas—bone maps, semantic intents, expressions, layer stack, verification. Use when posing VRMs, expressions, `pose_bones`, `raise_leg`, `bend_knee`, `arms_down_rest`, `set_layer_stack`, or capture-driven pose work in this repo.
disable-model-invocation: false
---

# Jarvis pose MCP

Argument shapes must match **`tools/list`** (and the client's cached tool JSON). Call **`get_pose_guide`** for Euler details; **`get_layer_authoring_guide`** for the layer stack.

## Tool ladder (try in order)

1. **Semantic intents** — `raise_leg`, `bend_knee`, `arms_down_rest`, `make_fist`. They compile to bounded Euler maps the server knows are safe. **Pick these first** for any "raise leg / bend knee / arms down / fist" intent.
2. **Library poses** — `apply_pose` after `list_poses`. Good for known baselines.
3. **Raw `pose_bones`** (Euler degrees) — only when no semantic tool fits. Always pass `dry_run: true` first if you are unsure of an axis or sign.
4. **Raw `set_bones`** (quaternions) — last resort, mostly for replaying numeric data.

## Hybrid safety policy

`pose_bones` and `set_bones` reject:

- **`preserve_omitted_bones: false`** — would reset every unlisted bone to bind/identity and breaks partial maps.
- **Catastrophic** requests — many bones at near-axis limits in one call.
- **Severe** angles (≥ 80° on any single axis) — unless you set `allow_large_angles: true`.

Optional knobs on `pose_bones`:

- `strict: true` — escalates near-limit warnings into hard-fails (recommended for batch/test fixtures).
- `allow_large_angles: true` — opt in to severe single-axis angles (still blocks catastrophic patterns).
- `dry_run: true` — runs validation + sanitize and returns the would-apply summary (sanitized rotations, warnings, severity) without touching the rig.

`capture_pose_views` returns a `viewCoverageWarning` whenever you ask for a front-only view set. Always include at least one side (`left`/`right`) and add `back` for upper-body changes.

## JSON shapes (literal; no double-encoded strings)

- Map-typed fields are **JSON objects**, not strings containing JSON.
- **`raise_leg`** / **`bend_knee`**: `side` = `"left"` | `"right"`, `amount` = 0..=1 (1 ≈ 70° on the dominant axis). `raise_leg` accepts optional `direction` = `"forward"` (default — hip flex) or `"outward"` (hip abduction, mirrored roll).
- **`arms_down_rest`**: optional `amount` (default 0.85). No bone names needed.
- **`pose_bones`**: top-level **`bones`** = object: bone name → `{ pitch_deg?, yaw_deg?, roll_deg? }`. Optional `strict`, `allow_large_angles`, `dry_run` booleans.
- **`set_expression`**: top-level **`expressions`** = object: preset name → weight 0..1.
- **`save_current_pose`**: top-level **`name`** (string) required.
- **`capture_pose_views`**: **`capture_id`** + **`views`** (array of view slugs) required. `output_dir` is optional — the server defaults it. `embed_images` (default true) returns inline images in the tool result so you can verify the pose without reading host paths.

## Examples

Lift the right leg forward halfway and verify:

```json
{ "side": "right", "amount": 0.5 }
```

```json
{
  "capture_id": "right_leg_lift",
  "views": ["front", "left", "right", "back"],
  "framing_preset": "full_body"
}
```

Drop both arms into a natural rest:

```json
{ "amount": 0.85 }
```

Raw `pose_bones` for a wide stance (stays inside safe `roll_deg` for upper legs):

```json
{
  "bones": {
    "leftUpperLeg":  { "roll_deg":  12, "pitch_deg": -5 },
    "rightUpperLeg": { "roll_deg": -12, "pitch_deg": -5 }
  },
  "dry_run": true
}
```

## Where detail lives

| Source | Use |
|--------|-----|
| [assets/POSE_GUIDE.md](../../../assets/POSE_GUIDE.md) | Euler axes, traps (arms/legs), probes, capture workflow |
| [assets/LAYER_AUTHORING_GUIDE.md](../../../assets/LAYER_AUTHORING_GUIDE.md) | Layer stack, `set_layer_stack`, pose-hold |
| MCP **`get_pose_guide`** | Same as POSE_GUIDE inside the tool loop |
| MCP **`get_layer_authoring_guide`** | Same as layer guide |

## Which tool for what

| Goal | Tool |
|------|------|
| Lift a leg / hip flex | `raise_leg` |
| Bend a knee | `bend_knee` |
| Drop arms to a rest pose | `arms_down_rest` |
| Hand curl | `make_fist` |
| Many bones, Euler degrees | `pose_bones` (raw — try semantic first) |
| Micro quat nudges | `adjust_bone` (tiny deltas only) |
| Face partial | `set_expression` |
| Face replace-all | `set_expressions_full` |
| Layers | `list_layers`, `add_layer`, `set_layer_stack`, … |
| Visual proof | `capture_pose_views` — **use returned images**, not host file paths |

## Session practices

- After **`jarvis-avatar`** restarts, reconnect or **refresh** the MCP server in the client.
- After **`load_vrm`**, wait briefly before **`pose_bones`** / **`get_bone_reference`**.
- After leg / arm edits, **always** capture `left`, `right`, and `back` — `front` alone hides knee direction, elbow inversion, and foot crossover.
- **One bone, one axis** probe on unfamiliar rigs (see POSE_GUIDE).
