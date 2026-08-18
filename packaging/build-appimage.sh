#!/usr/bin/env bash
# Build an AppImage for LDM.
#
# Requirements: linuxdeploy (https://github.com/linuxdeploy/linuxdeploy) and
# appimagetool on PATH, or set LINUXDEPLOY / APPIMAGETOOL. The script fetches
# them if missing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCH="${ARCH:-x86_64}"
TARGET_DIR="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
APP_DIR="$TARGET_DIR/AppDir"

LINUXDEPLOY="${LINUXDEPLOY:-$TARGET_DIR/linuxdeploy-$ARCH.AppImage}"
APPIMAGETOOL="${APPIMAGETOOL:-$TARGET_DIR/appimagetool-$ARCH.AppImage}"

if [ ! -x "$LINUXDEPLOY" ]; then
  echo "==> Downloading linuxdeploy..."
  curl -L -o "$LINUXDEPLOY" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage"
  chmod +x "$LINUXDEPLOY"
fi
if [ ! -x "$APPIMAGETOOL" ]; then
  echo "==> Downloading appimagetool..."
  curl -L -o "$APPIMAGETOOL" \
    "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
  chmod +x "$APPIMAGETOOL"
fi

echo "==> Building release binaries..."
cargo build --release -p ldm-gui -p ldm-cli -p ldm-native-host --manifest-path "$ROOT/Cargo.toml"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/usr/bin" "$APP_DIR/usr/lib/ldm" "$APP_DIR/usr/share/applications" \
  "$APP_DIR/usr/share/icons/hicolor/512x512/apps"

cp "$TARGET_DIR/release/ldm-gui" "$APP_DIR/usr/bin/"
cp "$TARGET_DIR/release/ldm" "$APP_DIR/usr/bin/"
cp "$TARGET_DIR/release/ldm-native-host" "$APP_DIR/usr/lib/ldm/"
cp "$ROOT/assets/icon.png" "$APP_DIR/usr/share/icons/hicolor/512x512/apps/ldm.png"
cp "$ROOT/assets/icon.png" "$APP_DIR/ldm.png"

sed 's/^Exec=ldm-gui/Exec=ldm-gui/; s/^Icon=ldm/Icon=ldm/' \
  "$ROOT/packaging/ldm.desktop" > "$APP_DIR/usr/share/applications/ldm.desktop"

cat > "$APP_DIR/AppRun" <<'EOF'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:$PATH"
export LD_LIBRARY_PATH="$HERE/usr/lib:$LD_LIBRARY_PATH"
exec "$HERE/usr/bin/ldm-gui" "$@"
EOF
chmod +x "$APP_DIR/AppRun"

echo "==> Bundling libraries with linuxdeploy..."
# Extract linuxdeploy (AppImages need FUSE; fall back to --appimage-extract).
if ! "$LINUXDEPLOY" --appimage-extract-and-run --appdir "$APP_DIR" \
    -d "$APP_DIR/usr/share/applications/ldm.desktop" \
    -i "$APP_DIR/usr/share/icons/hicolor/512x512/apps/ldm.png"; then
  echo "linuxdeploy failed — is FUSE available? Try: sudo apt install libfuse2"
  exit 1
fi

echo "==> Building AppImage..."
"$APPIMAGETOOL" --appimage-extract-and-run "$APP_DIR" "$TARGET_DIR/LDM-$ARCH.AppImage"
echo "==> Done: $TARGET_DIR/LDM-$ARCH.AppImage"
