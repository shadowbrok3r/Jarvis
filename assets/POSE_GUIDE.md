# VRM Pose Authoring Guide

Reference for hand-authored poses and for MCP callers. Stored rotations are **normalized pose quaternions** (deltas from each bone’s rest pose in the rig). The runtime maps them to actual joint locals; see `pose_driver` in the repo for the full formula. Quaternions use `[x, y, z, w]`; `**[0, 0, 0, 1]` means “no pose delta”** on that bone (the bone stays at rest for pose purposes).

## Coordinate system

VRM bones are described here in the same normalized space the app expects. All rotations use quaternions `[x, y, z, w]` as above.

For limb bones (arms, legs):

- **X axis** = flexion/extension (bend forward/back)
- **Y axis** = twist (internal/external rotation)
- **Z axis** = abduction/adduction (spread away from/toward body)

For spine/torso bones:

- **X axis** = forward/backward lean
- **Y axis** = left/right twist
- **Z axis** = left/right tilt

## Bone Hierarchy

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

## Quaternion Cheat Sheet

### Key rules

- No pose delta on a bone: `[0, 0, 0, 1]`
- **Body/limb bones**: Never exceed ±0.3 on any x/y/z for natural movement
- **Finger bones**: Z axis is the curl axis (up to ±0.70 for full fist)
  - Right hand: positive Z = curl, Left hand: negative Z = curl
  - Thumb: Y axis is primary (opposition/flexion)
- Typical subtle adjustments: 0.02–0.08
- Moderate adjustments: 0.08–0.18
- Maximum natural range (body): 0.18–0.30
- The `w` component should always be positive (e.g., 0.70–1.0 for fingers, 0.92–1.0 for body)

### Component-to-Visual Mapping


| Component | Effect on Arms                 | Effect on Spine | Effect on Legs  |
| --------- | ------------------------------ | --------------- | --------------- |
| +X        | Flex forward/down              | Lean forward    | Raise forward   |
| -X        | Extend back/up                 | Lean backward   | Extend backward |
| +Y        | Twist outward (L) / inward (R) | Twist right     | Twist outward   |
| -Y        | Twist inward (L) / outward (R) | Twist left      | Twist inward    |
| +Z        | Abduct (away)                  | Tilt right      | Abduct          |
| -Z        | Adduct (toward body)           | Tilt left       | Adduct          |


### Quick Reference Rotations


| Desired Movement            | Quaternion             |
| --------------------------- | ---------------------- |
| Slight forward lean (spine) | `[0.05, 0, 0, 0.999]`  |
| Arm raised ~30° to side     | `[0, 0, -0.26, 0.966]` |
| Arm raised ~15° to side     | `[0, 0, -0.13, 0.992]` |
| Elbow bent ~45°             | `[-0.2, 0, 0, 0.98]`   |
| Elbow bent ~30°             | `[-0.13, 0, 0, 0.992]` |
| Head tilt right slight      | `[0, 0, 0.05, 0.999]`  |
| Head turn right slight      | `[0, -0.08, 0, 0.997]` |
| Wrist twist slight          | `[0, 0.08, 0, 0.997]`  |


## Natural Pose Construction Method

### Step-by-step process:

1. **Start from `[0, 0, 0, 1]` on each bone you touch** — omit bones that should stay at rest
2. **Work parent-to-child** — set shoulder before upper arm before lower arm before hand
3. **Adjust one bone at a time** with increments of 0.05–0.15
4. **Check the viewport** after each bone change
5. **Symmetry**: to mirror left→right, negate the Y and Z components:
  - Left: `[x, y, z, w]` → Right: `[x, -y, -z, w]`

### Building a wave pose example:

```
1. rightShoulder: [0, 0, -0.04, 0.999]      — slight shoulder lift
2. rightUpperArm: [-0.02, 0.05, -0.25, 0.97] — arm raised to side
3. rightLowerArm: [-0.15, 0, 0, 0.989]       — elbow bent
4. rightHand: [0, 0.08, 0, 0.997]            — slight wrist turn
5. Check the viewport and adjust
```

## Relaxed Hand Template

