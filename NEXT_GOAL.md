# Next Goal: Kimodo Motion Pass (Other PC)

## Current State
- `reset_pose` behavior is stable again.
- Pose verification tooling is in place and working:
  - `capture_pose_views` supports transparent PNG output.
  - Framing presets available: `full_body`, `face_closeup`.
  - Deterministic output naming is already implemented.
- We successfully refined and saved a reusable pose:
  - `mcp_retry_smile_tilt_wavehand_v2`
- Expression + pose refinement loop is working with MCP.

## Blocker We Hit
- `generate_motion` timed out twice waiting for Kimodo (180s each).
- Because of that, no new wave animation file was produced in the latest run.

## Immediate Next Goal
- Run Kimodo with this project on the other PC.
- Then run a **motion-only retry** from the refined pose state:
  1. Generate motion for: smile + slight head tilt + natural wave + individual finger curls.
  2. Capture verification images with both:
     - `framing_preset: "full_body"`
     - `framing_preset: "face_closeup"`
  3. Do 1-2 targeted correction passes if needed.
  4. Save final animation metadata/category for reuse.

## Suggested First Prompt For Motion
- "Natural friendly wave with slight right head tilt and warm smile. Keep torso stable. Right arm performs small oscillating wrist-led wave. Include subtle independent right-finger curl/uncurl offsets (index/middle/ring/little unsynced) for human variation."

## Useful Capture Targets
- Output directory:
  - `/home/shadowbroker/Desktop/JarvisAvatar/artifacts/pose_capture_retry`
- Use one capture ID per pass, for example:
  - `motion_pass1_full`
  - `motion_pass1_face`

## Definition of Done
- One saved animation that reads natural in both full-body and face-closeup captures.
- No obvious robotic finger sync, no harsh arm twist, smile/head tilt coordinated with wave rhythm.
