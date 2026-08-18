//! Core data models: download records, statuses, filters.

use serde::{Deserialize, Serialize};

/// Deterministic download lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DownloadStatus {
    /// Waiting for an active slot in the queue.
    Queued,
    /// Preparing to start (probing server, allocating segments).
    Starting,
    /// Establishing the connection.
    Connecting,
    /// Actively downloading.
    Downloading,
    /// Paused by the user or system.
    Paused,
    /// Successfully finished and verified.
    Completed,
    /// Failed permanently (after retries exhausted / non-retryable error).
    Failed,
    /// Cancelled by the user.
    Cancelled,
    /// Verifying size/hash before completion.
    Verifying,
    /// Waiting to retry after a transient error.
    Retrying,
    /// Waiting for its scheduled start time.
    Scheduled,
}

impl DownloadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DownloadStatus::Queued => "QUEUED",
            DownloadStatus::Starting => "STARTING",
            DownloadStatus::Connecting => "CONNECTING",
            DownloadStatus::Downloading => "DOWNLOADING",
            DownloadStatus::Paused => "PAUSED",
            DownloadStatus::Completed => "COMPLETED",
            DownloadStatus::Failed => "FAILED",
            DownloadStatus::Cancelled => "CANCELLED",
            DownloadStatus::Verifying => "VERIFYING",
            DownloadStatus::Retrying => "RETRYING",
            DownloadStatus::Scheduled => "SCHEDULED",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Starting
                | DownloadStatus::Connecting
                | DownloadStatus::Downloading
                | DownloadStatus::Verifying
                | DownloadStatus::Retrying
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Completed | DownloadStatus::Failed | DownloadStatus::Cancelled
        )
    }
}

impl std::fmt::Display for DownloadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low = -1,
    Normal = 0,
    High = 1,
}

impl Priority {
    pub fn from_i32(v: i32) -> Self {
        match v {
            v if v <= -1 => Priority::Low,
            v if v >= 1 => Priority::High,
            _ => Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentStatus {
    Pending,
    Active,
    Completed,
    Failed,
}

impl SegmentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SegmentStatus::Pending => "PENDING",
            SegmentStatus::Active => "ACTIVE",
            SegmentStatus::Completed => "COMPLETED",
            SegmentStatus::Failed => "FAILED",
        }
    }
}

/// One byte-range of a segmented download (persisted in the `segments` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRecord {
    pub id: i64,
    pub download_id: i64,
    /// Inclusive start byte.
    pub start_byte: i64,
    /// Inclusive end byte, `None` when the total size is unknown.
    pub end_byte: Option<i64>,
    /// Bytes of this segment already written.
    pub downloaded_bytes: i64,
    pub status: SegmentStatus,
    pub attempts: i32,
    pub last_error: Option<String>,
}

impl SegmentRecord {
    pub fn remaining(&self) -> i64 {
        match self.end_byte {
            Some(end) => (end - self.start_byte + 1).saturating_sub(self.downloaded_bytes).max(0),
            None => 0,
        }
    }
}

/// A download as exposed to the UI (mirrors the `downloads` table row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub id: i64,
    pub url: String,
    /// URL after redirects, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    /// Filename without directory.
    pub filename: String,
    /// Destination directory.
    pub dir_path: String,
    /// Temporary partial file path while downloading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_path: Option<String>,
    pub category: String,
    pub status: DownloadStatus,
    /// Total size in bytes; `None` while unknown.
    pub total_bytes: Option<i64>,
    pub downloaded_bytes: i64,
    pub current_speed: u64,
    pub avg_speed: u64,
    pub peak_speed: u64,
    /// Remaining seconds estimate.
    pub eta_seconds: Option<i64>,
    pub connections: i32,
    pub priority: Priority,
    /// Per-download speed limit in bytes/sec.
    pub speed_limit: Option<i64>,
    /// Basic-auth username (not a secret; persisted for convenience).
    pub username: Option<String>,
    /// Key into the in-memory credential store (never persisted).
    pub password_ref: Option<String>,
    /// Referrer header for the download.
    pub referrer: Option<String>,
    pub protocol: String,
    pub server: Option<String>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
    pub error: Option<ErrorInfo>,
    /// Verification fields (optional user-provided checksum).
    pub verify_hash: Option<String>,
    pub verify_type: Option<String>,
    pub verification_status: Option<String>,
    pub queue_name: Option<String>,
    /// Epoch seconds at which a scheduled download may start.
    pub scheduled_start: Option<i64>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub updated_at: i64,
    /// Present when the download is paused due to an error (recoverable).
    pub can_resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub kind: String,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// View filters for listing downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadFilter {
    All,
    Active,
    Paused,
    Completed,
    Failed,
    Queued,
    Scheduled,
    Cancelled,
}

impl DownloadFilter {
    pub fn matches(&self, status: DownloadStatus) -> bool {
        match self {
            DownloadFilter::All => true,
            DownloadFilter::Active => status.is_active(),
            DownloadFilter::Paused => status == DownloadStatus::Paused,
            DownloadFilter::Completed => status == DownloadStatus::Completed,
            DownloadFilter::Failed => status == DownloadStatus::Failed,
            DownloadFilter::Queued => status == DownloadStatus::Queued,
            DownloadFilter::Scheduled => status == DownloadStatus::Scheduled,
            DownloadFilter::Cancelled => status == DownloadStatus::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    Name,
    Size,
    Progress,
    Speed,
    Status,
    DateAdded,
    Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    Desc,
}

/// SQL ORDER BY clause for a sort specification.
pub fn sort_clause(field: SortField, order: SortOrder) -> String {
    let col = match field {
        SortField::Name => "filename COLLATE NOCASE",
        SortField::Size => "COALESCE(total_bytes, -1)",
        SortField::Progress => "downloaded_bytes",
        SortField::Speed => "current_speed",
        SortField::Status => "status",
        SortField::DateAdded => "created_at",
        SortField::Category => "category COLLATE NOCASE",
    };
    let dir = match order {
        SortOrder::Asc => "ASC",
        SortOrder::Desc => "DESC",
    };
    format!("{col} {dir}")
}

/// Numeric formatting helpers shared by engine and UI-facing JSON.
pub mod fmt {
    /// Human-readable byte size, e.g. "4.2 GB".
    pub fn bytes(n: i64) -> String {
        const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
        let mut v = n as f64;
        let mut i = 0;
        while v >= 1024.0 && i < UNITS.len() - 1 {
            v /= 1024.0;
            i += 1;
        }
        if i == 0 {
            format!("{} B", n)
        } else {
            format!("{:.1} {}", v, UNITS[i])
        }
    }

    /// Human-readable duration, e.g. "1m 27s".
    pub fn duration(secs: i64) -> String {
        if secs < 0 {
            return "—".to_string();
        }
        if secs < 60 {
            return format!("{}s", secs);
        }
        if secs < 3600 {
            return format!("{}m {:02}s", secs / 60, secs % 60);
        }
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{}h {:02}m", h, m)
    }
}
