//! Protocol abstraction (spec §94): the download manager speaks to a
//! `DownloadProtocol`; HTTP is implemented today, FTP/Torrent can be added
//! without touching the core. Async trait methods are stable since Rust 1.75.

use crate::error::Result;
use futures_util::stream::BoxStream;
use serde::Serialize;
use url::Url;

/// Result of probing a resource before downloading.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeInfo {
    /// Total size in bytes; `None` when the server does not tell us.
    pub total_bytes: Option<i64>,
    /// Whether the server honored an actual byte-range request (206).
    pub ranges_supported: bool,
    /// Filename suggested by the server (Content-Disposition), if any.
    pub filename: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub server: Option<String>,
    /// URL after following redirects.
    pub final_url: Option<String>,
    /// Server insisted on Content-Encoding (we preserve bytes as received).
    pub has_content_encoding: bool,
}

/// A streaming response body plus the metadata needed to validate it.
pub struct RangeStream {
    pub status: u16,
    /// Requested inclusive start byte.
    pub requested_start: u64,
    /// Confirmed start byte from `Content-Range` (must equal requested).
    pub content_start: u64,
    /// Inclusive end byte from `Content-Range`, when the server provides one.
    pub content_end: Option<u64>,
    /// Total size from `Content-Range` (`*` → None).
    pub total_size: Option<i64>,
    /// Content-Length of this response body.
    pub content_length: Option<u64>,
    pub final_url: Url,
    pub body: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
}

/// Options that differ per download (auth, headers, cookies, referrer).
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
    pub username: Option<String>,
    pub password: Option<String>,
    pub bearer: Option<String>,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub referrer: Option<String>,
}

/// The protocol interface implemented by HTTP (and, in the future, FTP).
/// `async_trait` keeps it `dyn`-compatible so protocols can be swapped.
#[async_trait::async_trait]
pub trait DownloadProtocol: Send + Sync {
    fn can_handle(&self, url: &Url) -> bool;

    async fn probe(&self, url: &Url, opts: &RequestOptions) -> Result<ProbeInfo>;

    /// Open a stream for `range` (`None` = whole resource). The caller must
    /// validate `Content-Range`/status before writing bytes.
    async fn open_range(
        &self,
        url: &Url,
        range: Option<(u64, Option<u64>)>,
        opts: &RequestOptions,
    ) -> Result<RangeStream>;
}