Every pose should include natural hand positioning. Fingers slightly curled using **Z axis** (negative Z for left, positive Z for right). Thumbs use **Y axis**:

```json
{
  "leftThumbMetacarpal": { "rotation": [0, 0.04, -0.02, 0.999] },
  "leftThumbProximal": { "rotation": [0, 0.06, 0, 0.998] },
  "leftIndexProximal": { "rotation": [0, 0, -0.1, 0.995] },
  "leftIndexIntermediate": { "rotation": [0, 0, -0.08, 0.997] },
  "leftMiddleProximal": { "rotation": [0, 0, -0.12, 0.993] },
  "leftMiddleIntermediate": { "rotation": [0, 0, -0.1, 0.995] },
  "leftRingProximal": { "rotation": [0, 0, -0.12, 0.993] },
  "leftRingIntermediate": { "rotation": [0, 0, -0.1, 0.995] },
  "leftLittleProximal": { "rotation": [0, 0, -0.1, 0.995] },
  "leftLittleIntermediate": { "rotation": [0, 0, -0.08, 0.997] },
  "rightThumbMetacarpal": { "rotation": [0, -0.04, 0.02, 0.999] },
  "rightThumbProximal": { "rotation": [0, -0.06, 0, 0.998] },
  "rightIndexProximal": { "rotation": [0, 0, 0.1, 0.995] },
  "rightIndexIntermediate": { "rotation": [0, 0, 0.08, 0.997] },
  "rightMiddleProximal": { "rotation": [0, 0, 0.12, 0.993] },
  "rightMiddleIntermediate": { "rotation": [0, 0, 0.1, 0.995] },
  "rightRingProximal": { "rotation": [0, 0, 0.12, 0.993] },
  "rightRingIntermediate": { "rotation": [0, 0, 0.1, 0.995] },
  "rightLittleProximal": { "rotation": [0, 0, 0.1, 0.995] },
  "rightLittleIntermediate": { "rotation": [0, 0, 0.08, 0.997] }
}
```

### Fist Reference (full grip)

Right hand fist — Z values from verified VRM pose data:

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

Left hand fist — mirror (negate Z):

```json
{
  "leftThumbProximal": { "rotation": [-0.21, 0.57, -0.40, 0.68] },
  "leftIndexProximal": { "rotation": [0, 0, -0.42, 0.908] },
  "leftIndexIntermediate": { "rotation": [0, 0, -0.68, 0.733] },
  "leftIndexDistal": { "rotation": [0, 0, -0.35, 0.937] }
}
```

These are automatically merged into any pose that doesn't explicitly set finger bones.

## Common Mistakes

1. **Over-rotation**: Using values like 0.5 or 0.7 produces grotesque distortion. Stay under 0.3.
2. **Forgetting the bone chain**: Setting `rightLowerArm` without `rightUpperArm` causes the forearm to rotate relative to a stationary upper arm — usually looks wrong.
3. **Setting child without parent**: The elbow can't bend naturally if the shoulder hasn't positioned the arm first.
4. **Ignoring symmetry**: A natural standing pose has roughly symmetrical arms and legs. Asymmetry should be intentional.
5. **Missing finger bones**: Without explicit finger data, the idle animation or T-pose default controls fingers, leading to stiff or random hand positions.
6. **Absolute vs. relative thinking**: Quaternions are relative to the bone's rest orientation, not world space. A bone that's already rotated by its parent inherits that rotation.

## Per-Bone Reference Table


