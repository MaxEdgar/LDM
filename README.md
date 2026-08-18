# LDM - Linux Download Manager

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux-lightgrey.svg)](https://github.com/MaxEdgar/LDM)
[![Version](https://img.shields.io/github/v/release/MaxEdgar/LDM)](https://github.com/MaxEdgar/LDM/releases)
[![Build](https://img.shields.io/github/actions/workflow/status/MaxEdgar/LDM/ci.yml?branch=main)](https://github.com/MaxEdgar/LDM/actions)
[![Downloads](https://img.shields.io/github/downloads/MaxEdgar/LDM/total)](https://github.com/MaxEdgar/LDM/releases)

A fast, reliable, native Linux download manager. LDM downloads files over
HTTP/HTTPS with **multi-connection (segmented) downloads**, **resume**, queues,
scheduling, speed limits, categories, notifications, a system tray, clipboard
monitoring and **browser integration for Chrome, Chromium, Edge, Brave and
Firefox**.

It is a single native application - a Rust download engine with a GTK3 desktop
UI (no Electron, no webview). The engine is a separate crate (`ldm-engine`)
that can be reused by other frontends.

---

## Install

### One-liner (AppImage)

```bash
curl -fsSL https://raw.githubusercontent.com/MaxEdgar/LDM/main/scripts/install.sh | bash
```

Downloads the latest release AppImage to `~/.local/bin/ldm` and registers the
desktop entry. Set `LDM_PREFIX=/usr/local` to install system-wide. If no
release exists yet it builds from source instead.

### Packages

| Format | Install |
| --- | --- |
| AppImage | download `LDM-x86_64.AppImage` from [Releases](https://github.com/MaxEdgar/LDM/releases), `chmod +x`, run |
| Debian/Ubuntu | `sudo apt install ./ldm_<version>_amd64.deb` |

### From source

Requires Rust (stable) and GTK3 dev headers:

```bash
# Debian/Ubuntu
sudo apt install build-essential pkg-config libgtk-3-dev libayatana-appindicator3-dev

git clone https://github.com/MaxEdgar/LDM.git
cd LDM
cargo build --release
# Binaries in target/release/: ldm-gui, ldm, ldm-native-host
```

---

## Usage

- **GUI**: run `ldm-gui` (or launch from the applications menu).
- **CLI**:

  ```bash
  ldm download "https://example.com/file.iso"   # download now
  ldm probe "https://example.com/file.iso"       # size / range support
  ldm list                                      # list downloads
  ldm pause 3 && ldm resume 3                   # control by id
  ```

Basic workflow: open LDM, press **Ctrl+N** (or the *Add Download* button),
paste a URL, let LDM auto-detect the filename/category/size, and click
*Add Download*. Watch progress, pause/resume from the row or context menu, and
find the file in your download folder when it finishes.

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| Ctrl+N | Add download |
| Ctrl+F | Search |
| Delete | Remove selected download |
| Ctrl+Q | Quit |
| Ctrl+, | Settings |

---

## Features

| Feature | Status |
| --- | --- |
| HTTP/HTTPS downloads (redirects, cookies, auth, custom headers) | Yes |
| Multi-connection segmented downloads (up to 32 connections) | Yes |
| Single-connection fallback when the server has no range support | Yes |
| Resume interrupted downloads (survives restarts and crashes) | Yes |
| Queues with reordering and concurrency limits | Yes |
| Scheduler (start/stop time, days, speed limit) | Yes |
| Global and per-download speed limits | Yes |
| Retries with exponential backoff and jitter | Yes |
| Categories auto-detected from file extension | Yes |
| Filename/size auto-detection in the Add dialog | Yes |
| Live transfer-rate graph and per-connection progress bars | Yes |
| System tray with pause/resume-all and speed limit | Yes |
| Desktop notifications | Yes |
| Dark / light / system themes | Yes |
| Clipboard monitoring | Yes |
| Browser integration (Chrome, Chromium, Edge, Brave, Firefox) | Yes |
| Import/export download history (JSON/CSV) | Yes |
| Verification (size + SHA-256/SHA-512/MD5 hashes) | Yes |
| Large files (64-bit offsets, >4 GB) | Yes |
| Disk-space checks before/during downloads | Yes |
| Command-line interface | Yes |

---

## Browser integration

LDM can take over downloads from your browser - with the same cookies and
referrer the browser would use, so login-gated downloads work.

### 1. Install the native host

The native host is a small binary that lets the browser talk to the running
LDM app over a secure local socket.

```bash
# from a source checkout
./browser/install.sh
```

This builds `ldm-native-host`, installs it to `~/.local/lib/ldm/` and
registers the Firefox manifest. For Chrome/Chromium you must pass the extension
ID (see step 3):

```bash
CHROME_EXT_ID=<extension-id> ./browser/install.sh
```

### 2. Load the extension

Load the unpacked extension from `browser/extension` (a source checkout) or
install a packed build:

- **Chrome / Chromium / Edge / Brave**: open `chrome://extensions`, enable
  *Developer mode*, click *Load unpacked*, select `browser/extension`.
- **Firefox**: open `about:debugging#/runtime/this-firefox`, click *Load
  Temporary Add-on*, select `browser/extension/manifest.json`.

### 3. Connect Chrome to the native host

Copy the extension ID shown on `chrome://extensions` and register the host:

```bash
CHROME_EXT_ID=abcdefghijklmnop ./browser/install.sh
```

Firefox uses the fixed extension ID `ldm-download@ldm.app` and needs no extra
step.

### 4. Configure

- In LDM: open **Settings -> Browser** and enable *Browser integration*. The
  capture-extension list there is synced with the extension's own rules.
- In the extension (click its toolbar icon): toggle auto-capture, decide
  whether to send cookies, and edit the list of file extensions and excluded
  hosts.

### How interception works

When a download matches your rules, the extension cancels the browser's own
download and hands the URL, referrer and (optionally) cookies to LDM, which
downloads it with multi-connection + resume. With auto-capture off you get a
notification with a *Download with LDM* button instead. Login, banking and
account pages are always excluded.

---

## Development

```bash
cargo build                                   # build everything (debug)
cargo run -p ldm-gui                          # run the GUI
cargo run -p ldm -- download <url>            # CLI
cargo test                                    # unit + integration tests
cargo test -p ldm-native-host                 # browser host end-to-end test
cargo clippy --all-targets                    # lint
cargo fmt                                     # format
```

The integration tests use a local demo server (`ldm-test-server`) that can
simulate slow downloads, missing range support, HTTP 404/429/500, redirects
and connection drops - no third-party websites are involved.

## Packaging

```bash
./packaging/build-deb.sh            # -> target/ldm_<version>_amd64.deb
./packaging/build-appimage.sh       # -> target/LDM-x86_64.AppImage
```

## Troubleshooting

- **HTTP 403 on a download**: the site blocks non-browser requests. Enable
  browser integration (or add the referrer/cookies in the Add dialog) so the
  download is made with the same context as your browser.
- **No tray icon**: install `libayatana-appindicator3` (runtime dependency).
- **Logs**: LDM logs to stderr - run `ldm-gui 2> ldm.log` and attach the file
  when reporting a bug.
- **Where is my data?** The database is in `~/.local/share/ldm/ldm.db`;
  incomplete downloads live in `.ldm/` next to the destination folder.

## Security and privacy

- Credentials (passwords, bearer tokens, cookies) are never written to logs;
  passwords are stored in the OS keyring when available.
- Downloaded files are never executed automatically.
- Server-provided filenames are sanitized so they cannot escape the download
  folder (`../` and absolute paths are neutralized).
- The browser host talks to LDM over a user-owned 0600 Unix socket with a
  per-run random token; only `ping` and `add_download` operations exist.

## Documentation

- [Browser integration](browser/README.md)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)

## License

MIT - see [LICENSE](LICENSE).
