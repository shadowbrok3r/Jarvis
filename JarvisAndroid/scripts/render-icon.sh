#!/usr/bin/env bash
# Rasterizes JarvisAndroid/icon/*.svg into the APK res/ tree.
# Legacy launcher icons are 48dp; adaptive layers are 108dp.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
svg="$root/icon"
res="$root/rust/res"

command -v inkscape >/dev/null || { echo "inkscape required" >&2; exit 1; }

# bucket:scale
buckets=(mdpi:1 hdpi:1.5 xhdpi:2 xxhdpi:3 xxxhdpi:4)

render() { # <svg> <out.png> <px>
  inkscape "$1" --export-type=png --export-filename="$2" \
    --export-width="$3" --export-height="$3" >/dev/null 2>&1
}

for entry in "${buckets[@]}"; do
  bucket="${entry%%:*}"
  scale="${entry##*:}"
  dir="$res/mipmap-$bucket"
  mkdir -p "$dir"

  legacy=$(python3 -c "print(round(48*$scale))")
  adaptive=$(python3 -c "print(round(108*$scale))")

  render "$svg/legacy.svg"     "$dir/ic_launcher.png"            "$legacy"
  render "$svg/background.svg" "$dir/ic_launcher_background.png" "$adaptive"
  render "$svg/foreground.svg" "$dir/ic_launcher_foreground.png" "$adaptive"
  echo "$bucket: ${legacy}px legacy, ${adaptive}px adaptive"
done

mkdir -p "$res/mipmap-anydpi-v26"
cat >"$res/mipmap-anydpi-v26/ic_launcher.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@mipmap/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
</adaptive-icon>
EOF

echo "wrote $res"
