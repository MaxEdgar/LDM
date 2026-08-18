//! The download task: one long-running async task per download.
//!
//! Responsibilities (spec §78, §79):
//! * Deterministic state machine (STARTING → CONNECTING → DOWNLOADING →
//!   PAUSED/RETRYING/VERIFYING → COMPLETED/FAILED/CANCELLED).
//! * Probe the server, verify range support with a real request, allocate
//!   segments, and drive segment workers.
//! * Stop-and-restart on errors: any worker failure halts the others at the
//!   next chunk boundary, then the task applies a single retry decision.
//! * Graceful pause/cancel, atomic completion (fsync → rename), optional
//!   size/hash verification, and debounced persistence.

use crate::db::Db;
use crate::error::{EngineError, Result};
use crate::events::EngineEvent;
use crate::http::HttpProtocol;
use crate::manager::ManagerMsg;
use crate::model::{DownloadRecord, DownloadStatus, ErrorInfo, SegmentRecord, SegmentStatus};
use crate::pathutil;
use crate::protocol::{DownloadProtocol, RequestOptions};
use crate::rate::TokenBucket;
use crate::retry::{Backoff, RetryDecision};
use crate::segment::{run_segment, SegOutcome, SegShared, SegStatus, TaskShared};
use crate::settings::Settings;
use crate::{fsutil, verify as hashing};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::Instant;

/// Commands a caller can send to a running download task.
#[derive(Debug, Clone)]
pub enum TaskCmd {
    Pause,
    Resume,
    /// Resume but restart the file from scratch (used when the remote changed).
    RetryFromScratch,
    Cancel { delete_partial: bool },
    SetSpeedLimit(Option<i64>),
    FlushNow,
}

/// Handle to a running (or paused) download task.
pub struct DownloadTask {
    pub shared: Arc<TaskShared>,
    pub cmd_tx: mpsc::UnboundedSender<TaskCmd>,
    pub done: Arc<AtomicBool>,
    pub record_id: i64,
}

impl DownloadTask {
    pub fn send(&self, cmd: TaskCmd) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }
}

/// Everything a task needs that is not per-download mutable state.
pub struct TaskContext {
    pub db: Arc<Db>,
    pub settings: Arc<RwLock<Settings>>,
    pub events: broadcast::Sender<EngineEvent>,
    pub mgr: mpsc::UnboundedSender<ManagerMsg>,
    pub http: Arc<HttpProtocol>,
    pub global_rate: Arc<TokenBucket>,
    pub opts: RequestOptions,
    pub headers_json: Option<String>,
    pub cookies_json: Option<String>,
    /// (hash_type, expected_hex)
    pub verify_hash: Option<(String, String)>,
    /// The download record at spawn time (fresh from the DB).
    pub record: DownloadRecord,
}

enum PendingAction {
    Retry { err: EngineError, delay: Duration },
    Fail { err: EngineError },
    PauseWithError { err: EngineError },
    WaitNetwork { err: EngineError },
}

const MIN_MULTI_SEGMENT_BYTES: i64 = 1024 * 1024;
const MAX_SEGMENTS: usize = 32;
const SEGMENT_TARGET_BYTES: i64 = 8 * 1024 * 1024;

/// Spawn a download task on the current runtime and return a handle.
pub async fn spawn_task(ctx: TaskContext) -> DownloadTask {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let done_flag = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(TaskShared {
        download_id: ctx.record.id,
        url: ctx
            .record
            .url
            .parse()
            .unwrap_or_else(|_| crate::Url::parse("https://invalid").unwrap()),
        opts: ctx.opts.clone(),
        protocol: ctx.http.clone(),
        pause: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        ranges_supported: std::sync::atomic::AtomicBool::new(true),
        downloaded: AtomicI64::new(0),
        total: AtomicI64::new(-1),
        file: tokio::sync::Mutex::new(tokio::fs::File::from_std(
            std::fs::File::open("/dev/null").expect("opening /dev/null"),
        )),
        per_download_rate: TokenBucket::new(ctx.record.speed_limit.unwrap_or(0) as u64),
        global_rate: ctx.global_rate.clone(),
        speed: std::sync::Mutex::new(crate::speed::SpeedMeter::new()),
    });
    let record_id = ctx.record.id;
    let mut loop_ = TaskLoop {
        ctx,
        shared: shared.clone(),
        cmd_rx,
        jobs: tokio::task::JoinSet::new(),
        segs: Vec::new(),
        pending: VecDeque::new(),
        active: HashSet::new(),
        record: TaskLoop::placeholder(),
        state: DownloadStatus::Queued,
        stopping: false,
        pending_action: None,
        retry_deadline: None,
        tick_count: 0,
        max_retries: 5,
        backoff: Backoff::default(),
        final_path: None,
        force_restart: false,
        connections: 8,
        offline: false,
        done_flag: done_flag.clone(),
        record_tmp_dir: None,
        cancel_delete_partial: None,
    };
    let record = loop_.ctx.record.clone();
    loop_.record = record;
    let settings = loop_.ctx.settings.read().await.clone();
    loop_.max_retries = if loop_.record.max_retries > 0 {
        loop_.record.max_retries
    } else {
        settings.retry_count
    };
    loop_.backoff = Backoff {
        base: Duration::from_secs(settings.retry_base_seconds.max(1)),
        ..Backoff::default()
    };
    loop_.connections = loop_.record.connections.max(1);

    tokio::spawn(async move {
        loop_.run().await;
    });
    DownloadTask {
        shared,
        cmd_tx,
        done: done_flag,
        record_id,
    }
}

