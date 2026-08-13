#!/usr/bin/env bash
# Re-copies the IME bridge from EguiMobile and re-applies the one rewrite we make
# to it. vendor/egui-android-ime/src/bridge.rs is a byte copy of upstream's
# ime_bridge.rs except that `crate::host::with_native_activity` (which is
# pub(crate), eframe-initialised, and drags in an android-activity fork) points
# at our own `crate::activity::with_native_activity` instead.
#
# Run after pulling EguiMobile, then diff to see what upstream changed.
set -euo pipefail

EM="${EGUI_MOBILE:-}"
[[ -n "$EM" ]] || { echo "set EGUI_MOBILE to an EguiMobile checkout" >&2; exit 1; }
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

src="$EM/crates/egui-android"
[[ -d "$src" ]] || { echo "EguiMobile not found at $EM (set EGUI_MOBILE)" >&2; exit 1; }

cp "$src/src/ime_bridge.rs" "$repo/vendor/egui-android-ime/src/bridge.rs"
sed -i 's/crate::host::with_native_activity/crate::activity::with_native_activity/g' \
  "$repo/vendor/egui-android-ime/src/bridge.rs"

java_dst="$repo/JarvisAndroid/rust/java/com/github/egui_mobile"
mkdir -p "$java_dst"
cp "$src/java/com/github/egui_mobile/EguiNativeActivity.java" "$java_dst/"
cp "$src/java/com/github/egui_mobile/EguiImeBridge.java" "$java_dst/"

n=$(grep -c 'crate::activity::with_native_activity' "$repo/vendor/egui-android-ime/src/bridge.rs")
left=$(grep -c 'crate::host' "$repo/vendor/egui-android-ime/src/bridge.rs" || true)
echo "bridge.rs: $n rewrites applied, $left leftover crate::host refs (must be 0)"
[[ "$left" == "0" ]] || { echo "upstream added a new crate::host use — port it by hand" >&2; exit 1; }
echo "java: $(ls "$java_dst" | tr '\n' ' ')"
