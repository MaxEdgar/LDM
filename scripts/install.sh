#!/usr/bin/env bash
# LDM one-liner installer.
#
#   curl -fsSL https://raw.githubusercontent.com/MaxEdgar/LDM/main/scripts/install.sh | bash
#
# Downloads the latest release for your architecture. If no release is
# published yet (or the download fails), it builds LDM from source instead.
set -euo pipefail

REPO="MaxEdgar/LDM"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64)  APP_ARCH="x86_64" ;;
  aarch64|arm64) APP_ARCH="aarch64" ;;
  *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

INSTALL_PREFIX="${LDM_PREFIX:-$HOME/.local}"
DEST_BIN="$INSTALL_PREFIX/bin"

msg()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# --- Try the latest GitHub release first -----------------------------------
LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
if RELEASE_JSON="$(curl -fsSL --max-time 15 "$LATEST_URL" 2>/dev/null || true)"; then
  ASSET_URL="$(printf '%s' "$RELEASE_JSON" \
    | grep -oE '"browser_download_url": *"[^"]*LDM-[^"]*\.AppImage"' \
    | head -1 | sed -E 's/.*"browser_download_url": *"([^"]*)".*/\1/')"
  VERSION="$(printf '%s' "$RELEASE_JSON" | grep -oE '"tag_name": *"[^"]*"' | head -1 | sed -E 's/.*"([^"]*)".*/\1/')"
  if [ -n "$ASSET_URL" ]; then
    TMP="$(mktemp -d)"
    trap 'rm -rf "$TMP"' EXIT
    APPIMAGE="$TMP/LDM-$APP_ARCH.AppImage"
    msg "Downloading LDM $VERSION ($APP_ARCH)..."
    curl -fL --progress-bar "$ASSET_URL" -o "$APPIMAGE" || {
      msg "Release download failed; falling back to a source build."
      rm -rf "$TMP"
      ASSET_URL=""
    }
    if [ -n "$ASSET_URL" ] && [ -s "$APPIMAGE" ]; then
      mkdir -p "$DEST_BIN" "$INSTALL_PREFIX/share/applications" "$INSTALL_PREFIX/share/icons/hicolor/512x512/apps"
      chmod +x "$APPIMAGE"
      install -m 0755 "$APPIMAGE" "$INSTALL_PREFIX/bin/ldm"
      # Desktop entry pointing at the AppImage.
      ICON_SRC="$TMP/icon.png"
      curl -fsSL --max-time 10 "https://raw.githubusercontent.com/$REPO/main/assets/icon.png" -o "$ICON_SRC" || true
      [ -s "$ICON_SRC" ] && install -m 0644 "$ICON_SRC" "$INSTALL_PREFIX/share/icons/hicolor/512x512/apps/ldm.png"
      cat > "$INSTALL_PREFIX/share/applications/ldm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=LDM
GenericName=Download Manager
Comment=Download files over HTTP/HTTPS with multi-connection, resume, queue and scheduling support
Exec=$INSTALL_PREFIX/bin/ldm %U
Icon=ldm
Terminal=false
Categories=Network;FileTransfer;Utility;
MimeType=x-scheme-handler/http;x-scheme-handler/https;
EOF
      msg "Installed to $INSTALL_PREFIX/bin/ldm"
      echo
      echo "Launch it with:  ldm"
      echo "Add it to your app menu with:  update-desktop-database $INSTALL_PREFIX/share/applications"
      exit 0
    fi
  fi
fi

# --- Fallback: build from source -------------------------------------------
msg "Building LDM from source (this requires Rust + GTK3 dev libraries)..."
if ! command -v cargo >/dev/null 2>&1; then
  die "cargo not found. Install Rust first: https://rustup.rs"
fi
if [ ! -d "$HOME/.rustup" ] && ! pkg-config --exists gtk+-3.0 2>/dev/null; then
  echo "GTK3 development files are required (e.g. libgtk-3-dev on Debian/Ubuntu)."
fi

SRC="$(mktemp -d)"
trap 'rm -rf "$SRC"' EXIT
msg "Cloning $REPO..."
git clone --depth 1 "https://github.com/$REPO.git" "$SRC/ldm"
cd "$SRC/ldm"
cargo build --release -p ldm-gui -p ldm-cli -p ldm-native-host
mkdir -p "$DEST_BIN"
install -m 0755 target/release/ldm-gui "$DEST_BIN/ldm"
install -m 0755 target/release/ldm "$DEST_BIN/ldm-cli"
msg "Installed: $DEST_BIN/ldm (GUI), $DEST_BIN/ldm-cli"
echo
echo "Launch it with:  ldm"