| Bone                 | Parent        | X Effect           | Y Effect         | Z Effect           | Safe Range          |
| -------------------- | ------------- | ------------------ | ---------------- | ------------------ | ------------------- |
| hips                 | (root)        | Forward/back tilt  | Left/right twist | Left/right sway    | ±0.08               |
| spine                | hips          | Forward/back bend  | Torso twist      | Side bend          | ±0.12               |
| chest                | spine         | Forward/back bend  | Torso twist      | Side bend          | ±0.10               |
| upperChest           | chest         | Forward/back bend  | Torso twist      | Side bend          | ±0.08               |
| neck                 | upperChest    | Nod forward/back   | Turn left/right  | Tilt left/right    | ±0.15               |
| head                 | neck          | Nod forward/back   | Turn left/right  | Tilt left/right    | ±0.15               |
| leftShoulder         | upperChest    | Shrug forward/back | Roll             | Shrug up/down      | ±0.06               |
| rightShoulder        | upperChest    | Shrug forward/back | Roll             | Shrug up/down      | ±0.06               |
| leftUpperArm         | leftShoulder  | Forward/back swing | Twist in/out     | Raise/lower        | ±0.30               |
| rightUpperArm        | rightShoulder | Forward/back swing | Twist in/out     | Raise/lower        | ±0.30               |
| leftLowerArm         | leftUpperArm  | Bend elbow         | Forearm twist    | (minimal)          | X: ±0.30, Y: ±0.15  |
| rightLowerArm        | rightUpperArm | Bend elbow         | Forearm twist    | (minimal)          | X: ±0.30, Y: ±0.15  |
| leftHand             | leftLowerArm  | Wrist flex/extend  | Wrist twist      | Wrist deviation    | ±0.15               |
| rightHand            | rightLowerArm | Wrist flex/extend  | Wrist twist      | Wrist deviation    | ±0.15               |
| leftUpperLeg         | hips          | Forward/back swing | Twist in/out     | Spread apart       | ±0.25               |
| rightUpperLeg        | hips          | Forward/back swing | Twist in/out     | Spread apart       | ±0.25               |
| leftLowerLeg         | leftUpperLeg  | Bend knee          | Twist            | (minimal)          | X: -0.30 to 0       |
| rightLowerLeg        | rightUpperLeg | Bend knee          | Twist            | (minimal)          | X: -0.30 to 0       |
| leftFoot             | leftLowerLeg  | Point/flex ankle   | Twist            | Inversion/eversion | ±0.15               |
| rightFoot            | rightLowerLeg | Point/flex ankle   | Twist            | Inversion/eversion | ±0.15               |
| leftToes             | leftFoot      | Curl toes          | —                | —                  | X: ±0.10            |
| rightToes            | rightFoot     | Curl toes          | —                | —                  | X: ±0.10            |
| *Finger Proximal     | Hand          | (minimal)          | Spread           | **Curl finger**    | Z: R +0.50, L -0.50 |
| *Finger Intermediate | Proximal      | (minimal)          | —                | **Curl further**   | Z: R +0.70, L -0.70 |
| *Finger Distal       | Intermediate  | (minimal)          | —                | **Curl tip**       | Z: R +0.42, L -0.42 |
| *Thumb Metacarpal    | Hand          | Flex               | **Opposition**   | Abduction          | Y: ±0.12            |
| *Thumb Proximal      | Metacarpal    | —                  | **Flex**         | —                  | Y: ±0.60            |
| *Thumb Distal        | Proximal      | —                  | **Flex tip**     | —                  | Y: ±0.45            |


## VRM Expressions

Available expression names and their effects:


| Expression | Effect                               |
| ---------- | ------------------------------------ |
| happy      | Smile, raised cheeks                 |
| angry      | Furrowed brows, tense jaw            |
| sad        | Drooping corners, raised inner brows |
| relaxed    | Soft smile, half-closed eyes         |
| surprised  | Wide eyes, raised brows, open mouth  |
| neutral    | Default resting face                 |


Expression values range from 0.0 (off) to 1.0 (full intensity). Typical natural values: 0.2–0.6.

## Iterative refinement (MCP or UI)

When creating a new pose:

1. **Load a baseline** — `apply_pose` with a library pose, or `reset_pose`, then build from there.
2. **Inspect** — use the running app’s 3D view; there is no screenshot MCP tool in jarvis-avatar.
3. **Read state** — `get_current_bone_state` for exact quaternions on the rig.
4. **Adjust** — `adjust_bone` with small deltas (±0.02 to ±0.05), or `set_bones` when you are replacing a full set you control.
5. **Save** — `create_pose` once it looks right.

Expect a few iterations per pose. Change only one or two bones per step when tuning.