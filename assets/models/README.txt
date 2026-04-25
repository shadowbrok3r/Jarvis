Place assets under `assets/` so Bevy’s `AssetServer` resolves paths from `config/default.toml`:

- `models/airi.vrm` — VRM 1.0 avatar
- `models/idle_loop.vrma` — default idle loop (VRMA 1.0), played on load

Paths are relative to the `assets/` directory (e.g. `models/foo.vrma` → `assets/models/foo.vrma` on disk).
