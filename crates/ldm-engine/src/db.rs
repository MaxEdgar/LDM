//! SQLite persistence layer.
//!
//! * WAL journal mode with `synchronous=NORMAL`: crash-safe, fast, and
//!   concurrent readers don't block writers.
//! * Versioned migrations (see `migrations/`); `PRAGMA user_version` tracks
//!   the applied version.
//! * If the database file is corrupt, it is backed up (never deleted) and a
//!   fresh database is created; the event is surfaced in diagnostics.
//! * All operations run on a blocking thread via `spawn_blocking` — heavy
//!   queries never block the async engine or the UI.

use crate::error::{EngineError, Result};
use crate::model::*;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MIGRATIONS: &[(&str, &str)] = &[(
    "001_initial",
    include_str!("../migrations/001_initial.sql"),
)];

pub struct Db {
    conn: Mutex<rusqlite::Connection>,
}

impl Db {
    /// Open (or create) the database at `path`, applying migrations.
    /// On corruption, backs up the file and starts fresh.
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        match Self::open_inner(path) {
            Ok(db) => Ok(db),
            Err(e) => {
                tracing::error!("database open failed ({e}); attempting recovery");
                Self::recover_corrupt(path)?;
                Self::open_inner(path)
            }
        }
    }

    fn open_inner(path: &Path) -> Result<Arc<Self>> {
        let conn = rusqlite::Connection::open(path).map_err(|e| {
            EngineError::database(format!("cannot open database {}: {e}", path.display()))
        })?;
        Self::configure(conn)
    }

    pub fn open_in_memory() -> Result<Arc<Self>> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::configure(conn)
    }

    fn configure(conn: rusqlite::Connection) -> Result<Arc<Self>> {
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        let db = Arc::new(Db {
            conn: Mutex::new(conn),
        });
        db.migrate()?;
        Ok(db)
    }

    /// Back up a corrupt database file before replacing it. Never deletes
    /// user data silently.
    fn recover_corrupt(path: &Path) -> Result<()> {
        if path.exists() {
            let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
            let backup = path.with_extension(format!("corrupt-{ts}.db"));
            std::fs::copy(path, &backup).map_err(|e| {
                EngineError::database(format!("cannot back up corrupt database: {e}"))
            })?;
            tracing::warn!(
                "database was corrupt; backed up to {}",
                backup.display()
            );
        }
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        Ok(())
    }

    /// Run a closure on the blocking pool with exclusive connection access.
    pub async fn call<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut rusqlite::Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.conn.lock().map_err(|_| {
                EngineError::database("database connection lock poisoned")
            })?;
            f(&mut conn)
        })
        .await
        .map_err(|e| EngineError::database(format!("database worker panicked: {e}")))?
    }

    /// Run a transaction: `f` receives the transaction inside BEGIN/COMMIT.
    pub async fn transaction<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut rusqlite::Transaction) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.conn.lock().map_err(|_| {
                EngineError::database("database connection lock poisoned")
            })?;
            let mut tx = conn.transaction().map_err(EngineError::from)?;
            let res = f(&mut tx);
            match res {
                Ok(v) => {
                    tx.commit().map_err(EngineError::from)?;
                    Ok(v)
                }
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(|e| EngineError::database(format!("database worker panicked: {e}")))?
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(EngineError::from)?;
        let mut next = version;
        for (name, sql) in MIGRATIONS {
            let num: i64 = name
                .split('_')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if num > next {
                let tx = conn.transaction().map_err(EngineError::from)?;
                tx.execute_batch(sql).map_err(EngineError::from)?;
                tx.pragma_update(None, "user_version", num)
                    .map_err(EngineError::from)?;
                tx.commit().map_err(EngineError::from)?;
                next = num;
                tracing::info!("applied database migration {name}");
            }
        }
        Ok(())
    }

    /// `PRAGMA integrity_check` — used by diagnostics.
    pub async fn integrity_check(self: &Arc<Self>) -> Result<String> {
        self.call(|conn| {
            let mut stmt = conn.prepare("PRAGMA integrity_check").map_err(EngineError::from)?;
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(EngineError::from)?
                .collect::<std::result::Result<_, _>>()
                .map_err(EngineError::from)?;
            Ok(rows.join("; "))
        })
        .await
    }

    // ------------------------------------------------------------------
    // Downloads
    // ------------------------------------------------------------------

    pub async fn insert_download(
        self: &Arc<Self>,
        d: &DownloadRecord,
        headers_json: Option<&str>,
        cookies_json: Option<&str>,
    ) -> Result<i64> {
        let d = d.clone();
        let hj = headers_json.map(|s| s.to_string());
        let cj = cookies_json.map(|s| s.to_string());
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO downloads (
                    url, final_url, filename, dir_path, temp_path, category, status,
                    total_bytes, downloaded_bytes, current_speed, avg_speed, peak_speed,
                    eta_seconds, connections, priority, speed_limit, username, password_ref,
                    headers, cookies, referrer, protocol, server, content_type, etag,
                    last_modified, retry_count, max_retries, error_code, error_message,
                    error_detail, verify_hash, verify_type, verification_status, queue_name,
                    scheduled_start, created_at, started_at, completed_at, updated_at, can_resume
                ) VALUES (
                    ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                    ?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,
                    ?36,?37,?38,?39,?40,?41
                )",
                rusqlite::params![
                    d.url, d.final_url, d.filename, d.dir_path, d.temp_path, d.category,
                    d.status.as_str(), d.total_bytes, d.downloaded_bytes, d.current_speed,
                    d.avg_speed, d.peak_speed, d.eta_seconds, d.connections, d.priority as i32,
                    d.speed_limit, d.username, d.password_ref, hj, cj, d.referrer, d.protocol,
                    d.server, d.content_type, d.etag, d.last_modified, d.retry_count,
                    d.max_retries, d.error.as_ref().map(|e| e.code.clone()),
                    d.error.as_ref().map(|e| e.message.clone()),
                    d.error.as_ref().and_then(|e| e.detail.clone()),
                    d.verify_hash, d.verify_type, d.verification_status, d.queue_name,
                    d.scheduled_start, d.created_at, d.started_at, d.completed_at,
                    d.updated_at, d.can_resume as i32,
                ],
            )
            .map_err(EngineError::from)?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    pub async fn update_download(self: &Arc<Self>, d: &DownloadRecord) -> Result<()> {
        let d = d.clone();
        self.call(move |conn| {
            conn.execute(
                "UPDATE downloads SET
                    url=?1, final_url=?2, filename=?3, dir_path=?4, temp_path=?5, category=?6,
                    status=?7, total_bytes=?8, downloaded_bytes=?9, current_speed=?10,
                    avg_speed=?11, peak_speed=?12, eta_seconds=?13, connections=?14,
                    priority=?15, speed_limit=?16, username=?17, password_ref=?18,
                    referrer=?19, protocol=?20, server=?21, content_type=?22, etag=?23,
                    last_modified=?24, retry_count=?25, max_retries=?26, error_code=?27,
                    error_message=?28, error_detail=?29, verify_hash=?30, verify_type=?31,
                    verification_status=?32, queue_name=?33, scheduled_start=?34,
                    started_at=?35, completed_at=?36, updated_at=?37, can_resume=?38
                 WHERE id=?39",
                rusqlite::params![
                    d.url, d.final_url, d.filename, d.dir_path, d.temp_path, d.category,
                    d.status.as_str(), d.total_bytes, d.downloaded_bytes, d.current_speed,
                    d.avg_speed, d.peak_speed, d.eta_seconds, d.connections, d.priority as i32,
                    d.speed_limit, d.username, d.password_ref, d.referrer, d.protocol, d.server,
                    d.content_type, d.etag, d.last_modified, d.retry_count, d.max_retries,
                    d.error.as_ref().map(|e| e.code.clone()),
                    d.error.as_ref().map(|e| e.message.clone()),
                    d.error.as_ref().and_then(|e| e.detail.clone()),
                    d.verify_hash, d.verify_type, d.verification_status, d.queue_name,
                    d.scheduled_start, d.started_at, d.completed_at, d.updated_at,
                    d.can_resume as i32, d.id,
                ],
            )
            .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    /// Debounced progress write: cheap partial update (spec §12, §155).
    pub async fn update_progress(
        self: &Arc<Self>,
        id: i64,
        downloaded: i64,
        speed: u64,
        avg_speed: u64,
        peak: u64,
        eta: Option<i64>,
        status: &str,
    ) -> Result<()> {
        let status = status.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE downloads SET downloaded_bytes=?1, current_speed=?2, avg_speed=?3,
                 peak_speed=?4, eta_seconds=?5, status=?6, updated_at=?7 WHERE id=?8",
                rusqlite::params![downloaded, speed as i64, avg_speed as i64, peak as i64, eta, status, now(), id],
            )
            .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    pub async fn get_download(self: &Arc<Self>, id: i64) -> Result<Option<DownloadRecord>> {
        self.call(move |conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM downloads WHERE id=?1")
                .map_err(EngineError::from)?;
            let mut rows = stmt
                .query_map([id], row_to_download)
                .map_err(EngineError::from)?;
            Ok(rows.next().transpose().map_err(EngineError::from)?)
        })
        .await
    }

    pub async fn list_downloads(
        self: &Arc<Self>,
        filter: DownloadFilter,
        search: &str,
        sort: (SortField, SortOrder),
        category: Option<&str>,
    ) -> Result<Vec<DownloadRecord>> {
        let search = search.to_string();
        let category = category.map(|s| s.to_string());
        let clause = sort_clause(sort.0, sort.1);
        let sql = format!(
            "SELECT * FROM downloads WHERE 1=1 {} {} ORDER BY {clause}",
            match filter {
                DownloadFilter::All => "".to_string(),
                f => format!("AND status IN ({})", filter_statuses(f)),
            },
            " ".to_string()
        );
        self.call(move |conn| {
            let mut stmt = conn.prepare(&sql).map_err(EngineError::from)?;
            let mut rows = Vec::new();
            let mut q = stmt
                .query_map([], row_to_download)
                .map_err(EngineError::from)?;
            while let Some(row_res) = q.next() {
                let d = row_res.map_err(EngineError::from)?;
                if !search.is_empty()
                    && !d.filename.to_lowercase().contains(&search.to_lowercase())
                    && !d.url.to_lowercase().contains(&search.to_lowercase())
                    && !d.category.to_lowercase().contains(&search.to_lowercase())
                    && !d.status.as_str().to_lowercase().contains(&search.to_lowercase())
                {
                    continue;
                }
                if let Some(cat) = &category {
                    if &d.category != cat {
                        continue;
                    }
                }
                rows.push(d);
            }
            Ok(rows)
        })
        .await
    }

    pub async fn delete_download(self: &Arc<Self>, id: i64) -> Result<()> {
        self.call(move |conn| {
            conn.execute("DELETE FROM downloads WHERE id=?1", [id])
                .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    pub async fn set_download_status(
        self: &Arc<Self>,
        id: i64,
        status: DownloadStatus,
    ) -> Result<()> {
        let s = status.as_str().to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE downloads SET status=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![s, now(), id],
            )
            .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    pub async fn set_download_fields(
        self: &Arc<Self>,
        id: i64,
        fields: &[(&str, rusqlite::types::Value)],
    ) -> Result<()> {
        let fields: Vec<(String, rusqlite::types::Value)> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        self.call(move |conn| {
            // Placeholders: ?1..?n for the fields, then updated_at, then id.
            let set = fields
                .iter()
                .enumerate()
                .map(|(i, (k, _))| format!("{k}=?{}", i + 1))
                .collect::<Vec<_>>()
                .join(", ");
            let n = fields.len();
            let mut params: Vec<rusqlite::types::Value> =
                fields.into_iter().map(|(_, v)| v).collect();
            params.push(rusqlite::types::Value::Integer(now()));
            params.push(rusqlite::types::Value::Integer(id));
            let sql = format!(
                "UPDATE downloads SET {set}, updated_at=?{} WHERE id=?{}",
                n + 1,
                n + 2
            );
            conn.execute(&sql, rusqlite::params_from_iter(params))
                .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    // ------------------------------------------------------------------
    // Segments
    // ------------------------------------------------------------------

    pub async fn replace_segments(
        self: &Arc<Self>,
        download_id: i64,
        segments: &[SegmentRecord],
    ) -> Result<()> {
        let segs = segments.to_vec();
        self.transaction(move |conn| {
            conn.execute("DELETE FROM segments WHERE download_id=?1", [download_id])
                .map_err(EngineError::from)?;
            for s in &segs {
                conn.execute(
                    "INSERT INTO segments (download_id, start_byte, end_byte, downloaded_bytes, status, attempts, last_error, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    rusqlite::params![
                        download_id, s.start_byte, s.end_byte, s.downloaded_bytes,
                        s.status.as_str(), s.attempts, s.last_error, now(), now()
                    ],
                )
                .map_err(EngineError::from)?;
            }
            Ok(())
        })
        .await
    }

    pub async fn get_segments(self: &Arc<Self>, download_id: i64) -> Result<Vec<SegmentRecord>> {
        self.call(move |conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM segments WHERE download_id=?1 ORDER BY start_byte")
                .map_err(EngineError::from)?;
            let mut rows = stmt
                .query_map([download_id], row_to_segment)
                .map_err(EngineError::from)?;
            let mut out = Vec::new();
            while let Some(s) = rows.next().transpose().map_err(EngineError::from)? {
                out.push(s);
            }
            Ok(out)
        })
        .await
    }

    pub async fn update_segment(
        self: &Arc<Self>,
        s: &SegmentRecord,
        last_error: Option<&str>,
    ) -> Result<()> {
        let s = s.clone();
        let le = last_error.map(|x| x.to_string());
        self.call(move |conn| {
            conn.execute(
                "UPDATE segments SET downloaded_bytes=?1, status=?2, attempts=?3, last_error=?4, updated_at=?5 WHERE id=?6",
                rusqlite::params![s.downloaded_bytes, s.status.as_str(), s.attempts, le, now(), s.id],
            )
            .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    pub async fn get_settings_json(self: &Arc<Self>) -> Result<Option<String>> {
        self.call(|conn| {
            let mut stmt = conn
                .prepare("SELECT value FROM settings WHERE key='app'")
                .map_err(EngineError::from)?;
            let mut rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(EngineError::from)?;
            Ok(rows.next().transpose().map_err(EngineError::from)?)
        })
        .await
    }

    pub async fn put_settings_json(self: &Arc<Self>, json: &str) -> Result<()> {
        let json = json.to_string();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('app', ?1)
                 ON CONFLICT(key) DO UPDATE SET value=?1",
                [&json],
            )
            .map_err(EngineError::from)?;
            Ok(())
        })
        .await
    }

    // ------------------------------------------------------------------
    // Aggregations (stats, spec §48)
    // ------------------------------------------------------------------

    pub async fn stats(self: &Arc<Self>) -> Result<crate::stats::Stats> {
        self.call(|conn| {
            let total_downloaded: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(downloaded_bytes),0) FROM downloads WHERE status='COMPLETED'",
                    [],
                    |r| r.get(0),
                )
                .map_err(EngineError::from)?;
            let counts: Vec<(String, i64)> = {
                let mut stmt = conn
                    .prepare("SELECT status, COUNT(*) FROM downloads GROUP BY status")
                    .map_err(EngineError::from)?;
                let mut rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .map_err(EngineError::from)?;
                let mut v = Vec::new();
                while let Some(x) = rows.next().transpose().map_err(EngineError::from)? {
                    v.push(x);
                }
                v
            };
            Ok(crate::stats::Stats {
                total_downloaded,
                counts,
            })
        })
        .await
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn filter_statuses(f: DownloadFilter) -> String {
    let statuses: Vec<&str> = match f {
        DownloadFilter::All => vec![],
        DownloadFilter::Active => vec!["STARTING", "CONNECTING", "DOWNLOADING", "VERIFYING", "RETRYING"],
        DownloadFilter::Paused => vec!["PAUSED"],
        DownloadFilter::Completed => vec!["COMPLETED"],
        DownloadFilter::Failed => vec!["FAILED"],
        DownloadFilter::Queued => vec!["QUEUED"],
        DownloadFilter::Scheduled => vec!["SCHEDULED"],
        DownloadFilter::Cancelled => vec!["CANCELLED"],
    };
    statuses
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",")
}

fn row_to_download(row: &rusqlite::Row) -> rusqlite::Result<DownloadRecord> {
    let status_str: String = row.get("status")?;
    let status = match status_str.as_str() {
        "QUEUED" => DownloadStatus::Queued,
        "STARTING" => DownloadStatus::Starting,
        "CONNECTING" => DownloadStatus::Connecting,
        "DOWNLOADING" => DownloadStatus::Downloading,
        "PAUSED" => DownloadStatus::Paused,
        "COMPLETED" => DownloadStatus::Completed,
        "FAILED" => DownloadStatus::Failed,
        "CANCELLED" => DownloadStatus::Cancelled,
        "VERIFYING" => DownloadStatus::Verifying,
        "RETRYING" => DownloadStatus::Retrying,
        "SCHEDULED" => DownloadStatus::Scheduled,
        other => {
            tracing::warn!("unknown status in db: {other}");
            DownloadStatus::Paused
        }
    };
    let error = match (
        row.get::<_, Option<String>>("error_code")?,
        row.get::<_, Option<String>>("error_message")?,
    ) {
        (Some(code), Some(message)) => Some(ErrorInfo {
            kind: code.clone(),
            code,
            message,
            detail: row.get("error_detail").ok().flatten(),
        }),
        _ => None,
    };
    Ok(DownloadRecord {
        id: row.get("id")?,
        url: row.get("url")?,
        final_url: row.get("final_url").ok().flatten(),
        filename: row.get("filename")?,
        dir_path: row.get("dir_path")?,
        temp_path: row.get("temp_path").ok().flatten(),
        category: row.get("category")?,
        status,
        total_bytes: row.get("total_bytes").ok().flatten(),
        downloaded_bytes: row.get("downloaded_bytes")?,
        current_speed: row.get::<_, i64>("current_speed")?.max(0) as u64,
        avg_speed: row.get::<_, i64>("avg_speed")?.max(0) as u64,
        peak_speed: row.get::<_, i64>("peak_speed")?.max(0) as u64,
        eta_seconds: row.get("eta_seconds").ok().flatten(),
        connections: row.get("connections")?,
        priority: Priority::from_i32(row.get("priority")?),
        speed_limit: row.get("speed_limit").ok().flatten(),
        username: row.get("username").ok().flatten(),
        password_ref: row.get("password_ref").ok().flatten(),
        referrer: row.get("referrer").ok().flatten(),
        protocol: row.get("protocol")?,
        server: row.get("server").ok().flatten(),
        content_type: row.get("content_type").ok().flatten(),
        etag: row.get("etag").ok().flatten(),
        last_modified: row.get("last_modified").ok().flatten(),
        retry_count: row.get("retry_count")?,
        max_retries: row.get("max_retries").ok().flatten().unwrap_or(0),
        error,
        verify_hash: row.get("verify_hash").ok().flatten(),
        verify_type: row.get("verify_type").ok().flatten(),
        verification_status: row.get("verification_status").ok().flatten(),
        queue_name: row.get("queue_name").ok().flatten(),
        scheduled_start: row.get("scheduled_start").ok().flatten(),
        created_at: row.get("created_at")?,
        started_at: row.get("started_at").ok().flatten(),
        completed_at: row.get("completed_at").ok().flatten(),
        updated_at: row.get("updated_at")?,
        can_resume: row.get::<_, i64>("can_resume")? != 0,
    })
}

fn row_to_segment(row: &rusqlite::Row) -> rusqlite::Result<SegmentRecord> {
    let status_str: String = row.get("status")?;
    let status = match status_str.as_str() {
        "PENDING" => SegmentStatus::Pending,
        "ACTIVE" => SegmentStatus::Active,
        "COMPLETED" => SegmentStatus::Completed,
        "FAILED" => SegmentStatus::Failed,
        _ => SegmentStatus::Pending,
    };
    Ok(SegmentRecord {
        id: row.get("id")?,
        download_id: row.get("download_id")?,
        start_byte: row.get("start_byte")?,
        end_byte: row.get("end_byte").ok().flatten(),
        downloaded_bytes: row.get("downloaded_bytes")?,
        status,
        attempts: row.get("attempts")?,
        last_error: row.get("last_error").ok().flatten(),
    })
}
