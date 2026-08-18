#!/usr/bin/env bash
# Build a .deb package for LDM.
#
# Usage: ./packaging/build-deb.sh [version]
set -euo pipefail

VERSION="${1:-0.1.0}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="ldm_${VERSION}_amd64"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> Building release binaries..."
cargo build --release -p ldm-gui -p ldm-cli -p ldm-native-host --manifest-path "$ROOT/Cargo.toml"

BIN_DIR="$STAGE/$PKG/usr/bin"
LIB_DIR="$STAGE/$PKG/usr/lib/ldm"
mkdir -p "$BIN_DIR" "$LIB_DIR" \
  "$STAGE/$PKG/usr/share/applications" \
  "$STAGE/$PKG/usr/share/icons/hicolor/scalable/apps" \
  "$STAGE/$PKG/usr/share/icons/hicolor/128x128/apps" \
  "$STAGE/$PKG/usr/share/icons/hicolor/64x64/apps" \
  "$STAGE/$PKG/usr/share/icons/hicolor/48x48/apps" \
  "$STAGE/$PKG/usr/share/icons/hicolor/32x32/apps" \
  "$STAGE/$PKG/usr/share/doc/ldm" \
  "$STAGE/$PKG/DEBIAN"

# Binaries.
cp "$ROOT/target/release/ldm-gui" "$BIN_DIR/"
cp "$ROOT/target/release/ldm" "$BIN_DIR/"
cp "$ROOT/target/release/ldm-native-host" "$LIB_DIR/"

# Native messaging host manifests (system-wide: /usr/lib/mozilla + /etc/chromium).
mkdir -p "$STAGE/$PKG/etc/chromium/native-messaging-hosts" \
  "$STAGE/$PKG/usr/lib/mozilla/native-messaging-hosts"
sed "s|/PATH/TO/ldm-native-host|/usr/lib/ldm/ldm-native-host|g" \
  "$ROOT/browser/native-host/ldm.firefox.json" \
  > "$STAGE/$PKG/usr/lib/mozilla/native-messaging-hosts/ldm.json"

# Icons.
for s in 32 48 64 128; do
  mkdir -p "$STAGE/$PKG/usr/share/icons/hicolor/${s}x${s}/apps"
  convert "$ROOT/assets/icon.png" -resize ${s}x${s} \
    "$STAGE/$PKG/usr/share/icons/hicolor/${s}x${s}/apps/ldm.png"
done
convert "$ROOT/assets/icon.png" -resize 512x512 \
  "$STAGE/$PKG/usr/share/icons/hicolor/scalable/apps/ldm.svg" 2>/dev/null || true

# Desktop entry.
cp "$ROOT/packaging/ldm.desktop" "$STAGE/$PKG/usr/share/applications/ldm.desktop"

# Docs.
cp "$ROOT/README.md" "$STAGE/$PKG/usr/share/doc/ldm/README.md" 2>/dev/null || true
cp "$ROOT/LICENSE" "$STAGE/$PKG/usr/share/doc/ldm/copyright" 2>/dev/null || true
gzip -9 -n "$STAGE/$PKG/usr/share/doc/ldm/README.md" 2>/dev/null || true
gzip -9 -n "$STAGE/$PKG/usr/share/doc/ldm/copyright" 2>/dev/null || true

# Control file.
SIZE="$(du -sk "$STAGE/$PKG" | cut -f1)"
cat > "$STAGE/$PKG/DEBIAN/control" <<EOF
Package: ldm
Version: $VERSION
Section: net
Priority: optional
Architecture: amd64
Depends: libgtk-3-0 (>= 3.22), libayatana-appindicator3-1, libpango-1.0-0
Maintainer: LDM contributors
Description: Linux Download Manager
 A native download manager for Linux inspired by classic download managers:
 multi-connection downloads, resume, queues, scheduling, speed limiting,
 categories, browser integration and a clean GTK interface.
EOF

cat > "$STAGE/$PKG/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
exit 0
EOF
chmod +x "$STAGE/$PKG/DEBIAN/postinst"

echo "==> Building $PKG.deb ..."
dpkg-deb --build --root-owner-group "$STAGE/$PKG" "$ROOT/target/$PKG.deb"
echo "==> Done: target/$PKG.deb"
