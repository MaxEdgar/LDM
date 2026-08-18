//! The public engine API (spec §78). The UI, CLI, and browser-integration
//! layers talk to the manager; it owns the task map, queue, scheduler,
//! settings, and persistence.

use crate::categories;
use crate::db::Db;
use crate::error::{EngineError, Result};
use crate::events::EngineEvent;
use crate::http::HttpProtocol;
use crate::model::{
    DownloadFilter, DownloadRecord, DownloadStatus, Priority, SortField, SortOrder,
};
use crate::protocol::{DownloadProtocol, ProbeInfo, RequestOptions};
use crate::rate::TokenBucket;
use crate::settings::{DuplicatePolicy, Settings};
use crate::task::{spawn_task, DownloadTask, TaskCmd, TaskContext};
use crate::{ipc_server, pathutil, urlutil, verify};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc};

/// Messages the background manager loop reacts to.
#[derive(Debug)]
pub enum ManagerMsg {
    DownloadFinished(i64),
    DownloadFailed(i64),
    DownloadCancelled(i64),
    NetworkOffline,
    NetworkOnline,
    IpcRequest(ipc_server::IpcRequest),
}

#[derive(Clone)]
pub struct ManagerConfig {
    pub db_path: PathBuf,
    /// Runtime data dir for the IPC socket / token (browser integration).
    pub runtime_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Read-only access: skip the startup reconciliation and the background
    /// loops (scheduler/clipboard/IPC). Used by the CLI for `list`/`probe` so
    /// it never disturbs downloads managed by the running desktop app.
    pub read_only: bool,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        let data = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ldm");
        let runtime = dirs::runtime_dir()
            .map(|d| d.join("ldm"))
            .unwrap_or_else(|| data.join("run"));
        Self {
            db_path: data.join("ldm.db"),
            runtime_dir: runtime,
            data_dir: data,
            read_only: false,
        }
    }
}

/// Options for adding a download.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddDownloadOptions {
    pub url: String,
    pub filename: Option<String>,
    pub dir: Option<String>,
    pub category: Option<String>,
    pub connections: Option<i32>,
    pub start_immediately: bool,
    pub queue: Option<String>,
    pub scheduled_start: Option<i64>,
    pub speed_limit: Option<i64>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub referrer: Option<String>,
    pub verify_hash: Option<String>,
    pub verify_type: Option<String>,
}

/// Result of `add_download`: either added, or needs a duplicate-file decision.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddOutcome {
    Added { download: DownloadRecord },
    NeedsDuplicateDecision {
        url: String,
        filename: String,
        dir: String,
        existing_path: String,
    },
    Skipped { reason: String },
}

/// A queue definition (loaded from the `queues` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDef {
    pub id: i64,
    pub name: String,
    pub max_active: i32,
    pub is_default: bool,
    /// Disabled while a schedule window is inactive (scheduler gating).
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDef {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    /// "HH:MM" 24h.
    pub start_time: String,
    pub stop_time: Option<String>,
    /// Bitmask: bit 0 = Monday … bit 6 = Sunday.
    pub days: i32,
    pub speed_limit: Option<i64>,
    pub max_active: Option<i32>,
    pub queue_id: Option<i64>,
    /// "none" | "shutdown" (shutdown requires explicit user confirmation).
    pub action: String,
}

pub struct DownloadManager {
    pub db: Arc<Db>,
    pub settings: Arc<tokio::sync::RwLock<Settings>>,
    pub http: Arc<HttpProtocol>,
    pub events: broadcast::Sender<EngineEvent>,
    pub global_rate: Arc<TokenBucket>,
    tasks: RwLock<HashMap<i64, Arc<DownloadTask>>>,
    mgr_tx: mpsc::UnboundedSender<ManagerMsg>,
    queues: RwLock<HashMap<String, QueueDef>>,
    /// In-memory credentials (never persisted to disk).
    credentials: RwLock<HashMap<i64, (String, String)>>,
    offline: std::sync::atomic::AtomicBool,
}

impl DownloadManager {
    /// Create the manager inside a running Tokio runtime.
    pub async fn new(config: ManagerConfig) -> Result<Arc<Self>> {
        let db = Db::open(&config.db_path)?;
        let settings_json = db.get_settings_json().await?;
        let settings = Arc::new(tokio::sync::RwLock::new(match settings_json {
            Some(j) => Settings::from_json(&j),
            None => {
                let s = Settings::default();
                let _ = db.put_settings_json(&s.to_json()).await;
                s
            }
        }));
        let http = Arc::new(HttpProtocol::new(settings.clone()));
        let global_rate = TokenBucket::new(settings.read().await.global_speed_limit.unwrap_or(0) as u64);
        let (events_tx, _) = broadcast::channel(1024);
        let (mgr_tx, mgr_rx) = mpsc::unbounded_channel();

        let this = Arc::new(Self {
            db,
            settings,
            http,
            events: events_tx,
            global_rate,
            tasks: RwLock::new(HashMap::new()),
            mgr_tx,
            queues: RwLock::new(HashMap::new()),
            credentials: RwLock::new(HashMap::new()),
            offline: std::sync::atomic::AtomicBool::new(false),
        });

        this.load_queues().await?;
        if !config.read_only {
            this.reconcile_startup().await?;
        }

        // Read-only managers (CLI list/probe) skip all background loops so
        // they never reconcile, resume, or otherwise disturb active downloads
        // owned by the desktop app (spec §44, §46).
        if !config.read_only {
            let mgr = this.clone();
            tokio::spawn(async move {
                mgr.manager_loop(mgr_rx).await;
            });

            // Scheduler background task.
            let mgr2 = this.clone();
            tokio::spawn(async move {
                mgr2.scheduler_loop().await;
            });

            // Clipboard monitor (enabled per settings).
            let mgr3 = this.clone();
            tokio::spawn(async move {
                mgr3.clipboard_loop().await;
            });

            // Browser-integration IPC socket (enabled per settings).
            let mgr4 = this.clone();
            tokio::spawn(async move {
                mgr4.ipc_loop().await;
            });
        }

        Ok(this)
    }

