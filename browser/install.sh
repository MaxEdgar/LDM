#!/usr/bin/env bash
# Install the LDM native messaging host for Chrome / Chromium / Firefox.
#
# Usage:
#   ./browser/install.sh                 # per-user install (no root needed)
#   CHROME_EXT_ID=abcdefgh... ./browser/install.sh
#
# For Chrome/Chromium the extension ID is required (see the notes at the end
# of this script). Firefox uses a fixed extension ID.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST_DIR="${LDM_HOST_DIR:-$HOME/.local/lib/ldm}"
HOST_BIN="$DEST_DIR/ldm-native-host"

echo "==> Building native host (release)..."
cargo build --release -p ldm-native-host --manifest-path "$ROOT/Cargo.toml"
mkdir -p "$DEST_DIR"
cp "$ROOT/target/release/ldm-native-host" "$HOST_BIN"
chmod +x "$HOST_BIN"
echo "    installed: $HOST_BIN"

# --- Firefox -------------------------------------------------------------
FF_DIR="$HOME/.mozilla/native-messaging-hosts"
mkdir -p "$FF_DIR"
sed "s|/PATH/TO/ldm-native-host|$HOST_BIN|g" \
  "$ROOT/browser/native-host/ldm.firefox.json" > "$FF_DIR/ldm.json"
echo "==> Firefox host manifest: $FF_DIR/ldm.json"

# --- Chromium ------------------------------------------------------------
CHROME_DIRS=()
if [ -n "${CHROME_EXT_ID:-}" ]; then
  CHROME_DIRS+=("$HOME/.config/google-chrome/NativeMessagingHosts")
  CHROME_DIRS+=("$HOME/.config/chromium/NativeMessagingHosts")
  CHROME_DIRS+=("$HOME/.config/microsoft-edge/NativeMessagingHosts")
  CHROME_DIRS+=("$HOME/.config/brave/NativeMessagingHosts")
  for d in "${CHROME_DIRS[@]}"; do
    mkdir -p "$d"
    sed -e "s|/PATH/TO/ldm-native-host|$HOST_BIN|g" \
        -e "s|__CHROME_EXT_ID__|$CHROME_EXT_ID|g" \
        "$ROOT/browser/native-host/ldm.chrome.json" > "$d/ldm.json"
    echo "==> Chrome host manifest: $d/ldm.json"
  done
else
  echo "==> Skipping Chrome/Chromium (set CHROME_EXT_ID to register those)."
fi

echo
echo "Done. To finish:"
echo "  1. Load the unpacked extension from $ROOT/browser/extension in your browser:"
echo "     - Chrome/Edge/Brave: chrome://extensions -> Developer mode -> Load unpacked"
echo "     - Firefox: about:debugging#/runtime/this-firefox -> Load Temporary Add-on (manifest.json)"
echo "  2. If you skipped Chrome registration, copy the extension ID from"
echo "     chrome://extensions (it is shown under the extension name) and re-run:"
echo "       CHROME_EXT_ID=<id> $0"
echo "  3. Start the LDM desktop app, then enable 'Browser integration' in"
echo "     LDM Settings -> Browser."
