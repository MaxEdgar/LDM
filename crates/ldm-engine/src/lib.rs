//! LDM (Linux Download Manager) engine.
//!
//! The engine is a UI-independent library: it owns the download state machine,
//! segmented (multi-connection) downloading, resume, retry, rate limiting,
//! queues, scheduling, categories, and SQLite persistence. The desktop UI,
//! CLI tools, and browser-integration layers all talk to it through the
//! [`DownloadManager`] API and subscribe to [`EngineEvent`]s.
//!
//! The engine must run inside a Tokio runtime.

pub mod categories;
pub mod clipboard;
pub mod db;
pub mod error;
pub mod events;
pub mod fsutil;
pub mod http;
pub mod ipc_server;
pub mod manager;
pub mod model;
pub mod pathutil;
pub mod protocol;
pub mod rate;
pub mod retry;
pub mod scheduler;
pub mod segment;
pub mod settings;
pub mod speed;
pub mod stats;
pub mod task;
pub mod urlutil;
pub mod verify;

pub use db::Db;
pub use error::{EngineError, ErrorKind};
pub use events::EngineEvent;
pub use manager::{AddDownloadOptions, AddOutcome, DownloadManager, ManagerConfig};
pub use model::{
    DownloadFilter, DownloadRecord, DownloadStatus, Priority, SortField, SortOrder,
};
pub use protocol::{ProbeInfo, RangeStream};
pub use settings::Settings;
pub use url::Url;

/// Version reported by the engine (semver, see CHANGELOG).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Current unix epoch seconds.
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}