pub struct TaskLoop {
    ctx: TaskContext,
    shared: Arc<TaskShared>,
    cmd_rx: mpsc::UnboundedReceiver<TaskCmd>,
    jobs: tokio::task::JoinSet<(i64, SegOutcome)>,
    segs: Vec<Arc<SegShared>>,
    pending: VecDeque<i64>,
    active: HashSet<i64>,
    record: DownloadRecord,
    state: DownloadStatus,
    stopping: bool,
    pending_action: Option<PendingAction>,
    retry_deadline: Option<Instant>,
    tick_count: u32,
    max_retries: i32,
    backoff: Backoff,
    final_path: Option<PathBuf>,
    force_restart: bool,
    connections: i32,
    offline: bool,
    done_flag: Arc<AtomicBool>,
    /// Configured temp dir (copied from settings at task start).
    record_tmp_dir: Option<String>,
    /// Pending cancel requests (with whether to delete the partial file).
    cancel_delete_partial: Option<bool>,
}

impl TaskLoop {
    fn placeholder() -> DownloadRecord {
        DownloadRecord {
            id: 0,
            url: String::new(),
            final_url: None,
            filename: String::new(),
            dir_path: String::new(),
            temp_path: None,
            category: "General".to_string(),
            status: DownloadStatus::Queued,
            total_bytes: None,
            downloaded_bytes: 0,
            current_speed: 0,
            avg_speed: 0,
            peak_speed: 0,
            eta_seconds: None,
            connections: 8,
            priority: crate::model::Priority::Normal,
            speed_limit: None,
            username: None,
            password_ref: None,
            referrer: None,
            protocol: "http".to_string(),
            server: None,
            content_type: None,
            etag: None,
            last_modified: None,
            retry_count: 0,
            max_retries: 0,
            error: None,
            verify_hash: None,
            verify_type: None,
            verification_status: None,
            queue_name: None,
            scheduled_start: None,
            created_at: 0,
            started_at: None,
            completed_at: None,
            updated_at: 0,
            can_resume: true,
        }
    }