    /// Clone a task handle out of the lock. Cloning the `Arc` (instead of
    /// borrowing) ensures the `std::sync::RwLockReadGuard` is dropped before
    /// any `.await`, keeping manager futures `Send` (required by Tauri).
    fn task_handle(&self, id: i64) -> Option<Arc<crate::task::DownloadTask>> {
        self.tasks.read().unwrap().get(&id).cloned()
    }

    /// Graceful shutdown (spec §58): pause all active downloads and give the
    /// tasks time to persist their state before the process exits.
    pub async fn shutdown(&self) {
        let ids: Vec<i64> = self.tasks.read().unwrap().keys().copied().collect();
        for id in ids {
            if let Some(t) = self.task_handle(id) {
                t.send(TaskCmd::Pause);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    /// Force the scheduler to evaluate windows now (used by tests and the
    /// "check now" button in the UI).
    pub async fn scheduler_tick(&self) -> Result<()> {
        self.run_scheduler_tick().await
    }

    // ------------------------------------------------------------------
    // Subscription
    // ------------------------------------------------------------------

    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.events.subscribe()
    }

    // ------------------------------------------------------------------
    // Add / start / pause / resume / cancel / retry / remove
    // ------------------------------------------------------------------

    /// Add a download. `policy_override` bypasses the configured duplicate policy.
    pub async fn add_download(
        &self,
        opts: AddDownloadOptions,
        policy_override: Option<DuplicatePolicy>,
    ) -> Result<AddOutcome> {
        let url = urlutil::validate_url(&opts.url)?;

        // Filename: explicit user value wins; otherwise probe for the server's
        // Content-Disposition name; otherwise the URL's filename (using the
        // final URL after redirects, so `/redirect/3` → `/file/x.bin` names
        // the file `x.bin`, not `3`).
        let mut filename = match &opts.filename {
            Some(f) if !f.trim().is_empty() => pathutil::sanitize_filename(f.trim()),
            _ => {
                let probe = self.probe_preview(&opts.url).await.ok();
                let probe_name = if self.settings.read().await.prefer_server_filename {
                    probe.as_ref().and_then(|p| p.filename.clone())
                } else {
                    None
                };
                let url_name = probe
                    .as_ref()
                    .and_then(|p| p.final_url.as_deref())
                    .and_then(|u| crate::Url::parse(u).ok())
                    .and_then(|u| urlutil::filename_from_url(&u))
                    .or_else(|| urlutil::filename_from_url(&url));
                probe_name
                    .or(url_name)
                    .unwrap_or_else(|| "download".to_string())
            }
        };

        let settings = self.settings.read().await.clone();
        let dir = match &opts.dir {
            Some(d) if !d.trim().is_empty() => pathutil::validate_destination_dir(d.trim())?,
            _ => pathutil::validate_destination_dir(&settings.default_dir)?,
        };
        let category = opts
            .category
            .clone()
            .unwrap_or_else(|| categories::category_for_extension(&categories::extension_of(&filename)));
        let connections = opts.connections.unwrap_or(settings.default_connections).clamp(1, 32);
        let policy = policy_override.unwrap_or(settings.duplicate_policy);

        // Duplicate handling at the destination.
        let check = pathutil::check_exists(&dir, &filename)?;
        if check.exists {
            match policy {
                DuplicatePolicy::Rename => {
                    filename = pathutil::unique_path(&dir, &filename)?
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                }
                DuplicatePolicy::Overwrite => {}
                DuplicatePolicy::Skip => {
                    return Ok(AddOutcome::Skipped {
                        reason: "A file with this name already exists.".to_string(),
                    });
                }
                DuplicatePolicy::Ask => {
                    return Ok(AddOutcome::NeedsDuplicateDecision {
                        url: opts.url.clone(),
                        filename: filename.clone(),
                        dir: dir.to_string_lossy().to_string(),
                        existing_path: check.raw_path.to_string_lossy().to_string(),
                    });
                }
            }
        }

        // Disk-space preflight when we know the size.
        if let Some(total) = self.probe_preview(&opts.url).await.ok().and_then(|p| p.total_bytes) {
            crate::fsutil::ensure_space(&dir, total)?;
        }

        let now = crate::now_unix();
        let scheduled = opts
            .scheduled_start
            .filter(|t| *t > now);
        let status = if scheduled.is_some() {
            DownloadStatus::Scheduled
        } else {
            DownloadStatus::Queued
        };
        let queue_name = opts
            .queue
            .clone()
            .unwrap_or_else(|| self.default_queue_name());
        if !self.queues.read().unwrap().contains_key(&queue_name) {
            return Err(EngineError::validation(format!(
                "Queue \"{queue_name}\" does not exist."
            )));
        }

        let record = DownloadRecord {
            id: 0,
            url: opts.url.clone(),
            final_url: None,
            filename,
            dir_path: dir.to_string_lossy().to_string(),
            temp_path: None,
            category,
            status,
            total_bytes: None,
            downloaded_bytes: 0,
            current_speed: 0,
            avg_speed: 0,
            peak_speed: 0,
            eta_seconds: None,
            connections,
            priority: Priority::Normal,
            speed_limit: opts.speed_limit,
            username: opts.username.clone(),
            password_ref: None,
            referrer: opts.referrer.clone(),
            protocol: url.scheme().to_string(),
            server: None,
            content_type: None,
            etag: None,
            last_modified: None,
            retry_count: 0,
            max_retries: settings.retry_count,
            error: None,
            verify_hash: opts.verify_hash.clone(),
            verify_type: opts.verify_type.clone(),
            verification_status: None,
            queue_name: Some(queue_name.clone()),
            scheduled_start: scheduled,
            created_at: now,
            started_at: None,
            completed_at: None,
            updated_at: now,
            can_resume: true,
        };

        let headers_json = serde_json::to_string(&opts.headers).ok();
        let cookies_json = serde_json::to_string(&opts.cookies).ok();
        let id = self.db.insert_download(&record, headers_json.as_deref(), cookies_json.as_deref()).await?;
        let mut record = record;
        record.id = id;

        // Credentials stay in memory only.
        if let (Some(u), Some(p)) = (&opts.username, &opts.password) {
            self.credentials
                .write()
                .unwrap()
                .insert(id, (u.clone(), p.clone()));
        }

        // Queue membership.
        self.enqueue(id, &queue_name).await?;

        let _ = self
            .events
            .send(EngineEvent::DownloadCreated {
                download: record.clone(),
            });

        if opts.start_immediately {
            self.start(id).await?;
        } else {
            self.maybe_start_next().await;
        }
        Ok(AddOutcome::Added { download: record })
    }

    /// Start (or resume) a download now. Explicit user action: bypasses queue
    /// limits, but a running download is left untouched.
    pub async fn start(&self, id: i64) -> Result<()> {
        let Some(record) = self.db.get_download(id).await? else {
            return Err(EngineError::validation("Download not found."));
        };
        if record.status.is_active() {
            return Ok(());
        }
        if let Some(task) = self.task_handle(id) {
            // Existing (paused/retrying) task → resume it.
            task.send(TaskCmd::Resume);
            return Ok(());
        }
        self.spawn_task_for(record).await?;
        Ok(())
    }

    pub async fn pause(&self, id: i64) -> Result<()> {
        let Some(record) = self.db.get_download(id).await? else {
            return Err(EngineError::validation("Download not found."));
        };
        if let Some(task) = self.task_handle(id) {
            task.send(TaskCmd::Pause);
            return Ok(());
        }
        // Not running: flip status directly.
        if record.status == DownloadStatus::Queued || record.status == DownloadStatus::Scheduled {
            self.db
                .set_download_fields(
                    id,
                    &[
                        ("status", rusqlite::types::Value::Text("PAUSED".into())),
                        ("scheduled_start", rusqlite::types::Value::Null),
                    ],
                )
                .await?;
            let _ = self.events.send(EngineEvent::DownloadPaused { download_id: id });
        }
        Ok(())
    }

    pub async fn resume(&self, id: i64) -> Result<()> {
        self.start(id).await
    }

    /// Cancel; `delete_partial` also removes the partial file.
    pub async fn cancel(&self, id: i64, delete_partial: bool) -> Result<()> {
        let Some(_) = self.db.get_download(id).await? else {
            return Err(EngineError::validation("Download not found."));
        };
        if let Some(task) = self.task_handle(id) {
            task.send(TaskCmd::Cancel { delete_partial });
            return Ok(());
        }
        // Not running: update directly.
        let mut record = self.db.get_download(id).await?.unwrap();
        record.status = DownloadStatus::Cancelled;
        if delete_partial {
            if let Some(tp) = &record.temp_path {
                let _ = std::fs::remove_file(tp);
                record.temp_path = None;
            }
            record.can_resume = false;
        }
        record.updated_at = crate::now_unix();
        self.db.update_download(&record).await?;
        let _ = self.events.send(EngineEvent::DownloadCancelled { download_id: id });
        Ok(())
    }

    /// Restart a download from scratch.
    pub async fn retry(&self, id: i64) -> Result<()> {
        let Some(record) = self.db.get_download(id).await? else {
            return Err(EngineError::validation("Download not found."));
        };
        if let Some(task) = self.task_handle(id) {
            task.send(TaskCmd::RetryFromScratch);
            return Ok(());
        }
        // Fresh task; clear partial state.
        if let Some(tp) = &record.temp_path {
            let _ = std::fs::remove_file(tp);
        }
        self.db
            .set_download_fields(
                id,
                &[
                    ("temp_path", rusqlite::types::Value::Null),
                    ("downloaded_bytes", rusqlite::types::Value::Integer(0)),
                    ("error_code", rusqlite::types::Value::Null),
                    ("error_message", rusqlite::types::Value::Null),
                    ("error_detail", rusqlite::types::Value::Null),
                ],
            )
            .await?;
        self.spawn_task_for(self.db.get_download(id).await?.unwrap())
            .await?;
        Ok(())
    }

    /// Remove a download from the list. `delete_file` also deletes the
    /// finished (or partial) file after the caller confirms.
    pub async fn remove(&self, id: i64, delete_file: bool) -> Result<()> {
        let record = self.db.get_download(id).await?;
        if let Some(task) = self.task_handle(id) {
            task.send(TaskCmd::Cancel { delete_partial: false });
        }
        if let Some(r) = &record {
            if delete_file {
                let candidate = if r.status == DownloadStatus::Completed {
                    pathutil::safe_join(Path::new(&r.dir_path), &r.filename).ok()
                } else {
                    r.temp_path.as_ref().map(PathBuf::from)
                };
                if let Some(p) = candidate {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        self.db.delete_download(id).await?;
        self.tasks.write().unwrap().remove(&id);
        self.credentials.write().unwrap().remove(&id);
        let _ = self.events.send(EngineEvent::DownloadRemoved { download_id: id });
        Ok(())
    }

    pub async fn set_priority(&self, id: i64, priority: Priority) -> Result<()> {
        self.db
            .set_download_fields(
                id,
                &[("priority", rusqlite::types::Value::Integer(priority as i64))],
            )
            .await?;
        let _ = self.events.send(EngineEvent::DownloadUpdated {
            download: self.db.get_download(id).await?.unwrap(),
        });
        Ok(())
    }

    pub async fn set_speed_limit(&self, id: i64, limit: Option<i64>) -> Result<()> {
        if let Some(task) = self.task_handle(id) {
            task.send(TaskCmd::SetSpeedLimit(limit));
        } else {
            let v = limit
                .map(rusqlite::types::Value::Integer)
                .unwrap_or(rusqlite::types::Value::Null);
            self.db.set_download_fields(id, &[("speed_limit", v)]).await?;
        }
        Ok(())
    }

    pub async fn set_global_speed_limit(&self, limit: Option<i64>) -> Result<()> {
        {
            let mut s = self.settings.write().await;
            s.global_speed_limit = limit;
            self.db.put_settings_json(&s.to_json()).await?;
        }
        self.global_rate.set_rate(limit.unwrap_or(0) as u64);
        Ok(())
    }

    /// Supply credentials for a download that needs authentication.
    pub async fn set_credentials(&self, id: i64, username: &str, password: &str) -> Result<()> {
        self.credentials
            .write()
            .unwrap()
            .insert(id, (username.to_string(), password.to_string()));
        self.db
            .set_download_fields(id, &[("username", rusqlite::types::Value::Text(username.into()))])
            .await?;
        // Retry the download with the new credentials.
        self.start(id).await
    }

    pub async fn set_verify_hash(
        &self,
        id: i64,
        hash_type: &str,
        expected: &str,
    ) -> Result<()> {
        self.db
            .set_download_fields(
                id,
                &[
                    ("verify_type", rusqlite::types::Value::Text(hash_type.into())),
                    ("verify_hash", rusqlite::types::Value::Text(expected.into())),
                    ("verification_status", rusqlite::types::Value::Null),
                ],
            )
            .await?;
        Ok(())
    }

    /// Verify a finished file against its stored checksum (if any).
    pub async fn verify_download(&self, id: i64) -> Result<String> {
        let record = self
            .db
            .get_download(id)
            .await?
            .ok_or_else(|| EngineError::validation("Download not found."))?;
        let path = pathutil::safe_join(Path::new(&record.dir_path), &record.filename)?;
        if !path.exists() {
            return Err(EngineError::validation("The file no longer exists."));
        }
        match (&record.verify_type, &record.verify_hash) {
            (Some(t), Some(h)) => {
                let kind = verify::HashType::parse(t)
                    .ok_or_else(|| EngineError::validation("Unknown hash type."))?;
                let p = path.clone();
                let exp = h.clone();
                let ok = tokio::task::spawn_blocking(move || verify::verify_checksum(&p, kind, &exp))
                    .await
                    .unwrap_or(Ok(false))?;
                let status = if ok { "passed" } else { "failed" };
                self.db
                    .set_download_fields(
                        id,
                        &[(
                            "verification_status",
                            rusqlite::types::Value::Text(status.into()),
                        )],
                    )
                    .await?;
                Ok(status.to_string())
            }
            _ => Err(EngineError::validation(
                "No checksum configured for this download.",
            )),
        }
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub async fn list_downloads(
        &self,
        filter: DownloadFilter,
        search: &str,
        sort: (SortField, SortOrder),
        category: Option<&str>,
    ) -> Result<Vec<DownloadRecord>> {
        self.db.list_downloads(filter, search, sort, category).await
    }

    pub async fn get_download(&self, id: i64) -> Result<Option<DownloadRecord>> {
        self.db.get_download(id).await
    }

    pub async fn stats(&self) -> Result<crate::stats::Stats> {
        self.db.stats().await
    }

    /// Best-effort server probe for the Add-download dialog (filename
    /// preview, size, range support).
    pub async fn probe_preview(&self, url: &str) -> Result<ProbeInfo> {
        let u = urlutil::validate_url(url)?;
        self.http.probe(&u, &RequestOptions::default()).await
    }

    // ------------------------------------------------------------------
    // Settings
    // ------------------------------------------------------------------

    pub async fn get_settings(&self) -> Settings {
        self.settings.read().await.clone()
    }

    pub async fn update_settings(&self, s: Settings) -> Result<()> {
        let mut s = s;
        s.validate();
        let old = self.settings.read().await.clone();
        let rate_changed = s.global_speed_limit != old.global_speed_limit;
        {
            let mut cur = self.settings.write().await;
            *cur = s.clone();
            self.db.put_settings_json(&s.to_json()).await?;
        }
        if rate_changed {
            self.global_rate.set_rate(s.global_speed_limit.unwrap_or(0) as u64);
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Queues
    // ------------------------------------------------------------------

    pub async fn list_queues(&self) -> Result<Vec<QueueDef>> {
        Ok(self.queues.read().unwrap().values().cloned().collect())
    }

    pub async fn create_queue(&self, name: &str, max_active: i32) -> Result<QueueDef> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(EngineError::validation("Queue name cannot be empty."));
        }
        let created = crate::now_unix();
        let name_for_sql = name.clone();
        let q = QueueDef {
            id: self
                .db
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO queues (name, max_active, is_default, created_at) VALUES (?1, ?2, 0, ?3)",
                        rusqlite::params![name_for_sql, max_active.clamp(1, 32), created],
                    )
                    .map_err(EngineError::from)?;
                    Ok(conn.last_insert_rowid())
                })
                .await?,
            name,
            max_active: max_active.clamp(1, 32),
            is_default: false,
            auto_start: true,
        };
        self.queues.write().unwrap().insert(q.name.clone(), q.clone());
        Ok(q)
    }

    pub async fn set_queue_max_active(&self, name: &str, max_active: i32) -> Result<()> {
        let max_active = max_active.clamp(1, 32);
        let name = name.to_string();
        let name_sql = name.clone();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE queues SET max_active=?1 WHERE name=?2",
                    rusqlite::params![max_active, name_sql],
                )
                .map_err(EngineError::from)?;
                Ok(())
            })
            .await?;
        if let Some(q) = self.queues.write().unwrap().get_mut(&name) {
            q.max_active = max_active;
        }
        Ok(())
    }

