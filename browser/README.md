# LDM Browser Integration

This directory contains everything needed to send downloads from a browser to
the LDM desktop app:

| Path | Purpose |
| --- | --- |
| `extension/` | Chrome / Firefox extension (Manifest V3) |
| `native-host/` | Native messaging host (`ldm-native-host` crate) |
| `install.sh` | Builds the host and registers it with Firefox / Chrome / Chromium |

## How it works

```
Browser download starts
      |
      v
Extension (background.js) matches it against your capture rules
      |
      v
Extension cancels the browser download and sends URL + referrer + cookies
      |
      v
Native host (ldm-native-host) forwards the request over the engine's
authenticated Unix socket ($XDG_RUNTIME_DIR/ldm/ldm.sock, token in ldm.token)
      |
      v
LDM downloads the file with multi-connection + resume + scheduling
```

Security properties:

- The socket is owned by the current user, mode 0600.
- Every request must carry a per-run random token read from a 0600 file.
- Only two operations exist: `ping` and `add_download`. No arbitrary commands,
  no filesystem access beyond the download itself.
- Only `http`/`https` URLs are accepted; URLs are capped at 4096 bytes.
- Login, banking and account pages are never intercepted (host- and
  keyword-based exclusions, plus a hard-coded denylist in the extension).

## Install

Prerequisites: a source checkout and the LDM desktop app running.

```bash
# Firefox (and any Chromium browser once the ID is set):
./browser/install.sh

# Chrome / Chromium / Edge / Brave:
CHROME_EXT_ID=<id-from-chrome://extensions> ./browser/install.sh
```

Then load the unpacked extension:

- Chrome: `chrome://extensions` -> Developer mode -> Load unpacked ->
  `browser/extension`
- Firefox: `about:debugging#/runtime/this-firefox` -> Load Temporary Add-on ->
  `browser/extension/manifest.json`

## Configuration

Two places control behaviour; both must be enabled:

1. **LDM desktop app** - Settings -> Browser -> *Browser integration* (the IPC
   socket only exists while this is on and the app is running).
2. **Extension options** (toolbar icon, or right-click the extension):
   - Enable/disable interception entirely.
   - Auto-capture matching downloads, or ask first via notification.
   - Send cookies with captured downloads (needed for login-gated files).
   - File extensions to capture (e.g. `.iso`, `.zip`, `.mp4`).
   - Hosts to exclude (e.g. `accounts.google.com`).

## Testing

The native host has an end-to-end test that spawns the real binary and
verifies `ping`, `add_download` (with an actual completed download), bad-input
handling and URL validation - all against the local test server, no internet:

```bash
cargo test -p ldm-native-host
```

The engine-side socket (token auth, protocol versioning, size limits) is
covered by `cargo test -p ldm-engine --test download_integration
browser_ipc_authenticated_requests`.

## Troubleshooting

- **"LDM is not running"**: start the desktop app and enable
  Settings -> Browser -> *Browser integration*.
- **Chrome says the host is not allowed**: the `CHROME_EXT_ID` in
  `ldm.chrome.json` must match the extension ID from `chrome://extensions`
  exactly (it changes if you reload from a different path). Re-run
  `browser/install.sh` with the correct ID.
- **Downloads not intercepted**: check the capture-extension list in the
  extension options and that the URL host is not excluded.
