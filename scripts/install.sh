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

# Write a per-user desktop entry + icon (used by AppImage/tarball installs).
write_desktop_entry() {
  mkdir -p "$INSTALL_PREFIX/share/applications" "$INSTALL_PREFIX/share/icons/hicolor/512x512/apps"
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
}

# --- Try the latest GitHub release first -----------------------------------
LATEST_URL="https://api.github.com/repos/$REPO/releases/latest"
if RELEASE_JSON="$(curl -fsSL --max-time 15 "$LATEST_URL" 2>/dev/null || true)"; then
  VERSION="$(printf '%s' "$RELEASE_JSON" | grep -oE '"tag_name": *"[^"]*"' | head -1 | sed -E 's/.*"([^"]*)".*/\1/' || true)"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT

  # Prefer an AppImage, then the tarball, then the .deb.
  APPIMAGE_URL="$(printf '%s' "$RELEASE_JSON" \
    | grep -oE '"browser_download_url": *"[^"]*LDM-[^"]*\.AppImage"' \
    | head -1 | sed -E 's/.*"browser_download_url": *"([^"]*)".*/\1/' || true)"
  TARBALL_URL="$(printf '%s' "$RELEASE_JSON" \
    | grep -oE '"browser_download_url": *"[^"]*ldm-[^"]*linux-'"$APP_ARCH"'\.tar\.gz"' \
    | head -1 | sed -E 's/.*"browser_download_url": *"([^"]*)".*/\1/' || true)"
  DEB_URL="$(printf '%s' "$RELEASE_JSON" \
    | grep -oE '"browser_download_url": *"[^"]*ldm_[^"]*_amd64\.deb"' \
    | head -1 | sed -E 's/.*"browser_download_url": *"([^"]*)".*/\1/' || true)"

  install_binary() { # $1 = downloaded file, $2 = name to install as
    mkdir -p "$DEST_BIN"
    chmod +x "$1"
    install -m 0755 "$1" "$DEST_BIN/$2"
  }

  if [ -n "$APPIMAGE_URL" ]; then
    msg "Downloading LDM $VERSION AppImage..."
    if curl -fL --progress-bar "$APPIMAGE_URL" -o "$TMP/LDM.AppImage" && [ -s "$TMP/LDM.AppImage" ]; then
      install_binary "$TMP/LDM.AppImage" ldm
      write_desktop_entry
      msg "Installed to $INSTALL_PREFIX/bin/ldm"
      echo "Launch it with:  ldm"
      exit 0
    fi
    msg "AppImage download failed; trying the tarball."
  fi

  if [ -n "$TARBALL_URL" ]; then
    msg "Downloading LDM $VERSION tarball..."
    if curl -fL --progress-bar "$TARBALL_URL" -o "$TMP/ldm.tar.gz" && [ -s "$TMP/ldm.tar.gz" ]; then
      mkdir -p "$TMP/extract"
      tar xzf "$TMP/ldm.tar.gz" -C "$TMP/extract"
      install_binary "$TMP/extract/ldm-gui" ldm
      install_binary "$TMP/extract/ldm" ldm-cli
      mkdir -p "$INSTALL_PREFIX/lib/ldm"
      install -m 0755 "$TMP/extract/ldm-native-host" "$INSTALL_PREFIX/lib/ldm/ldm-native-host"
      write_desktop_entry
      msg "Installed to $INSTALL_PREFIX/bin/ldm (GUI) and $INSTALL_PREFIX/bin/ldm-cli"
      echo "Launch it with:  ldm"
      exit 0
    fi
    msg "Tarball download failed; trying the .deb."
  fi

  if [ -n "$DEB_URL" ]; then
    msg "Downloading LDM $VERSION .deb..."
    if curl -fL --progress-bar "$DEB_URL" -o "$TMP/ldm.deb" && [ -s "$TMP/ldm.deb" ]; then
      if command -v sudo >/dev/null 2>&1; then
        sudo dpkg -i "$TMP/ldm.deb" && { echo "LDM $VERSION installed system-wide."; exit 0; }
      else
        dpkg -i "$TMP/ldm.deb" && { echo "LDM $VERSION installed system-wide."; exit 0; }
      fi
      msg "dpkg install failed; falling back to a source build."
    fi
  fi
  rm -rf "$TMP"
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
