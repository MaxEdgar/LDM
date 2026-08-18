//! Engine event system. The engine emits these on a broadcast channel; the UI
//! (or any other consumer) subscribes and updates without polling the database.

use crate::model::DownloadRecord;
use serde::Serialize;

/// Events emitted by the engine. Tagged by `type` for JSON consumers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    DownloadCreated {
        download: DownloadRecord,
    },
    DownloadQueued {
        download_id: i64,
    },
    DownloadStarted {
        download_id: i64,
    },
    DownloadConnecting {
        download_id: i64,
    },
    /// Throttled to a few updates per second per download.
    DownloadProgress {
        download_id: i64,
        downloaded_bytes: i64,
        total_bytes: Option<i64>,
        speed: u64,
        avg_speed: u64,
        peak_speed: u64,
        eta_seconds: Option<i64>,
        percentage: Option<f64>,
        status: String,
        /// Per-segment byte progress (for connection visualisation).
        segments: Vec<SegmentProgress>,
    },
    DownloadPaused {
        download_id: i64,
    },
    DownloadResumed {
        download_id: i64,
    },
    DownloadRetrying {
        download_id: i64,
        attempt: i32,
        next_retry_in_seconds: u64,
        error: String,
    },
    DownloadVerifying {
        download_id: i64,
    },
    DownloadCompleted {
        download: DownloadRecord,
    },
    DownloadFailed {
        download: DownloadRecord,
        error: String,
    },
    DownloadCancelled {
        download_id: i64,
    },
    DownloadRemoved {
        download_id: i64,
    },
    DownloadUpdated {
        download: DownloadRecord,
    },
    SegmentStarted {
        download_id: i64,
        segment_id: i64,
    },
    SegmentCompleted {
        download_id: i64,
        segment_id: i64,
    },
    NetworkOffline,
    NetworkOnline,
    /// A URL was detected on the clipboard (only when monitoring is enabled).
    ClipboardUrlDetected {
        url: String,
        filename: Option<String>,
    },
    /// A download was requested through browser integration.
    BrowserDownloadRequested {
        url: String,
        filename: Option<String>,
        referrer: Option<String>,
    },
    QueueChanged,
    SchedulerStateChanged {
        active: bool,
    },
    /// The download queue is fully idle (nothing active or queued) and the
    /// `after_completion` setting is enabled - the UI should confirm and act.
    DownloadsIdle,
}

/// Byte progress of one segment, carried inside [`EngineEvent::DownloadProgress`]
/// so the UI can draw per-connection bars (spec §63).
#[derive(Debug, Clone, Serialize)]
pub struct SegmentProgress {
    pub segment_id: i64,
    pub start: i64,
    pub end: Option<i64>,
    pub downloaded: i64,
}

/// Subscriber handle returned by [`crate::DownloadManager::subscribe`].
pub type EventReceiver = tokio::sync::broadcast::Receiver<EngineEvent>;
