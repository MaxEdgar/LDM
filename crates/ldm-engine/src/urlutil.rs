//! URL handling: validation, filename extraction (URL + Content-Disposition
//! with RFC 5987 `filename*`), and display-safe query redaction.
//!
//! Security notes:
//! * Only `http`/`https` URLs are accepted by the engine in this release.
//! * URLs containing embedded credentials (`user:pass@host`) are rejected;
//!   credentials must be supplied through the dedicated auth fields.
//! * The actual download URL is never modified for privacy (signed URLs must
//!   keep their query parameters); redaction only applies to display/history.

use crate::error::{EngineError, Result};
use percent_encoding::percent_decode_str;
use url::Url;

/// Parse and validate a download URL.
pub fn validate_url(input: &str) -> Result<Url> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(EngineError::validation("Please enter a URL."));
    }
    let url = Url::parse(trimmed).map_err(|e| {
        EngineError::validation(format!("This does not look like a valid URL: {e}"))
    })?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(EngineError::validation(format!(
                "Unsupported URL scheme \"{other}\". Only http and https are supported."
            )))
        }
    }
    if url.host_str().is_none() {
        return Err(EngineError::validation(
            "The URL has no host. Did you mean to prefix it with https:// ?",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EngineError::validation(
            "Embedded credentials in the URL are not supported. Use the username/password fields instead.",
        ));
    }
    Ok(url)
}

/// Extract a filename from a URL's path (percent-decoded, no directory).
/// Returns `None` when the URL has no usable path segment.
pub fn filename_from_url(url: &Url) -> Option<String> {
    let path = url.path();
    let seg = path.rsplit('/').next().unwrap_or("");
    if seg.is_empty() || seg == "/" || seg.ends_with('/') {
        return None;
    }
    let decoded = percent_decode_str(seg).decode_utf8_lossy().to_string();
    let cleaned = crate::pathutil::sanitize_filename(&decoded);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Parse the `Content-Disposition` header value and return a usable filename.
/// Supports both `filename=` and RFC 5987 `filename*=UTF-8''...`.
pub fn filename_from_content_disposition(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if part.to_ascii_lowercase().starts_with("filename*=") {
            let raw = &part["filename*=".len()..];
            if let Some(decoded) = decode_rfc5987(raw) {
                let cleaned = crate::pathutil::sanitize_filename(&decoded);
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }
    for part in value.split(';') {
        let part = part.trim();
        if part.to_ascii_lowercase().starts_with("filename=") {
            let raw = &part["filename=".len()..];
            let raw = raw.trim();
            let raw = raw.trim_matches('"');
            let decoded = percent_decode_str(raw).decode_utf8_lossy().to_string();
            let cleaned = crate::pathutil::sanitize_filename(&decoded);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Decode an RFC 5987 encoded value like `UTF-8''caf%C3%A9.txt`.
fn decode_rfc5987(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches('"');
    let (charset, rest) = raw.split_once("''")?;
    if !charset.eq_ignore_ascii_case("utf-8") {
        // Only UTF-8 is supported; fall through to plain filename handling.
        return None;
    }
    percent_decode_str(rest).decode_utf8().ok().map(|c| c.to_string())
}

/// Parameters commonly used for authentication/signing that should be hidden
/// when a URL is displayed or stored in history. The download URL itself is
/// never modified.
const SENSITIVE_PARAMS: &[&str] = &[
    "token", "access_token", "auth", "authorization", "signature", "sig", "key", "apikey",
    "api_key", "session", "sessionid", "session_id", "sid", "password", "passwd", "secret",
    "credential", "code", "ticket", "expires", "x-amz-signature", "x-amz-credential",
    "x-amz-security-token", "awsaccesskeyid", "policy", "jwt",
];

/// Return a display-safe copy of the URL with sensitive query parameters
/// redacted. Used only for display and history; never for downloading.
pub fn redact_url(url: &Url) -> String {
    let mut redacted = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if pairs.is_empty() {
        return url.to_string();
    }
    let kept: Vec<(String, String)> = pairs
        .into_iter()
        .filter(|(k, _)| !SENSITIVE_PARAMS.contains(&k.to_ascii_lowercase().as_str()))
        .collect();
    if kept.len() == url.query_pairs().count() {
        return url.to_string();
    }
    redacted.set_query(None);
    if !kept.is_empty() {
        redacted.set_query(Some(
            &kept
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&"),
        ));
    }
    redacted.to_string()
}

/// Return the host part of a URL for display (e.g. "example.com").
pub fn host_for_display(url: &Url) -> String {
    url.host_str().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_urls() {
        assert!(validate_url("https://example.com/file.zip").is_ok());
        assert!(validate_url("http://example.com/file.zip").is_ok());
        assert!(validate_url("example.com/file.zip").is_err());
        assert!(validate_url("ftp://example.com/x").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("https://user:pass@example.com/x").is_err());
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("").is_err());
        assert!(validate_url("https://").is_err());
    }

    #[test]
    fn extracts_filename_from_url() {
        let u = Url::parse("https://example.com/dir/Ubuntu.iso?x=1").unwrap();
        assert_eq!(filename_from_url(&u).unwrap(), "Ubuntu.iso");
        let u = Url::parse("https://example.com/dir/my%20file.zip").unwrap();
        assert_eq!(filename_from_url(&u).unwrap(), "my file.zip");
        let u = Url::parse("https://example.com/").unwrap();
        assert!(filename_from_url(&u).is_none());
        let u = Url::parse("https://example.com").unwrap();
        assert!(filename_from_url(&u).is_none());
    }

    #[test]
    fn content_disposition_parsing() {
        assert_eq!(
            filename_from_content_disposition("attachment; filename=\"file.zip\"").unwrap(),
            "file.zip"
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename=plain.zip").unwrap(),
            "plain.zip"
        );
        assert_eq!(
            filename_from_content_disposition("attachment; filename*=UTF-8''caf%C3%A9.txt")
                .unwrap(),
            "café.txt"
        );
        // filename* wins over filename
        assert_eq!(
            filename_from_content_disposition(
                "attachment; filename=\"old.txt\"; filename*=UTF-8''new%20file.txt"
            )
            .unwrap(),
            "new file.txt"
        );
        assert!(filename_from_content_disposition("inline").is_none());
    }

    #[test]
    fn redacts_sensitive_params() {
        let u = Url::parse("https://example.com/dl?file=a&token=abc123&x=1").unwrap();
        let r = redact_url(&u);
        assert!(!r.contains("abc123"));
        assert!(r.contains("x=1"));
        assert!(r.contains("file=a"));
        // Signed URLs keep non-sensitive params
        let u = Url::parse("https://example.com/dl?X-Amz-Algorithm=AWS4-HMAC-SHA256&a=b").unwrap();
        assert!(redact_url(&u).contains("a=b"));
        // No query → unchanged
        let u = Url::parse("https://example.com/dl").unwrap();
        assert_eq!(redact_url(&u), "https://example.com/dl");
    }
}
