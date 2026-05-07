---
name: jarvis-pose-mcp
description: Uses jarvis-avatar pose MCP (`user-pose-controller` / `pose-controller`) per tool JSON schemas—bone maps, expressions, layer stack, verification. Use when posing VRMs, expressions, `pose_bones`, `set_layer_stack`, or capture-driven pose work in this repo.
disable-model-invocation: false
---

# Jarvis pose MCP

This skill describes how to call the **jarvis-avatar** pose MCP server: argument shapes match the schemas exposed by `tools/list` (also cached under the client’s MCP tool descriptors). It does not assume any particular IDE or agent runtime.

## Where the detail lives

| Source | Use |
|--------|-----|
| [assets/POSE_GUIDE.md](../../../assets/POSE_GUIDE.md) | Euler tools, limits, mirroring, squat/recline patterns, capture-as-truth |
| [assets/LAYER_AUTHORING_GUIDE.md](../../../assets/LAYER_AUTHORING_GUIDE.md) | Layer stack, blend modes, `set_layer_stack`, pose-hold, saving sets |
| MCP **`get_pose_guide`** | Same pose material inside the tool loop |
| MCP **`get_layer_authoring_guide`** | Same layer material inside the tool loop |

## JSON shapes (schema-aligned)

Tools deserialize arguments with **serde** against Rust types and JSON Schema. Match the schema literally.

### Objects vs nested structure

- Any property whose schema type is **`object`** must be a **JSON object** in the `tools/call` `arguments` payload: `{ "key": { ... } }`.
- Do **not** pass a **string** whose text is JSON (a second serialization layer). The server expects a nested object, not a string value holding encoded JSON.
- Same rule for **`bones`**, **`expressions`**, **`driver`**, **`layers`** entries, and other map-shaped fields.

### `pose_bones`

- **`bones`**: object whose keys are bone names (VRM humanoid names); each value is an object with optional `pitch_deg`, `yaw_deg`, `roll_deg` (numbers). **Not** an array of pairs.
- **`expressions`**: optional object, preset name → weight `0..1`. Omit the key if unused.

Illustrative shape (values arbitrary):

```json
{
  "bones": {
    "leftUpperLeg": { "yaw_deg": 25, "pitch_deg": -10 },
    "hips": { "pitch_deg": -15 }
  },
  "expressions": { "smile_smirk": 0.8 }
}
```

### Expressions without bones

Use **`set_expression`** (partial merge) or **`set_expressions_full`** (replace all presets). The `expressions` argument is the same map shape. Preset names must exist on the loaded model — use **`list_expressions`** first when unsure.

### Layer tools

`add_layer` takes a structured **`driver`** object (`kind` + fields). **`set_layer_stack`** takes **`layers`**: an array of layer objects, each with `driver`, `blend_mode`, etc. Author from [LAYER_AUTHORING_GUIDE.md](../../../assets/LAYER_AUTHORING_GUIDE.md) (batch section); keep each layer value as a real JSON object, not a string blob.

## Which tool for what

| Goal | Tool |
|------|------|
| Many bones in local Euler degrees | `pose_bones` |
| Hand curl template | `make_fist` |
| Small quaternion-component tweaks | `adjust_bone` (tiny steps only; see POSE_GUIDE) |
| Face partial merge | `set_expression` or `pose_bones` with `expressions` |
| Full face state in one map | `set_expressions_full` |
| Stack of layers / presets | `list_layers`, `add_layer`, **`set_layer_stack`**, `save_layer_set`, `load_layer_set` |
| Visual proof | `capture_pose_views` — read PNGs as ground truth |

## Session practices (jarvis-avatar)

- If the MCP client stops receiving responses after a **jarvis-avatar** restart, **reconnect or refresh** the pose MCP server in the client so the stdio session matches the running binary.
- After **`load_vrm`**, allow a short moment before **`pose_bones`** or **`get_bone_reference`** so the rig can index.
- Prefer MCP tools for posing and layers; batch tools (`set_layer_stack`, etc.) reduce round-trips when authoring stacks.

## Minimal probes

**One bone, one axis** (axis discovery on an unfamiliar rig — expand in POSE_GUIDE):

```json
{ "bones": { "rightLowerArm": { "roll_deg": -55 } } }
```

**Layer batch**: follow the JSONc example in LAYER_AUTHORING_GUIDE (`set_layer_stack`); `layers` is an array of objects, each `driver` a tagged object.
