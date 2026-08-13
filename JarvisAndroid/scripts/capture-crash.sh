#!/usr/bin/env bash
# Installs a build, cold-starts it, and dumps everything needed to diagnose a
# crash to /tmp/jarvis-crash.log.
#
#   ./JarvisAndroid/scripts/capture-crash.sh                     # ime-bridge build
#   ./JarvisAndroid/scripts/capture-crash.sh jarvis-default.apk  # a specific one
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apk="${1:-jarvis-ime-bridge.apk}"
[[ -f "$repo/dist/$apk" ]] || { echo "no $repo/dist/$apk" >&2; exit 1; }

export PATH="$HOME/Android/Sdk/platform-tools:$PATH"
pkg=com.kingsofalchemy.jarvis
out=/tmp/jarvis-crash.log

adb install -r "$repo/dist/$apk"
adb shell am force-stop "$pkg" || true
adb logcat -c
adb shell am start -n "$pkg/com.github.egui_mobile.EguiNativeActivity" >/dev/null
sleep 12

{
  echo "=== apk: $apk"
  echo "=== pid after 12s: $(adb shell pidof "$pkg" || echo DEAD)"
  echo
  echo "=== rust / ime / bevy ==="
  adb logcat -d -s jarvis:V RustStdoutStderr:V RustPanic:E EguiIme:V AndroidRuntime:E DEBUG:V 2>/dev/null
  echo
  echo "=== native crash (tombstone header) ==="
  adb logcat -d -b crash 2>/dev/null | tail -60
} >"$out"

echo
echo "wrote $out ($(wc -l <"$out") lines)"
grep -iE "panic|fatal|signal|abort|Exception" "$out" | head -20 || echo "(no obvious fatal line)"
