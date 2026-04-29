#!/usr/bin/env bash
# Build libjarvis_ios.a for aarch64-apple-ios (Linux + xtool / osxcross-style
# setups: ensure CC_/cargo config matches your iOS SDK.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RUST="$ROOT/rust"
GEN="$RUST/generated"

SWIFT_DST="$ROOT/Sources/JarvisIOS"
C_DST="$ROOT/Sources/BridgeFFI/include"
LIB_DST="$ROOT/RustLibs"

mkdir -p "$SWIFT_DST"
# SwiftPM `.copy("assets")` must materialize real files. A symlink to ../assets
# is copied into the .bundle as a symlink and iOS installd rejects it (InvalidSymlink).
ASSET_SRC="$ROOT/../assets"
ASSET_DST="$SWIFT_DST/assets"
mkdir -p "$ASSET_DST"
if [[ -d "$ASSET_SRC" ]]; then
  rsync -aL --delete --exclude '.gitkeep' "$ASSET_SRC/" "$ASSET_DST/"
else
  echo "WARN: Missing $ASSET_SRC; app bundle may lack VRM/animations until path exists." >&2
fi

# 1) Build Rust for device
cargo build --manifest-path "$RUST/Cargo.toml" --target aarch64-apple-ios --release

# 2) Remove stale generated bindings from the Swift target (swift-bridge overwrites below).
# Do **not** delete other *.swift files here — hand-authored app sources live alongside generated
# ones; an allowlist drift silently broke xtool builds (missing types after `build-rust.sh`).
rm -f \
  "$SWIFT_DST/SwiftBridgeCore.swift" \
  "$SWIFT_DST/jarvis_ios.swift"

# 3) Copy generated Swift into the Swift target (flattened)
cp -f "$GEN/SwiftBridgeCore.swift" "$SWIFT_DST/SwiftBridgeCore.swift"

if [[ -f "$GEN/jarvis_ios/jarvis_ios.swift" ]]; then
  cp -f "$GEN/jarvis_ios/jarvis_ios.swift" "$SWIFT_DST/jarvis_ios.swift"
fi

# 3b) Swift 6 / xtool: swift-bridge emits RustStr conformances without @retroactive.
if command -v perl >/dev/null 2>&1 && [[ -f "$SWIFT_DST/SwiftBridgeCore.swift" ]]; then
  # Generated header may use one or two spaces after the colon (Swift 6 needs @retroactive).
  perl -pi -e 's/extension RustStr:\s*Identifiable\b/extension RustStr: @retroactive Identifiable/' "$SWIFT_DST/SwiftBridgeCore.swift"
  perl -pi -e 's/extension RustStr:\s*Equatable\b/extension RustStr: @retroactive Equatable/' "$SWIFT_DST/SwiftBridgeCore.swift"
fi

# 4) Ensure generated Swift can see the C declarations from the BridgeFFI module
for f in "$SWIFT_DST/SwiftBridgeCore.swift" "$SWIFT_DST/jarvis_ios.swift"; do
  if [[ -f "$f" ]] && ! head -n 5 "$f" | grep -q '^import BridgeFFI'; then
    tmp="$(mktemp)"
    printf "import BridgeFFI\n\n" > "$tmp"
    cat "$f" >> "$tmp"
    mv "$tmp" "$f"
  fi
done

# 5) Copy the static library
mkdir -p "$LIB_DST"

LIB="$RUST/target/aarch64-apple-ios/release/libjarvis_ios.a"
if [[ ! -f "$LIB" ]]; then
  echo "ERROR: Missing $LIB"
  echo "Ensure Cargo.toml has [lib] crate-type = [\"staticlib\"]"
  exit 1
fi
cp -f "$LIB" "$LIB_DST/"

# 5b) Catch stale staticlibs before xtool link fails with undefined __swift_bridge__$… symbols
COPIED="$LIB_DST/libjarvis_ios.a"
if command -v llvm-nm >/dev/null 2>&1; then
  OUT=$(llvm-nm "$COPIED" 2>/dev/null || true)
  if [[ -n "$OUT" ]] && ! grep -qF '__swift_bridge__$jarvis_renderer_new' <<<"$OUT"; then
    echo "ERROR: $COPIED is missing __swift_bridge__\$jarvis_renderer_new (FFI out of sync)."
    echo "Rebuild the iOS staticlib; then re-run xtool dev."
    exit 1
  fi
  if [[ -n "$OUT" ]] && ! grep -qF '__swift_bridge__$jarvis_renderer_queue_anim_json' <<<"$OUT"; then
    echo "ERROR: $COPIED is missing __swift_bridge__\$jarvis_renderer_queue_anim_json (FFI out of sync)."
    echo "Rebuild the iOS staticlib; then re-run xtool dev."
    exit 1
  fi
fi

# 6) Copy generated C headers for BridgeFFI
mkdir -p "$C_DST"

cp -f "$GEN/SwiftBridgeCore.h" "$C_DST/SwiftBridgeCore.h"

HDR="$GEN/jarvis_ios/jarvis_ios.h"
if [[ ! -f "$HDR" ]]; then
  echo "ERROR: Missing $HDR (swift-bridge output layout)"
  exit 1
fi
cp -f "$HDR" "$C_DST/jarvis_ios.h"

cat > "$C_DST/bridging-header.h" <<'EOF'
#pragma once
#include "SwiftBridgeCore.h"
#include "jarvis_ios.h"
EOF
