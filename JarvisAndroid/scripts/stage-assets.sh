#!/usr/bin/env bash
# Copies the bootstrap file set into the APK staging dir and writes index.txt,
# which asset_bootstrap.rs reads (AAssetDir cannot enumerate subdirectories).
#
# Paths in index.txt are relative to the app's internal storage root, so
# `assets/...`, `config/...` and a top-level `user.toml` can all ship.
#
# The 2.1 GB desktop assets/ tree is NOT packaged wholesale: one VRM plus the
# animation/pose libraries. Everything else is meant to arrive over the hub.
#
#   INCLUDE_SECRETS=1   ship config/user.toml verbatim (auth tokens included)
#                       otherwise token/secret values are blanked
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dst="$repo/JarvisAndroid/rust/assets"

# The bundled avatar, repo-relative. Change this to ship a different model.
model="assets/models/3.ios.vrm"

files=(
  "$model"
  "assets/models/idle_loop.vrma"
  "assets/egui_jarvis_theme.json"
  # IBL environment map. user.toml sets environment_map_enabled, and without
  # these the load errors every frame. The 80 MB .exr source is not shipped.
  "assets/maps/_diffuse.ktx2"
  "assets/maps/_specular.ktx2"
)

# Whole directories, repo-relative. Animation clips are what the layer stack and
# avatar_defaults.idle_clip resolve by name, so they have to be on-device until
# the hub client exists.
dirs=(
  "assets/animations"
  "assets/poses"
  "config/spring_presets"
  "config/ModelOverrides"
)

# Loose config files. `semantic_intent_calibration` is skipped: its plugin is
# compiled out on Android.
files+=(
  "config/emotions.json"
  "config/mtoon_overrides.json"
  "config/anim_layer_sets.json"
  "config/anim_layer_sets2.json"
  "config/color_scheme.json"
  "config/pose_graph.json"
)

for d in "${dirs[@]}"; do
  if [[ ! -d "$repo/$d" ]]; then
    echo "warn: missing $repo/$d — skipped" >&2
    continue
  fi
  while IFS= read -r f; do
    files+=("${f#"$repo/"}")
  done < <(find "$repo/$d" -type f \( -name '*.json' -o -name '*.toml' -o -name '*.vrma' \) | sort)
done

rm -rf "$dst"
mkdir -p "$dst"

: >"$dst/index.txt"
for rel in "${files[@]}"; do
  if [[ ! -f "$repo/$rel" ]]; then
    echo "warn: missing $repo/$rel — skipped" >&2
    continue
  fi
  mkdir -p "$dst/$(dirname "$rel")"
  cp "$repo/$rel" "$dst/$rel"
  echo "$rel" >>"$dst/index.txt"
done

# The device settings overlay is read from <internal>/user.toml, not config/.
if [[ -f "$repo/config/user.toml" ]]; then
  if [[ "${INCLUDE_SECRETS:-0}" == "1" ]]; then
    cp "$repo/config/user.toml" "$dst/user.toml"
    echo "user.toml: shipped WITH secrets" >&2
  else
    # Blank any value whose key looks like a credential. Everything else is
    # copied byte-for-byte so camera/graphics/avatar tuning survives intact.
    sed -E 's/^([[:space:]]*[a-z_]*(auth_token|webhook_secret|ha_token|api_key|password)[[:space:]]*=[[:space:]]*)".*"/\1""/' \
      "$repo/config/user.toml" >"$dst/user.toml"
    echo "user.toml: secrets blanked (set INCLUDE_SECRETS=1 to ship them)" >&2
  fi
  echo "user.toml" >>"$dst/index.txt"
fi

# Content stamp. The on-device re-extraction marker is a hash of index.txt, so
# without this, editing a staged file without changing the file *list* would
# leave the stale copy on the device.
stamp="$(find "$dst" -type f ! -name index.txt -print0 | sort -z \
  | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)"
printf '# content %s\n%s' "$stamp" "$(cat "$dst/index.txt")" >"$dst/index.txt.new"
mv "$dst/index.txt.new" "$dst/index.txt"

echo "staged $(grep -vc '^#' "$dst/index.txt") files -> $dst ($(du -sh "$dst" | cut -f1)), stamp ${stamp:0:12}"