    pub async fn start_queue(&self, name: &str) -> Result<()> {
        let name = name.to_string();
        let exists = self.queues.read().unwrap().contains_key(&name);
        if exists {
            // Re-enable auto-start (e.g. after a schedule window ended).
            if let Some(q) = self.queues.write().unwrap().get_mut(&name) {
                q.auto_start = true;
            }
        }
        self.maybe_start_next().await;
        Ok(())
    }

    pub async fn pause_queue(&self, name: &str) -> Result<()> {
        let name = name.to_string();
        if self.queues.read().unwrap().contains_key(&name) {
            self.queues.write().unwrap().get_mut(&name).unwrap().auto_start = false;
        }
        let records = self.db.list_downloads(DownloadFilter::Active, "", (SortField::Name, SortOrder::Asc), None).await?;
        for r in records {
            if r.queue_name.as_deref() == Some(name.as_str()) {
                self.pause(r.id).await?;
            }
        }
        Ok(())
    }

    /// Reorder a download inside its queue.
    pub async fn move_download(&self, id: i64, delta: i32) -> Result<()> {
        let record = self
            .db
            .get_download(id)
            .await?
            .ok_or_else(|| EngineError::validation("Download not found."))?;
        let queue = record.queue_name.clone().unwrap_or_else(|| self.default_queue_name());
        self.db
            .call(move |conn| {
                let queue_id: i64 = conn
                    .query_row(
                        "SELECT id FROM queues WHERE name=?1",
                        [&queue],
                        |r| r.get(0),
                    )
                    .map_err(EngineError::from)?;
                let pos: i64 = conn
                    .query_row(
                        "SELECT position FROM queue_items WHERE download_id=?1 AND queue_id=?2",
                        rusqlite::params![id, queue_id],
                        |r| r.get(0),
                    )
                    .map_err(EngineError::from)?;
                let target = pos + delta as i64;
                if delta < 0 {
                    conn.execute(
                        "UPDATE queue_items SET position = position + 1 WHERE queue_id=?1 AND position >= ?2 AND position < ?3",
                        rusqlite::params![queue_id, target, pos],
                    )
                    .map_err(EngineError::from)?;
                } else {
                    conn.execute(
                        "UPDATE queue_items SET position = position - 1 WHERE queue_id=?1 AND position > ?2 AND position <= ?3",
                        rusqlite::params![queue_id, pos, target],
                    )
                    .map_err(EngineError::from)?;
                }
                conn.execute(
                    "UPDATE queue_items SET position=?1 WHERE download_id=?2 AND queue_id=?3",
                    rusqlite::params![target, id, queue_id],
                )
                .map_err(EngineError::from)?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Schedules
    // ------------------------------------------------------------------

    pub async fn list_schedules(&self) -> Result<Vec<ScheduleDef>> {
        self.db
            .call(|conn| {
                let mut stmt = conn
                    .prepare("SELECT * FROM schedules ORDER BY id")
                    .map_err(EngineError::from)?;
                let mut rows = stmt
                    .query_map([], |r| {
                        Ok(ScheduleDef {
                            id: r.get("id")?,
                            name: r.get("name")?,
                            enabled: r.get::<_, i64>("enabled")? != 0,
                            start_time: r.get("start_time")?,
                            stop_time: r.get("stop_time").ok().flatten(),
                            days: r.get("days")?,
                            speed_limit: r.get("speed_limit").ok().flatten(),
                            max_active: r.get("max_active").ok().flatten(),
                            queue_id: r.get("queue_id").ok().flatten(),
                            action: r.get("action")?,
                        })
                    })
                    .map_err(EngineError::from)?;
                let mut out = Vec::new();
                while let Some(s) = rows.next().transpose().map_err(EngineError::from)? {
                    out.push(s);
                }
                Ok(out)
            })
            .await
    }

    pub async fn upsert_schedule(&self, s: &ScheduleDef) -> Result<i64> {
        let s = s.clone();
        let id = self
            .db
            .call(move |conn| {
                if s.id == 0 {
                    conn.execute(
                        "INSERT INTO schedules (name, enabled, start_time, stop_time, days, speed_limit, max_active, queue_id, action, created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                        rusqlite::params![
                            s.name, s.enabled as i64, s.start_time, s.stop_time, s.days,
                            s.speed_limit, s.max_active, s.queue_id, s.action, crate::now_unix()
                        ],
                    )
                    .map_err(EngineError::from)?;
                    Ok(conn.last_insert_rowid())
                } else {
                    conn.execute(
                        "UPDATE schedules SET name=?1, enabled=?2, start_time=?3, stop_time=?4, days=?5, speed_limit=?6, max_active=?7, queue_id=?8, action=?9 WHERE id=?10",
                        rusqlite::params![
                            s.name, s.enabled as i64, s.start_time, s.stop_time, s.days,
                            s.speed_limit, s.max_active, s.queue_id, s.action, s.id
                        ],
                    )
                    .map_err(EngineError::from)?;
                    Ok(s.id)
                }
            })
            .await?;
        Ok(id)
    }

    pub async fn delete_schedule(&self, id: i64) -> Result<()> {
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM schedules WHERE id=?1", [id])
                    .map_err(EngineError::from)?;
                Ok(())
            })
            .await
    }

