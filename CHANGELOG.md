# Changelog

All notable changes to LDM are documented here.

## [0.1.0] - 2026-08-18

First release. Native Rust + GTK3 download manager.

### Download engine

- HTTP/HTTPS downloads with redirects, cookies, Basic/Bearer auth and custom
  headers.
- Multi-connection segmented downloads (up to 32 connections) with automatic
  single-connection fallback when the server does not support byte ranges.
- Resume of interrupted downloads across app restarts and process crashes,
  with per-segment reconciliation and integrity checks.
- Automatic retries with exponential backoff + jitter for transient errors
  (429, 5xx, timeouts, resets); permanent errors (404, 403, redirect loops)
  fail immediately.
- Global and per-download token-bucket rate limiting.
- 64-bit byte offsets throughout (large files), disk-space checks, atomic
  temp-file install on completion.
- Optional SHA-256 / SHA-512 / MD5 verification.

### Desktop UI (GTK3)

- Main window with sidebar (library views, categories, queues), searchable
  download list with progress, speed, ETA and status, and a status bar.
- Add Download dialog with URL probing: filename, category and size are
  auto-detected while you type.
- Live transfer-rate graph and per-connection progress bars for the selected
  download.
- Download properties dialog (network, server metadata, verification,
  segments), context menu, keyboard shortcuts.
- System tray (pause/resume-all, speed limit, quit) and desktop notifications.
- Dark / light / system themes.

### Productivity

- Queues with reordering and concurrency limits; scheduler with start/stop
  time, days and speed limit; automatic categories by file extension.
- Clipboard monitoring, download history, import/export (JSON/CSV).

### Browser integration

- Native messaging host (`ldm-native-host`) talking to the engine over an
  authenticated 0600 Unix socket with a per-run token.
- Chrome / Chromium / Edge / Brave / Firefox extension (Manifest V3) that
  captures matching downloads, optionally sends cookies, and excludes login
  and banking pages.

### CLI

- `ldm download|fetch|probe|list|pause|resume|cancel|remove`.

### Infrastructure

- Local demo test server for integration tests (no internet required).
- Integration tests for multi-connection integrity, resume after crash,
  retries, rate limiting, queue limits, scheduling and the browser IPC surface.
- Packaging scripts for .deb and AppImage; desktop entry and icons.