    pub async fn run(&mut self) {
        self.state = self.record.status;
        match self.state {
            DownloadStatus::Queued | DownloadStatus::Scheduled => {
                if let Some(ts) = self.record.scheduled_start {
                    let now = crate::now_unix();
                    if ts > now {
                        if !self.await_until().await {
                            return;
                        }
                    }
                }
                self.begin_download().await;
            }
            _ => {
                self.begin_download().await;
            }
        }
        if self.done_flag.load(Ordering::SeqCst) {
            return;
        }

        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let _ = ticker.tick().await; // consume the immediate first tick

        loop {
            // Far-future deadline when no retry is scheduled: branch never fires.
            let deadline = self
                .retry_deadline
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        None => break,
                        Some(c) => self.on_cmd(c).await,
                    }
                }
                _ = ticker.tick() => {
                    self.on_tick().await;
                }
                res = self.jobs.join_next(), if !self.jobs.is_empty() => {
                    self.on_worker_done(res).await;
                }
                _ = &mut sleep => {
                    if self.retry_deadline.is_some() {
                        self.on_retry_fire().await;
                    }
                }
            }
            if self.done_flag.load(Ordering::SeqCst) {
                break;
            }
        }
        let _ = self.flush_progress().await;
        self.done_flag.store(true, Ordering::SeqCst);
    }

    /// Wait while idle (e.g. for a scheduled start), handling commands.
    async fn await_until(&mut self) -> bool {
        loop {
            let Some(ts) = self.record.scheduled_start else {
                return true;
            };
            let remain = ts - crate::now_unix();
            if remain <= 0 {
                return true;
            }
            let sleep = tokio::time::sleep(Duration::from_secs(remain as u64));
            tokio::pin!(sleep);
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        None => return false,
                        Some(TaskCmd::Cancel { delete_partial }) => {
                            self.finish_cancel(delete_partial).await;
                            return false;
                        }
                        Some(TaskCmd::Resume) | Some(TaskCmd::RetryFromScratch) => {
                            self.record.scheduled_start = None;
                            let _ = self.ctx.db
                                .set_download_fields(self.record.id, &[("scheduled_start", rusqlite::types::Value::Null)])
                                .await;
                            return true;
                        }
                        Some(_) => {}
                    }
                }
                _ = &mut sleep => {}
            }
        }
    }

    async fn begin_download(&mut self) {
        if self.state.is_terminal() || self.done_flag.load(Ordering::SeqCst) {
            return;
        }
        self.shared.pause.store(false, Ordering::SeqCst);
        if self.shared.cancel.load(Ordering::SeqCst) {
            return;
        }
        self.state = DownloadStatus::Starting;
        self.set_db_status(DownloadStatus::Starting).await;
        let _ = self.ctx.events.send(EngineEvent::DownloadStarted {
            download_id: self.record.id,
        });

        self.state = DownloadStatus::Connecting;
        self.set_db_status(DownloadStatus::Connecting).await;
        let _ = self.ctx.events.send(EngineEvent::DownloadConnecting {
            download_id: self.record.id,
        });

        if let Err(e) = self.probe_and_prepare().await {
            if self.shared.cancel.load(Ordering::SeqCst) {
                self.finish_cancel(false).await;
            } else {
                self.apply_retry_decision(e).await;
                self.finish_stop_if_idle().await;
            }
            return;
        }

        self.state = DownloadStatus::Downloading;
        self.set_db_status(DownloadStatus::Downloading).await;
        let _ = self.ctx.events.send(EngineEvent::DownloadResumed {
            download_id: self.record.id,
        });
        self.spawn_workers();
        if self.all_completed() {
            self.finalize().await;
        }
    }

    /// Probe the server, validate resume-ability, build/reconcile segments,
    /// and open the temp file.
    async fn probe_and_prepare(&mut self) -> Result<()> {
        self.record_tmp_dir = self.ctx.settings.read().await.temp_dir.clone();
        let probe = self
            .ctx
            .http
            .probe(&self.shared.url, &self.shared.opts)
            .await?;

        self.record.final_url = probe.final_url.clone();
        self.record.etag = probe.etag.clone();
        self.record.last_modified = probe.last_modified.clone();
        self.record.server = probe.server.clone();
        self.record.content_type = probe.content_type.clone();
        self.record.total_bytes = probe.total_bytes;
        self.shared
            .ranges_supported
            .store(probe.ranges_supported, Ordering::SeqCst);

        // Remote-changed detection on resume (spec §143/§144).
        if self.record.downloaded_bytes > 0 && !self.force_restart {
            let etag_changed = match (&self.record.etag, &probe.etag) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            let lm_changed = match (&self.record.last_modified, &probe.last_modified) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            if etag_changed || lm_changed {
                return Err(EngineError::remote_changed().with_detail(
                    "The remote file appears to have changed since this partial download began. Restart the download to continue safely.",
                ));
            }
        }

        // Force-restart clears all partial data.
        if self.force_restart {
            self.record.downloaded_bytes = 0;
            if let Some(tp) = &self.record.temp_path {
                let _ = std::fs::remove_file(tp);
            }
            self.record.temp_path = None;
            self.force_restart = false;
        }

        // Build segments: reuse persisted ones when resuming, else allocate.
        let has_partial = self.record.downloaded_bytes > 0 || self.record.temp_path.is_some();
        let db_segs = if has_partial {
            self.ctx
                .db
                .get_segments(self.record.id)
                .await
                .ok()
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let segs: Vec<SegmentRecord> = if let Some(existing) = db_segs {
            existing
        } else {
            let ranges = build_ranges(probe.total_bytes, self.connections, probe.ranges_supported);
            ranges
                .into_iter()
                .map(|(s, e)| SegmentRecord {
                    id: 0,
                    download_id: self.record.id,
                    start_byte: s,
                    end_byte: e,
                    downloaded_bytes: 0,
                    status: SegmentStatus::Pending,
                    attempts: 0,
                    last_error: None,
                })
                .collect()
        };

        // Temp file (same filesystem as the destination → atomic rename).
        let dir = PathBuf::from(&self.record.dir_path);
        let tmp_dir = self.temp_dir_for(&dir);
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| EngineError::permission(format!("cannot create temp folder: {e}")))?;
        let tmp_path = tmp_dir.join(format!("{}.part", self.record.filename));

        let exists = tmp_path.exists();
        let file = open_temp_file(&tmp_path, exists)?;
        *self.shared.file.lock().await = file;
        self.record.temp_path = Some(tmp_path.to_string_lossy().to_string());

        let file_len = self
            .shared
            .file
            .lock()
            .await
            .metadata()
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        // Reconcile segment progress against the actual file size (crash
        // recovery: the file may have been truncated by a power loss).
        let mut shared_segs = Vec::new();
        let mut total_downloaded: i64 = 0;
        for seg in &segs {
            let downloaded = match seg.end_byte {
                Some(_) => seg.downloaded_bytes.min((file_len - seg.start_byte).max(0)),
                None => seg.downloaded_bytes.min(file_len),
            };
            total_downloaded += downloaded;
            let mut s = SegShared::new(
                seg.id,
                self.record.id,
                seg.start_byte,
                seg.end_byte,
                downloaded,
            );
            s.attempts = std::sync::atomic::AtomicI64::new(seg.attempts as i64);
            shared_segs.push(Arc::new(s));
        }
        self.segs = shared_segs;
        self.shared.downloaded.store(total_downloaded, Ordering::SeqCst);
        self.shared
            .total
            .store(probe.total_bytes.unwrap_or(-1), Ordering::SeqCst);
        self.record.downloaded_bytes = total_downloaded;

        // Zero-byte file: everything is already "downloaded".
        if probe.total_bytes == Some(0) {
            for s in &self.segs {
                if let Ok(mut st) = s.status.lock() {
                    *st = SegStatus::Completed;
                }
            }
        }

        // Persist segments (fresh allocation or reconciled state).
        let seg_records: Vec<SegmentRecord> = self
            .segs
            .iter()
            .map(|s| SegmentRecord {
                id: s.id,
                download_id: s.download_id,
                start_byte: s.start,
                end_byte: s.end,
                downloaded_bytes: s.downloaded.load(Ordering::SeqCst),
                status: SegmentStatus::Pending,
                attempts: s.attempts.load(Ordering::SeqCst) as i32,
                last_error: None,
            })
            .collect();
        self.ctx
            .db
            .replace_segments(self.record.id, &seg_records)
            .await?;

        self.pending.clear();
        for (i, s) in self.segs.iter().enumerate() {
            let complete = matches!(*s.status.lock().unwrap(), SegStatus::Completed);
            if !complete && (s.remaining() > 0 || s.end.is_none()) {
                self.pending.push_back(i as i64);
            }
        }

        self.final_path = Some(pathutil::safe_join(&dir, &self.record.filename)?);
        Ok(())
    }

    fn temp_dir_for(&self, dir: &std::path::Path) -> PathBuf {
        if let Some(t) = &self.record_tmp_dir {
            if t.starts_with('/') {
                return PathBuf::from(t);
            }
        }
        dir.join(".ldm-tmp")
    }

    fn spawn_workers(&mut self) {
        if self.stopping {
            return;
        }
        let conn = self.connections.max(1) as usize;
        while self.active.len() < conn {
            let Some(idx) = self.pending.pop_front() else { break };
            if self.shared.pause.load(Ordering::SeqCst)
                || self.shared.cancel.load(Ordering::SeqCst)
            {
                self.pending.push_front(idx);
                break;
            }
            self.active.insert(idx);
            let seg = self.segs[idx as usize].clone();
            if let Ok(mut st) = seg.status.lock() {
                *st = SegStatus::Active;
            }
            let shared = self.shared.clone();
            let id = seg.id;
            let _ = self.ctx.events.send(EngineEvent::SegmentStarted {
                download_id: self.record.id,
                segment_id: id,
            });
            self.jobs.spawn(async move {
                let outcome = run_segment(shared, seg).await;
                (id, outcome)
            });
        }
    }

    async fn on_worker_done(
        &mut self,
        res: Option<std::result::Result<(i64, SegOutcome), tokio::task::JoinError>>,
    ) {
        let (seg_id, outcome) = match res {
            Some(Ok(x)) => x,
            Some(Err(_)) => {
                // Aborted (cancel) or panicked.
                if !self.shared.cancel.load(Ordering::SeqCst) {
                    self.stopping = true;
                    self.shared.pause.store(true, Ordering::SeqCst);
                    self.pending_action = Some(PendingAction::Fail {
                        err: EngineError::unknown(
                            "A download worker stopped unexpectedly. Retrying.",
                        ),
                    });
                }
                self.finish_stop_if_idle().await;
                return;
            }
            None => return,
        };
        let seg_idx = self.segs.iter().position(|s| s.id == seg_id);
        let Some(seg_idx) = seg_idx else { return };
        self.active.remove(&(seg_idx as i64));

        match outcome {
            SegOutcome::Done => {
                if let Ok(mut st) = self.segs[seg_idx].status.lock() {
                    *st = SegStatus::Completed;
                }
                let _ = self.ctx.events.send(EngineEvent::SegmentCompleted {
                    download_id: self.record.id,
                    segment_id: seg_id,
                });
                if self.stopping {
                    self.finish_stop_if_idle().await;
                } else if self.all_completed() {
                    self.finalize().await;
                } else {
                    self.spawn_workers();
                }
            }
            SegOutcome::Paused => {
                if self.shared.cancel.load(Ordering::SeqCst) {
                    self.finish_stop_if_idle().await;
                } else if self.stopping {
                    self.finish_stop_if_idle().await;
                } else if self.jobs.is_empty() {
                    self.finish_pause().await;
                }
            }
            SegOutcome::Cancelled => {
                self.finish_stop_if_idle().await;
            }
            SegOutcome::Failed(err) => {
                if let Ok(mut st) = self.segs[seg_idx].status.lock() {
                    *st = SegStatus::Failed;
                }
                if let Ok(mut le) = self.segs[seg_idx].last_error.lock() {
                    *le = Some(err.message.clone());
                }
                if !self.stopping {
                    self.stopping = true;
                    self.shared.pause.store(true, Ordering::SeqCst);
                    self.apply_retry_decision(err).await;
                }
                self.finish_stop_if_idle().await;
            }
        }
    }

    async fn finish_stop_if_idle(&mut self) {
        if !self.jobs.is_empty() {
            return;
        }
        if self.shared.cancel.load(Ordering::SeqCst) {
            let delete = self.cancel_delete_partial.take().unwrap_or(false);
            self.finish_cancel(delete).await;
            return;
        }
        let Some(action) = self.pending_action.take() else {
            // Workers stopped for a plain pause.
            self.finish_pause().await;
            return;
        };
        match action {
            PendingAction::Retry { err, delay } => {
                self.state = DownloadStatus::Retrying;
                self.set_db_status(DownloadStatus::Retrying).await;
                self.retry_deadline = Some(Instant::now() + delay);
                let _ = self.ctx.events.send(EngineEvent::DownloadRetrying {
                    download_id: self.record.id,
                    attempt: self.record.retry_count,
                    next_retry_in_seconds: delay.as_secs(),
                    error: err.message,
                });
                self.stopping = false;
            }
            PendingAction::Fail { err } => {
                self.fail(err).await;
            }
            PendingAction::PauseWithError { err } => {
                self.record.error = Some(ErrorInfo {
                    kind: format!("{:?}", err.kind).to_lowercase(),
                    code: err.code.clone(),
                    message: err.message.clone(),
                    detail: err.detail.clone(),
                });
                self.finish_pause().await;
            }
            PendingAction::WaitNetwork { err } => {
                let _ = err;
                self.state = DownloadStatus::Retrying;
                self.set_db_status(DownloadStatus::Retrying).await;
                self.offline = true;
                let _ = self.ctx.events.send(EngineEvent::NetworkOffline);
                let _ = self.ctx.mgr.send(ManagerMsg::NetworkOffline);
                // Poll connectivity every 30s; workers restart when back.
                self.retry_deadline = Some(Instant::now() + Duration::from_secs(30));
                self.stopping = false;
            }
        }
    }

    async fn on_retry_fire(&mut self) {
        self.retry_deadline = None;
        self.stopping = false;
        self.shared.pause.store(false, Ordering::SeqCst);
        if self.shared.cancel.load(Ordering::SeqCst) {
            self.finish_cancel(false).await;
            return;
        }
        self.pending.clear();
        self.active.clear();
        for (i, s) in self.segs.iter().enumerate() {
            let done = matches!(*s.status.lock().unwrap(), SegStatus::Completed);
            if !done {
                if let Ok(mut st) = s.status.lock() {
                    *st = SegStatus::Pending;
                }
                self.pending.push_back(i as i64);
            }
        }
        if self.offline {
            self.offline = false;
            let _ = self.ctx.events.send(EngineEvent::NetworkOnline);
            let _ = self.ctx.mgr.send(ManagerMsg::NetworkOnline);
        }
        self.state = DownloadStatus::Downloading;
        self.set_db_status(DownloadStatus::Downloading).await;
        let _ = self.ctx.events.send(EngineEvent::DownloadResumed {
            download_id: self.record.id,
        });
        self.spawn_workers();
    }

    async fn apply_retry_decision(&mut self, err: EngineError) {
        let attempt = self.record.retry_count.max(0) as u32;
        let decision = crate::retry::decide(&err, attempt, self.max_retries, None, &self.backoff);
        match decision {
            RetryDecision::Retry { delay } => {
                self.record.retry_count += 1;
                self.pending_action = Some(PendingAction::Retry { err, delay });
            }
            RetryDecision::GiveUp => {
                self.pending_action = Some(PendingAction::Fail { err });
            }
            RetryDecision::Pause => {
                self.pending_action = Some(PendingAction::PauseWithError { err });
            }
            RetryDecision::WaitForNetwork => {
                self.record.retry_count += 1;
                self.pending_action = Some(PendingAction::WaitNetwork { err });
            }
        }
    }

    async fn on_cmd(&mut self, cmd: TaskCmd) {
        match cmd {
            TaskCmd::Pause => {
                if self.state.is_terminal() {
                    return;
                }
                self.shared.pause.store(true, Ordering::SeqCst);
                if self.jobs.is_empty() && !self.stopping {
                    self.finish_pause().await;
                }
            }
            TaskCmd::Resume => {
                self.resume(false).await;
            }
            TaskCmd::RetryFromScratch => {
                self.resume(true).await;
            }
            TaskCmd::Cancel { delete_partial } => {
                self.shared.cancel.store(true, Ordering::SeqCst);
                self.shared.pause.store(true, Ordering::SeqCst);
                self.retry_deadline = None;
                self.jobs.abort_all();
                self.stopping = true;
                self.cancel_delete_partial = Some(delete_partial);
                if self.jobs.is_empty() {
                    self.finish_cancel(delete_partial).await;
                }
            }
            TaskCmd::SetSpeedLimit(limit) => {
                self.record.speed_limit = limit;
                self.shared
                    .per_download_rate
                    .set_rate(limit.unwrap_or(0) as u64);
                let _ = self
                    .ctx
                    .db
                    .set_download_fields(
                        self.record.id,
                        &[(
                            "speed_limit",
                            rusqlite::types::Value::Integer(limit.unwrap_or(0)),
                        )],
                    )
                    .await;
            }
            TaskCmd::FlushNow => {
                let _ = self.flush_progress().await;
            }
        }
    }

    async fn resume(&mut self, from_scratch: bool) {
        if self.state.is_terminal() {
            return;
        }
        self.force_restart = from_scratch;
        self.shared.pause.store(false, Ordering::SeqCst);
        self.shared.cancel.store(false, Ordering::SeqCst);
        self.stopping = false;
        self.pending_action = None;
        self.retry_deadline = None;
        self.state = DownloadStatus::Queued;
        self.begin_download().await;
    }

    fn all_completed(&self) -> bool {
        !self.segs.is_empty()
            && self
                .segs
                .iter()
                .all(|s| matches!(*s.status.lock().unwrap(), SegStatus::Completed))
    }

    /// All segments done: verify, then atomically install the file.
    async fn finalize(&mut self) {
        if self.shared.pause.load(Ordering::SeqCst)
            || self.shared.cancel.load(Ordering::SeqCst)
            || self.done_flag.load(Ordering::SeqCst)
        {
            self.finish_pause().await;
            return;
        }
        self.state = DownloadStatus::Verifying;
        self.set_db_status(DownloadStatus::Verifying).await;
        let _ = self.ctx.events.send(EngineEvent::DownloadVerifying {
            download_id: self.record.id,
        });

        let sync_res = {
            let f = self.shared.file.lock().await;
            f.sync_all().await
        };
        if let Err(e) = sync_res {
            self.fail(EngineError::disk(e.to_string())).await;
            return;
        }
        let file_len = match {
            let f = self.shared.file.lock().await;
            f.metadata().await
        } {
            Ok(m) => m.len() as i64,
            Err(e) => {
                self.fail(EngineError::disk(e.to_string())).await;
                return;
            }
        };

        // Size verification (always).
        if let Some(total) = self.record.total_bytes {
            if total >= 0 && file_len != total {
                self.fail(
                    EngineError::unknown(format!(
                        "Size mismatch after download: expected {total} bytes, got {file_len}. The file may be corrupt."
                    ))
                    .with_detail("integrity check failed"),
                )
                .await;
                return;
            }
        }

        // Optional checksum verification (streaming, off the async thread).
        if let Some((htype, expected)) = &self.ctx.verify_hash {
            let Some(kind) = hashing::HashType::parse(htype) else {
                self.fail(EngineError::validation("Unknown hash type.")).await;
                return;
            };
            if let Some(path) = &self.record.temp_path {
                let p = PathBuf::from(path);
                let exp = expected.clone();
                let ok = tokio::task::spawn_blocking(move || {
                    hashing::verify_checksum(&p, kind, &exp)
                })
                .await
                .unwrap_or(Ok(false));
                match ok {
                    Ok(true) => {
                        self.record.verification_status = Some("passed".to_string());
                    }
                    Ok(false) => {
                        self.record.verification_status = Some("failed".to_string());
                        self.fail(
                            EngineError::unknown(
                                "Checksum verification failed. The file does not match the expected hash.",
                            )
                            .with_detail("checksum mismatch"),
                        )
                        .await;
                        return;
                    }
                    Err(e) => {
                        self.record.verification_status = Some("error".to_string());
                        self.fail(e).await;
                        return;
                    }
                }
            }
        }
        if self.shared.pause.load(Ordering::SeqCst)
            || self.shared.cancel.load(Ordering::SeqCst)
        {
            self.finish_pause().await;
            return;
        }

        // Atomic install: fsync done, rename temp → final (same filesystem).
        let mut final_path = match &self.final_path {
            Some(p) => p.clone(),
            None => pathutil::safe_join(
                PathBuf::from(&self.record.dir_path).as_path(),
                &self.record.filename,
            )
            .unwrap_or_else(|_| PathBuf::from(&self.record.filename)),
        };
        let dup_policy = self.ctx.settings.read().await.duplicate_policy;
        if final_path.exists() {
            match dup_policy {
                crate::settings::DuplicatePolicy::Overwrite => {}
                _ => {
                    final_path =
                        pathutil::unique_path(final_path.parent().unwrap(), &self.record.filename)
                            .unwrap_or(final_path);
                }
            }
        }
        let tmp = PathBuf::from(self.record.temp_path.as_deref().unwrap_or(""));
        // Linux allows renaming an open file; the handle is closed when the
        // task and its workers are dropped.
        if let Err(e) = fsutil::atomic_rename(&tmp, &final_path) {
            self.fail(EngineError::unknown(format!(
                "Could not move the finished file into place: {e}"
            )))
            .await;
            return;
        }
        // Safe permissions: never executable (spec §110).
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o644));

        self.state = DownloadStatus::Completed;
        self.record.status = DownloadStatus::Completed;
        self.record.filename = final_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(self.record.filename.clone());
        self.record.downloaded_bytes = file_len;
        self.record.completed_at = Some(crate::now_unix());
        self.record.temp_path = None;
        self.record.can_resume = false;
        self.record.updated_at = crate::now_unix();
        if self.record.verification_status.is_none() {
            self.record.verification_status = Some("passed".to_string());
        }
        if let Err(e) = self.ctx.db.update_download(&self.record).await {
            tracing::error!("failed to persist completion: {e}");
        }
        // Emit a final 100% progress event so fast downloads still report a
        // terminal progress state to the UI (spec §79).
        let _ = self.ctx.events.send(EngineEvent::DownloadProgress {
            download_id: self.record.id,
            downloaded_bytes: file_len,
            total_bytes: self.record.total_bytes,
            speed: 0,
            avg_speed: 0,
            peak_speed: 0,
            eta_seconds: Some(0),
            percentage: Some(100.0),
            status: "completed".to_string(),
            segments: Vec::new(),
        });
        let ev = EngineEvent::DownloadCompleted {
            download: self.record.clone(),
        };
        let _ = self.ctx.events.send(ev);
        let _ = self.ctx.mgr.send(ManagerMsg::DownloadFinished(self.record.id));
        self.done_flag.store(true, Ordering::SeqCst);
    }

    async fn fail(&mut self, err: EngineError) {
        self.state = DownloadStatus::Failed;
        self.record.status = DownloadStatus::Failed;
        self.record.error = Some(ErrorInfo {
            kind: format!("{:?}", err.kind).to_lowercase(),
            code: err.code.clone(),
            message: err.message.clone(),
            detail: err.detail.clone(),
        });
        self.record.can_resume = matches!(
            err.kind,
            crate::error::ErrorKind::Network
                | crate::error::ErrorKind::Timeout
                | crate::error::ErrorKind::Dns
                | crate::error::ErrorKind::Tls
                | crate::error::ErrorKind::Protocol
                | crate::error::ErrorKind::Offline
                | crate::error::ErrorKind::Http
        );
        self.record.updated_at = crate::now_unix();
        if let Err(e) = self.ctx.db.update_download(&self.record).await {
            tracing::error!("failed to persist failure: {e}");
        }
        let ev = EngineEvent::DownloadFailed {
            download: self.record.clone(),
            error: err.message.clone(),
        };
        let _ = self.ctx.events.send(ev);
        let _ = self.ctx.mgr.send(ManagerMsg::DownloadFailed(self.record.id));
        self.done_flag.store(true, Ordering::SeqCst);
    }

    async fn finish_pause(&mut self) {
        if self.state == DownloadStatus::Paused {
            return;
        }
        self.record.status = DownloadStatus::Paused;
        self.record.downloaded_bytes = self.shared.downloaded.load(Ordering::SeqCst);
        self.record.updated_at = crate::now_unix();
        if let Err(e) = self.ctx.db.update_download(&self.record).await {
            tracing::error!("failed to persist pause: {e}");
        }
        let mut seg_records: Vec<SegmentRecord> = Vec::new();
        for s in &self.segs {
            seg_records.push(SegmentRecord {
                id: s.id,
                download_id: s.download_id,
                start_byte: s.start,
                end_byte: s.end,
                downloaded_bytes: s.downloaded.load(Ordering::SeqCst),
                status: SegmentStatus::Pending,
                attempts: s.attempts.load(Ordering::SeqCst) as i32,
                last_error: None,
            });
        }
        let _ = self
            .ctx
            .db
            .replace_segments(self.record.id, &seg_records)
            .await;
        self.state = DownloadStatus::Paused;
        let _ = self.ctx.events.send(EngineEvent::DownloadPaused {
            download_id: self.record.id,
        });
        self.stopping = false;
        self.pending_action = None;
        self.retry_deadline = None;
    }

    async fn finish_cancel(&mut self, delete_partial: bool) {
        if self.state.is_terminal() {
            return;
        }
        self.state = DownloadStatus::Cancelled;
        self.record.status = DownloadStatus::Cancelled;
        if delete_partial {
            if let Some(tp) = &self.record.temp_path {
                let _ = std::fs::remove_file(tp);
                self.record.temp_path = None;
            }
            self.record.can_resume = false;
        }
        self.record.updated_at = crate::now_unix();
        if let Err(e) = self.ctx.db.update_download(&self.record).await {
            tracing::error!("failed to persist cancel: {e}");
        }
        let _ = self.ctx.events.send(EngineEvent::DownloadCancelled {
            download_id: self.record.id,
        });
        let _ = self.ctx.mgr.send(ManagerMsg::DownloadCancelled(self.record.id));
        self.done_flag.store(true, Ordering::SeqCst);
    }

    async fn on_tick(&mut self) {
        self.tick_count += 1;
        if self.state.is_active() || self.state == DownloadStatus::Verifying {
            self.emit_progress().await;
        }
        // Debounced DB flush every ~1s.
        if self.tick_count % 5 == 0 {
            let _ = self.flush_progress().await;
        }
        // Periodic disk-space check every ~12s while downloading.
        if self.tick_count % 60 == 0 && self.state.is_active() {
            self.check_disk_space().await;
        }
    }

    async fn emit_progress(&mut self) {
        let downloaded = self.shared.downloaded.load(Ordering::SeqCst);
        let total = self.shared.total.load(Ordering::SeqCst);
        let (inst, avg, peak) = {
            let mut s = self.shared.speed.lock().unwrap();
            let inst = s.instant();
            s.observe(inst);
            (inst, s.short_avg(), s.peak())
        };
        let pct = if total > 0 {
            Some((downloaded as f64 / total as f64) * 100.0)
        } else {
            None
        };
        let eta = if total > 0 && avg > 0 {
            Some(((total - downloaded).max(0) as f64 / avg as f64) as i64)
        } else {
            None
        };
        let segments = self
            .segs
            .iter()
            .map(|s| crate::events::SegmentProgress {
                segment_id: s.id,
                start: s.start,
                end: s.end,
                downloaded: s.downloaded.load(Ordering::SeqCst),
            })
            .collect();
        let _ = self.ctx.events.send(EngineEvent::DownloadProgress {
            download_id: self.record.id,
            downloaded_bytes: downloaded,
            total_bytes: if total >= 0 { Some(total) } else { None },
            speed: inst,
            avg_speed: avg,
            peak_speed: peak,
            eta_seconds: eta,
            percentage: pct,
            status: self.state.as_str().to_string(),
            segments,
        });
    }

    async fn flush_progress(&mut self) -> Result<()> {
        let downloaded = self.shared.downloaded.load(Ordering::SeqCst);
        let total = self.shared.total.load(Ordering::SeqCst);
        let (inst, avg, peak) = {
            let s = self.shared.speed.lock().unwrap();
            (s.instant(), s.short_avg(), s.peak())
        };
        let eta = if total > 0 && avg > 0 {
            Some(((total - downloaded).max(0) as f64 / avg as f64) as i64)
        } else {
            None
        };
        self.record.downloaded_bytes = downloaded;
        self.ctx
            .db
            .update_progress(
                self.record.id,
                downloaded,
                inst,
                avg,
                peak,
                eta,
                self.state.as_str(),
            )
            .await?;
        Ok(())
    }

    async fn check_disk_space(&mut self) {
        let total = self.shared.total.load(Ordering::SeqCst);
        let downloaded = self.shared.downloaded.load(Ordering::SeqCst);
        let remaining = if total > 0 { (total - downloaded).max(0) as u64 } else { 0 };
        let dir = PathBuf::from(&self.record.dir_path);
        let free = fsutil::free_space(&dir).unwrap_or(u64::MAX);
        if remaining > 0 && free < remaining {
            let e = EngineError::disk(format!(
                "Disk space exhausted: {:.1} GB still needed, {:.1} GB available. Download paused — free space and resume.",
                remaining as f64 / 1e9,
                free as f64 / 1e9
            ));
            self.shared.pause.store(true, Ordering::SeqCst);
            self.stopping = true;
            self.pending_action = Some(PendingAction::PauseWithError { err: e });
            if self.jobs.is_empty() {
                self.finish_stop_if_idle().await;
            }
        }
    }

    async fn set_db_status(&mut self, status: DownloadStatus) {
        self.record.status = status;
        self.record.updated_at = crate::now_unix();
        if let Err(e) = self.ctx.db.update_download(&self.record).await {
            tracing::error!("failed to persist status: {e}");
        }
    }
}

