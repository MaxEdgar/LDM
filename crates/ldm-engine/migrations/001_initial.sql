-- LDM schema migration 001: initial tables.
-- Applied inside a transaction; user_version is advanced by the migrator.

CREATE TABLE IF NOT EXISTS downloads (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    url               TEXT NOT NULL,
    final_url         TEXT,
    filename          TEXT NOT NULL,
    dir_path          TEXT NOT NULL,
    temp_path         TEXT,
    category          TEXT NOT NULL DEFAULT 'General',
    status            TEXT NOT NULL DEFAULT 'QUEUED',
    total_bytes       INTEGER,
    downloaded_bytes  INTEGER NOT NULL DEFAULT 0,
    current_speed     INTEGER NOT NULL DEFAULT 0,
    avg_speed         INTEGER NOT NULL DEFAULT 0,
    peak_speed        INTEGER NOT NULL DEFAULT 0,
    eta_seconds       INTEGER,
    connections       INTEGER NOT NULL DEFAULT 8,
    priority          INTEGER NOT NULL DEFAULT 0,
    speed_limit       INTEGER,
    username          TEXT,
    password_ref      TEXT,
    headers           TEXT,
    cookies           TEXT,
    referrer          TEXT,
    protocol          TEXT NOT NULL DEFAULT 'http',
    server            TEXT,
    content_type      TEXT,
    etag              TEXT,
    last_modified     TEXT,
    retry_count       INTEGER NOT NULL DEFAULT 0,
    max_retries       INTEGER,
    error_code        TEXT,
    error_message     TEXT,
    error_detail      TEXT,
    verify_hash       TEXT,
    verify_type       TEXT,
    verification_status TEXT,
    queue_name        TEXT,
    scheduled_start   INTEGER,
    created_at        INTEGER NOT NULL,
    started_at        INTEGER,
    completed_at      INTEGER,
    updated_at        INTEGER NOT NULL,
    can_resume        INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_downloads_status   ON downloads(status);
CREATE INDEX IF NOT EXISTS idx_downloads_created  ON downloads(created_at);
CREATE INDEX IF NOT EXISTS idx_downloads_category ON downloads(category);

CREATE TABLE IF NOT EXISTS segments (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    download_id      INTEGER NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    start_byte       INTEGER NOT NULL,
    end_byte         INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    status           TEXT NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER NOT NULL DEFAULT 0,
    last_error       TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_segments_download ON segments(download_id);

CREATE TABLE IF NOT EXISTS categories (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    dir_path    TEXT,
    extensions  TEXT NOT NULL DEFAULT '[]',
    is_builtin  INTEGER NOT NULL DEFAULT 0,
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS queues (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    max_active  INTEGER NOT NULL DEFAULT 3,
    is_default  INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS queue_items (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_id    INTEGER NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
    download_id INTEGER NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    UNIQUE(queue_id, download_id)
);

CREATE TABLE IF NOT EXISTS schedules (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT NOT NULL,
    enabled        INTEGER NOT NULL DEFAULT 1,
    start_time     TEXT NOT NULL,
    stop_time      TEXT,
    days           INTEGER NOT NULL DEFAULT 127,
    speed_limit    INTEGER,
    max_active     INTEGER,
    queue_id       INTEGER,
    action         TEXT NOT NULL DEFAULT 'none',
    created_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