    // ------------------------------------------------------------------
    // History / import / export
    // ------------------------------------------------------------------

    pub async fn clear_history(&self, scope: &str) -> Result<()> {
        let clause = match scope {
            "all" => "WHERE status IN ('COMPLETED','FAILED','CANCELLED')".to_string(),
            "completed" => "WHERE status='COMPLETED'".to_string(),
            "failed" => "WHERE status IN ('FAILED','CANCELLED')".to_string(),
            _ => return Err(EngineError::validation("Unknown history scope.")),
        };
        self.db
            .call(move |conn| {
                conn.execute(&format!("DELETE FROM downloads {clause}"), [])
                    .map_err(EngineError::from)?;
                Ok(())
            })
            .await
    }

    pub async fn export_downloads(&self, format: &str) -> Result<String> {
        let records = self.db.list_downloads(DownloadFilter::All, "", (SortField::DateAdded, SortOrder::Desc), None).await?;
        let redact = self.settings.read().await.redact_urls_in_history;
        let redact_url = |url: &str| -> String {
            if redact {
                crate::Url::parse(url)
                    .map(|u| urlutil::redact_url(&u))
                    .unwrap_or_else(|_| url.to_string())
            } else {
                url.to_string()
            }
        };
        match format {
            "json" => {
                let redacted: Vec<_> = records
                    .iter()
                    .map(|r| {
                        let mut r = r.clone();
                        r.url = redact_url(&r.url);
                        r
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "app": "ldm",
                    "version": crate::ENGINE_VERSION,
                    "downloads": redacted,
                }))?)
            }
            "csv" => {
                let mut out = String::from("id,filename,url,dir,category,status,size_bytes,created_at\n");
                for r in &records {
                    out.push_str(&format!(
                        "{},{},{},{},{},{},{},{}\n",
                        r.id,
                        csv_escape(&r.filename),
                        csv_escape(&redact_url(&r.url)),
                        csv_escape(&r.dir_path),
                        r.category,
                        r.status.as_str(),
                        r.total_bytes.unwrap_or(0),
                        r.created_at,
                    ));
                }
                Ok(out)
            }
            _ => Err(EngineError::validation("Unknown export format.")),
        }
    }

    pub async fn import_downloads(&self, json: &str) -> Result<usize> {
        let v: serde_json::Value =
            serde_json::from_str(json).map_err(|e| EngineError::validation(format!("Invalid import file: {e}")))?;
        let downloads = v
            .get("downloads")
            .and_then(|d| d.as_array())
            .ok_or_else(|| EngineError::validation("Import file has no \"downloads\" array."))?;
        let mut added = 0usize;
        for item in downloads {
            let url = item
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or_else(|| EngineError::validation("A download record is missing its URL."))?;
            let filename = item.get("filename").and_then(|f| f.as_str()).map(|s| s.to_string());
            let dir = item.get("dir_path").and_then(|d| d.as_str()).map(|s| s.to_string());
            let category = item.get("category").and_then(|c| c.as_str()).map(|s| s.to_string());
            // Never auto-start imported downloads (spec §44).
            let opts = AddDownloadOptions {
                url: url.to_string(),
                filename,
                dir,
                category,
                start_immediately: false,
                ..Default::default()
            };
            match self.add_download(opts, Some(DuplicatePolicy::Rename)).await {
                Ok(AddOutcome::Added { .. }) => added += 1,
                Ok(AddOutcome::Skipped { .. }) | Ok(AddOutcome::NeedsDuplicateDecision { .. }) => {}
                Err(_) => {}
            }
        }
        Ok(added)
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn default_queue_name(&self) -> String {
        self.queues
            .read()
            .unwrap()
            .values()
            .find(|q| q.is_default)
            .map(|q| q.name.clone())
            .unwrap_or_else(|| "Default".to_string())
    }

    async fn load_queues(&self) -> Result<()> {
        let queues: Vec<QueueDef> = self
            .db
            .call(|conn| {
                let mut stmt = conn
                    .prepare("SELECT * FROM queues ORDER BY is_default DESC, id")
                    .map_err(EngineError::from)?;
                let mut rows = stmt
                    .query_map([], |r| {
                        Ok(QueueDef {
                            id: r.get("id")?,
                            name: r.get("name")?,
                            max_active: r.get("max_active")?,
                            is_default: r.get::<_, i64>("is_default")? != 0,
                            auto_start: true,
                        })
                    })
                    .map_err(EngineError::from)?;
                let mut out = Vec::new();
                while let Some(q) = rows.next().transpose().map_err(EngineError::from)? {
                    out.push(q);
                }
                Ok(out)
            })
            .await?;
        let mut map = HashMap::new();
        for q in queues {
            map.insert(q.name.clone(), q);
        }
        if !map.contains_key("Default") {
            let created = crate::now_unix();
            let id = self
                .db
                .call(move |conn| {
                    conn.execute(
                        "INSERT INTO queues (name, max_active, is_default, created_at) VALUES ('Default', 3, 1, ?1)",
                        [created],
                    )
                    .map_err(EngineError::from)?;
                    Ok(conn.last_insert_rowid())
                })
                .await?;
            map.insert(
                "Default".to_string(),
                QueueDef {
                    id,
                    name: "Default".to_string(),
                    max_active: 3,
                    is_default: true,
                    auto_start: true,
                },
            );
        }
        *self.queues.write().unwrap() = map;
        Ok(())
    }

    async fn enqueue(&self, download_id: i64, queue_name: &str) -> Result<()> {
        let queue_name = queue_name.to_string();
        self.db
            .call(move |conn| {
                let queue_id: i64 = conn
                    .query_row("SELECT id FROM queues WHERE name=?1", [&queue_name], |r| r.get(0))
                    .map_err(EngineError::from)?;
                let pos: i64 = conn
                    .query_row(
                        "SELECT COALESCE(MAX(position),0)+1 FROM queue_items WHERE queue_id=?1",
                        [queue_id],
                        |r| r.get(0),
                    )
                    .map_err(EngineError::from)?;
                conn.execute(
                    "INSERT INTO queue_items (queue_id, download_id, position) VALUES (?1,?2,?3)",
                    rusqlite::params![queue_id, download_id, pos],
                )
                .map_err(EngineError::from)?;
                Ok(())
            })
            .await
    }

    /// Build and spawn a task for a record (fetching credentials).
    async fn spawn_task_for(&self, record: DownloadRecord) -> Result<()> {
        let creds = self.credentials.read().unwrap().get(&record.id).cloned();
        let (username, password) = creds
            .map(|(u, p)| (Some(u), Some(p)))
            .unwrap_or((record.username.clone(), None));
        // Headers/cookies are stored as JSON columns; re-fetch them.
        let (headers, cookies) = self.fetch_headers_cookies(record.id).await;
        let opts = RequestOptions {
            username,
            password,
            bearer: None,
            headers,
            cookies,
            referrer: record.referrer.clone(),
        };
        let verify_hash = match (&record.verify_type, &record.verify_hash) {
            (Some(t), Some(h)) => Some((t.clone(), h.clone())),
            _ => None,
        };
        let ctx = TaskContext {
            db: self.db.clone(),
            settings: self.settings.clone(),
            events: self.events.clone(),
            mgr: self.mgr_tx.clone(),
            http: self.http.clone(),
            global_rate: self.global_rate.clone(),
            opts,
            headers_json: None,
            cookies_json: None,
            verify_hash,
            record,
        };
        let task = spawn_task(ctx).await;
        self.tasks.write().unwrap().insert(task.record_id, Arc::new(task));
        Ok(())
    }

    async fn fetch_headers_cookies(&self, id: i64) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let out = self
            .db
            .call(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT headers, cookies FROM downloads WHERE id=?1")
                    .map_err(EngineError::from)?;
                let row: Option<(Option<String>, Option<String>)> = stmt
                    .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .ok();
                Ok(row)
            })
            .await
            .ok()
            .flatten();
        let (h, c) = out.unwrap_or((None, None));
        let headers = h
            .and_then(|j| serde_json::from_str::<Vec<(String, String)>>(&j).ok())
            .unwrap_or_default();
        let cookies = c
            .and_then(|j| serde_json::from_str::<Vec<(String, String)>>(&j).ok())
            .unwrap_or_default();
        (headers, cookies)
    }

    /// Start queued downloads within the configured limits.
    async fn maybe_start_next(&self) {
        let settings = self.settings.read().await.clone();
        let active_now = self
            .db
            .list_downloads(DownloadFilter::Active, "", (SortField::DateAdded, SortOrder::Asc), None)
            .await
            .unwrap_or_default()
            .len();
        let global_slots = (settings.max_active_downloads as i64 - active_now as i64).max(0);
        if global_slots == 0 {
            return;
        }
        // Candidates: QUEUED downloads, high priority first, then queue position.
        let candidates = self
            .db
            .list_downloads(DownloadFilter::Queued, "", (SortField::DateAdded, SortOrder::Asc), None)
            .await
            .unwrap_or_default();
        let mut slots = global_slots;
        for rec in candidates {
            if slots <= 0 {
                break;
            }
            let qname = rec.queue_name.clone().unwrap_or_else(|| self.default_queue_name());
            let queue = match self.queues.read().unwrap().get(&qname) {
                Some(q) => q.clone(),
                None => continue,
            };
            if !queue.auto_start {
                continue;
            }
            // Per-queue limit.
            let queue_active = self
                .db
                .list_downloads(DownloadFilter::Active, "", (SortField::DateAdded, SortOrder::Asc), None)
                .await
                .unwrap_or_default()
                .iter()
                .filter(|r| r.queue_name.as_deref() == Some(qname.as_str()))
                .count();
            if queue_active >= queue.max_active as usize {
                continue;
            }
            if self.start(rec.id).await.is_ok() {
                slots -= 1;
            }
        }
    }

    async fn manager_loop(&self, mut rx: mpsc::UnboundedReceiver<ManagerMsg>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                ManagerMsg::DownloadFinished(id) | ManagerMsg::DownloadFailed(id) | ManagerMsg::DownloadCancelled(id) => {
                    self.tasks.write().unwrap().remove(&id);
                    self.maybe_start_next().await;
                    // IDM-style "after completion" action: when the whole
                    // queue is idle and an action is configured, tell the UI.
                    let idle = self.tasks.read().unwrap().is_empty()
                        && self
                            .db
                            .list_downloads(
                                DownloadFilter::Active,
                                "",
                                (SortField::DateAdded, SortOrder::Asc),
                                None,
                            )
                            .await
                            .map(|rs| rs.is_empty())
                            .unwrap_or(false);
                    if idle
                        && self.settings.read().await.after_completion
                            != crate::settings::AfterCompletion::None
                    {
                        let _ = self.events.send(EngineEvent::DownloadsIdle);
                    }
                }
                ManagerMsg::NetworkOffline => {
                    self.offline.store(true, Ordering::SeqCst);
                }
                ManagerMsg::NetworkOnline => {
                    self.offline.store(false, Ordering::SeqCst);
                }
                ManagerMsg::IpcRequest(req) => {
                    let _ = self.handle_ipc(req).await;
                }
            }
        }
    }

    /// Reconcile interrupted downloads after a crash/restart (spec §11, §101).
    async fn reconcile_startup(&self) -> Result<()> {
        let records = self
            .db
            .list_downloads(DownloadFilter::All, "", (SortField::DateAdded, SortOrder::Asc), None)
            .await?;
        let resume = self.settings.read().await.resume_on_start;
        for r in records {
            match r.status {
                DownloadStatus::Starting
                | DownloadStatus::Connecting
                | DownloadStatus::Downloading
                | DownloadStatus::Verifying
                | DownloadStatus::Retrying => {
                    if resume && r.can_resume {
                        self.db
                            .set_download_fields(
                                r.id,
                                &[("status", rusqlite::types::Value::Text("QUEUED".into()))],
                            )
                            .await?;
                    } else {
                        self.db
                            .set_download_fields(
                                r.id,
                                &[("status", rusqlite::types::Value::Text("PAUSED".into()))],
                            )
                            .await?;
                    }
                }
                DownloadStatus::Completed => {
                    // Crash between rename and DB update: detect and fix.
                    let final_path = pathutil::safe_join(Path::new(&r.dir_path), &r.filename).ok();
                    if let Some(p) = final_path {
                        if p.exists() {
                            if let Some(total) = r.total_bytes {
                                if let Ok(md) = std::fs::metadata(&p) {
                                    if md.len() as i64 == total && r.completed_at.is_none() {
                                        self.db
                                            .set_download_fields(
                                                r.id,
                                                &[(
                                                    "completed_at",
                                                    rusqlite::types::Value::Integer(crate::now_unix()),
                                                )],
                                            )
                                            .await?;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Restart queued downloads if configured.
        if resume {
            self.maybe_start_next().await;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Background loops
    // ------------------------------------------------------------------

    async fn scheduler_loop(&self) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = self.run_scheduler_tick().await {
                tracing::debug!("scheduler tick error: {e}");
            }
        }
    }

    async fn run_scheduler_tick(&self) -> Result<()> {
        use crate::scheduler::is_window_active;
        let schedules = self.list_schedules().await?;
        for s in schedules {
            if !s.enabled {
                continue;
            }
            let active = is_window_active(&s.start_time, s.stop_time.as_deref(), s.days);
            let queue_name = if let Some(qid) = s.queue_id {
                self.queues
                    .read()
                    .unwrap()
                    .values()
                    .find(|q| q.id == qid)
                    .map(|q| q.name.clone())
            } else {
                None
            };
            if active {
                if let Some(limit) = s.speed_limit {
                    self.global_rate.set_rate(limit as u64);
                }
                if let Some(qname) = &queue_name {
                    self.queues.write().unwrap().get_mut(qname).map(|q| q.auto_start = true);
                }
                if let Some(ma) = s.max_active {
                    if let Some(qname) = &queue_name {
                        self.set_queue_max_active(qname, ma).await?;
                    }
                }
            } else {
                // Window ended: restore configured global limit, gate the queue.
                let g = self.settings.read().await.global_speed_limit.unwrap_or(0);
                self.global_rate.set_rate(g.max(0) as u64);
                if let Some(qname) = &queue_name {
                    self.queues.write().unwrap().get_mut(qname).map(|q| q.auto_start = false);
                }
                if s.stop_time.is_some() {
                    // Pause active downloads in the queue when the window ends.
                    let active_recs = self
                        .db
                        .list_downloads(DownloadFilter::Active, "", (SortField::DateAdded, SortOrder::Asc), None)
                        .await?;
                    for r in active_recs {
                        if r.queue_name.as_deref() == queue_name.as_deref() {
                            let _ = self.pause(r.id).await;
                        }
                    }
                }
            }
        }
        // Promote SCHEDULED downloads whose time has come.
        let records = self
            .db
            .list_downloads(DownloadFilter::Scheduled, "", (SortField::DateAdded, SortOrder::Asc), None)
            .await?;
        let now = crate::now_unix();
        for r in records {
            if r.scheduled_start.map(|t| t <= now).unwrap_or(false) {
                self.db
                    .set_download_fields(
                        r.id,
                        &[("status", rusqlite::types::Value::Text("QUEUED".into()))],
                    )
                    .await?;
            }
        }
        self.maybe_start_next().await;
        Ok(())
    }

    async fn clipboard_loop(&self) {
        let mut enabled = self.settings.read().await.clipboard_monitoring;
        let mut monitor = if enabled {
            match crate::clipboard::ClipboardMonitor::new() {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!("clipboard monitoring unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let now_enabled = self.settings.read().await.clipboard_monitoring;
            if now_enabled != enabled {
                enabled = now_enabled;
                if enabled && monitor.is_none() {
                    monitor = crate::clipboard::ClipboardMonitor::new().ok();
                }
            }
            if let Some(m) = monitor.as_mut() {
                if enabled {
                    if let Some((url, filename)) = m.poll() {
                        let _ = self.events.send(EngineEvent::ClipboardUrlDetected {
                            url,
                            filename,
                        });
                    }
                }
            }
        }
    }

    async fn ipc_loop(&self) {
        let mut enabled = self.settings.read().await.browser_integration_enabled;
        let mut server: Option<ipc_server::IpcServer> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let now_enabled = self.settings.read().await.browser_integration_enabled;
            if now_enabled != enabled {
                enabled = now_enabled;
                if enabled {
                    let cfg = ipc_server::IpcConfig {
                        runtime_dir: dirs::runtime_dir()
                            .unwrap_or_else(|| PathBuf::from("."))
                            .join("ldm"),
                    };
                    match ipc_server::IpcServer::start(cfg) {
                        Ok(s) => {
                            tracing::info!("browser integration IPC listening on {}", s.socket_path.display());
                            server = Some(s);
                        }
                        Err(e) => tracing::warn!("browser integration IPC failed to start: {e}"),
                    }
                } else if let Some(s) = server.take() {
                    let _ = s.stop();
                }
            }
            if let Some(s) = server.as_mut() {
                // Drain incoming requests.
                while let Ok(req) = s.try_recv() {
                    let mgr = self.mgr_tx.clone();
                    let _ = mgr.send(ManagerMsg::IpcRequest(req));
                }
            }
        }
    }

    async fn handle_ipc(&self, req: ipc_server::IpcRequest) {
        use crate::model::DownloadFilter as _DF; // keep import used
        let _ = _DF::All;
        use ipc_server::IpcResponse;
        let reply = req.reply;
        let resp = match req.op {
            ipc_server::IpcOp::Ping => IpcResponse::ok(serde_json::json!({"pong": true})),
            ipc_server::IpcOp::AddDownload { url, filename, dir, referrer, cookies, category, connections } => {
                let opts = AddDownloadOptions {
                    url,
                    filename,
                    dir,
                    category,
                    connections,
                    referrer,
                    cookies,
                    start_immediately: true,
                    ..Default::default()
                };
                match self.add_download(opts, None).await {
                    Ok(AddOutcome::Added { download }) => {
                        IpcResponse::ok(serde_json::json!({"download_id": download.id}))
                    }
                    Ok(AddOutcome::NeedsDuplicateDecision { .. }) => {
                        IpcResponse::ok(serde_json::json!({"download_id": null, "needs_decision": true}))
                    }
                    Ok(AddOutcome::Skipped { reason }) => {
                        IpcResponse::ok(serde_json::json!({"download_id": null, "skipped": reason}))
                    }
                    Err(e) => IpcResponse::error(e.message),
                }
            }
        };
        let _ = reply.send(resp);
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