fn open_temp_file(path: &std::path::Path, exists: bool) -> Result<tokio::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW);
    if exists {
        opts.open(path)
    } else {
        opts.create_new(true).mode(0o600).open(path)
    }
    .map_err(|e| {
        EngineError::permission(format!("Cannot open temporary file {}: {e}", path.display()))
    })
    .map(tokio::fs::File::from_std)
}

/// Split `[0, total)` into segments. Returns `(start, end_inclusive)` pairs.
pub fn build_ranges(
    total: Option<i64>,
    connections: i32,
    ranges_supported: bool,
) -> Vec<(i64, Option<i64>)> {
    let Some(total) = total else {
        return vec![(0, None)];
    };
    if total <= 0 {
        return vec![(0, Some(0))];
    }
    if !ranges_supported || total < MIN_MULTI_SEGMENT_BYTES || connections <= 1 {
        return vec![(0, Some(total - 1))];
    }
    let max_by_target = ((total as f64) / (SEGMENT_TARGET_BYTES as f64)).ceil() as usize;
    let n = (connections as usize).min(max_by_target).min(MAX_SEGMENTS).max(1);
    let base = total / n as i64;
    let rem = total % n as i64;
    let mut out = Vec::with_capacity(n);
    let mut start = 0i64;
    for i in 0..n {
        let len = base + if (i as i64) < rem { 1 } else { 0 };
        let end = start + len - 1;
        out.push((start, Some(end)));
        start = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_split() {
        // Unknown total → single open-ended segment.
        assert_eq!(build_ranges(None, 8, true), vec![(0, None)]);
        // No range support → single segment.
        assert_eq!(build_ranges(Some(100), 8, false), vec![(0, Some(99))]);
        // Tiny file → single segment.
        assert_eq!(
            build_ranges(Some(10 * 1024), 8, true),
            vec![(0, Some(10 * 1024 - 1))]
        );
        // 1 GB, 8 connections → 8 segments covering everything.
        let ranges = build_ranges(Some(1 << 30), 8, true);
        assert_eq!(ranges.len(), 8);
        let total_covered: i64 = ranges.iter().map(|(s, e)| e.unwrap() - s + 1).sum();
        assert_eq!(total_covered, 1 << 30);
        // No gaps and no overlaps.
        for w in ranges.windows(2) {
            assert_eq!(w[0].1.unwrap() + 1, w[1].0);
        }
        // Respect MAX_SEGMENTS.
        let ranges = build_ranges(Some(1 << 40), 32, true);
        assert!(ranges.len() <= MAX_SEGMENTS);
    }
}
