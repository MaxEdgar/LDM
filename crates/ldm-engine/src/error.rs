//! Structured, typed errors. The UI maps these to user-friendly, localizable
//! messages; they are never formatted as opaque codes.

use std::fmt;

/// Broad error class used to select a recovery strategy (see [`crate::retry`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Network,
    Timeout,
    Dns,
    Tls,
    Http,
    Authentication,
    Disk,
    Permission,
    Validation,
    Cancelled,
    RemoteChanged,
    Offline,
    Protocol,
    Database,
    Unknown,
}

/// A structured engine error with a stable kind, a short code, and a
/// human-readable message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub code: String,
    pub message: String,
    /// Machine-readable detail, safe to show in expandable UI sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Server-provided `Retry-After` (seconds) when the failure was HTTP 429.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

impl EngineError {
    pub fn new(kind: ErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            detail: None,
            retry_after: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, "validation_error", message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Network, "network_error", message)
    }

    pub fn timeout() -> Self {
        Self::new(
            ErrorKind::Timeout,
            "connection_timeout",
            "The connection timed out.",
        )
    }

    pub fn dns(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Dns, "dns_error", message)
    }

    pub fn tls(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Tls, "tls_error", message)
    }

    pub fn http(status: u16) -> Self {
        Self::new(
            ErrorKind::Http,
            "http_error",
            format!("The server returned HTTP status {status}."),
        )
        .with_detail(format!("HTTP {status}"))
    }

    pub fn auth() -> Self {
        Self::new(
            ErrorKind::Authentication,
            "authentication_failed",
            "Authentication failed. Please check your credentials.",
        )
    }

    pub fn disk(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Disk, "disk_error", message)
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Permission, "permission_denied", message)
    }

    pub fn cancelled() -> Self {
        Self::new(
            ErrorKind::Cancelled,
            "cancelled",
            "The download was cancelled.",
        )
    }

    pub fn remote_changed() -> Self {
        Self::new(
            ErrorKind::RemoteChanged,
            "remote_changed",
            "The remote file appears to have changed.",
        )
    }

    pub fn offline() -> Self {
        Self::new(
            ErrorKind::Offline,
            "network_offline",
            "The network appears to be unavailable.",
        )
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, "protocol_error", message)
    }

    pub fn database(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Database, "database_error", message)
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unknown, "unknown_error", message)
    }

    /// Classify a reqwest error into a structured error.
    pub fn from_reqwest(err: &reqwest::Error) -> Self {
        if err.is_timeout() {
            return Self::timeout();
        }
        if err.is_connect() {
            let msg = err.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("dns") || lower.contains("resolve") || lower.contains("nodename")
                || lower.contains("name or service")
            {
                return Self::dns(msg);
            }
            if lower.contains("certificate")
                || lower.contains("ssl")
                || lower.contains("tls")
                || lower.contains("handshake")
            {
                return Self::tls("The server's TLS certificate could not be verified.")
                    .with_detail(msg);
            }
            return Self::network(msg);
        }
        if err.is_body() {
            return Self::network("The connection was closed while reading data.")
                .with_detail(err.to_string());
        }
        if err.is_decode() {
            return Self::protocol("The server sent data that could not be decoded.")
                .with_detail(err.to_string());
        }
        if err.is_redirect() {
            // Redirect loops / policy rejections are permanent: retrying will
            // never succeed. Distinct code so the retry policy gives up.
            return Self::new(
                ErrorKind::Protocol,
                "redirect_error",
                "The server redirected too many times or into a loop.",
            )
            .with_detail(err.to_string());
        }
        let msg = err.to_string();
        let lower = msg.to_ascii_lowercase();
        if lower.contains("certificate") || lower.contains("tls") || lower.contains("ssl") {
            return Self::tls("The server's TLS certificate could not be verified.")
                .with_detail(msg);
        }
        Self::network(msg)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        Self::database(e.to_string())
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        Self::validation(e.to_string())
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => Self::validation(e.to_string()),
            K::PermissionDenied => Self::permission(e.to_string()),
            K::StorageFull | K::WriteZero => Self::disk(e.to_string()),
            _ => Self::unknown(e.to_string()),
        }
    }
}

pub type Result<T, E = EngineError> = std::result::Result<T, E>;
