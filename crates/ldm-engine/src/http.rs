//! HTTP/HTTPS protocol implementation.
//!
//! Key correctness properties:
//! * Range support is verified with an *actual* `Range: bytes=0-0` request —
//!   `Accept-Ranges` headers alone are not trusted (spec §84).
//! * Every range response is validated against `Content-Range` before any
//!   byte is written (spec §85, §147).
//! * Redirects are limited to 10, restricted to http/https, and never allowed
//!   to downgrade https→http (spec §18, §86).
//! * TLS certificates are always verified; there is no "ignore SSL" switch
//!   (spec §87).
//! * Transparent decompression is disabled: wire bytes are file bytes
//!   (spec §146). If a server insists on `Content-Encoding`, the download
//!   falls back to a single connection and the encoding is preserved on disk
//!   as received (never silently decompressed into a corrupt file).
//! * Credentials are only ever placed in request headers — never logged.

use crate::error::{EngineError, Result};
use crate::protocol::{DownloadProtocol, ProbeInfo, RangeStream, RequestOptions};
use crate::settings::Settings;
use base64::Engine as _;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const MAX_REDIRECTS: usize = 10;

pub struct HttpProtocol {
    settings: Arc<tokio::sync::RwLock<Settings>>,
    probe_client: tokio::sync::OnceCell<reqwest::Client>,
}

impl HttpProtocol {
    pub fn new(settings: Arc<tokio::sync::RwLock<Settings>>) -> Self {
        Self {
            settings,
            probe_client: tokio::sync::OnceCell::new(),
        }
    }

    /// Build a client for one download. Cookie stores are per-download so
    /// cookies never leak between downloads.
    pub async fn make_client(&self, opts: &RequestOptions) -> Result<reqwest::Client> {
        let s = self.settings.read().await;
        let mut builder = reqwest::Client::builder()
            .user_agent(s.user_agent.clone())
            .connect_timeout(Duration::from_secs(s.connect_timeout_seconds))
            .redirect(redirect_policy())
            .pool_max_idle_per_host(8);
        // Idle/read timeout: any period without data longer than this aborts
        // the connection so a dead server can't hold a download forever.
        builder = builder.read_timeout(Duration::from_secs(s.read_timeout_seconds));
        if opts.cookies.is_empty() {
            builder = builder.cookie_store(false);
        } else {
            builder = builder.cookie_store(true);
        }
        if let Some(proxy) = proxy_for(&s)? {
            builder = builder.proxy(proxy);
        }
        builder.build().map_err(|e| EngineError::from_reqwest(&e))
    }

    async fn shared_probe_client(&self) -> Result<reqwest::Client> {
        if let Some(c) = self.probe_client.get() {
            return Ok(c.clone());
        }
        let s = self.settings.read().await;
        let mut builder = reqwest::Client::builder()
            .user_agent(s.user_agent.clone())
            .connect_timeout(Duration::from_secs(s.connect_timeout_seconds))
            .redirect(redirect_policy())
            .read_timeout(Duration::from_secs(s.read_timeout_seconds))
            .cookie_store(false);
        if let Some(proxy) = proxy_for(&s)? {
            builder = builder.proxy(proxy);
        }
        let client = builder.build().map_err(|e| EngineError::from_reqwest(&e))?;
        let _ = self.probe_client.set(client.clone());
        Ok(client)
    }

    /// Apply auth headers/cookies/referrer/custom headers to a request builder.
    fn apply_opts(
        &self,
        req: reqwest::RequestBuilder,
        opts: &RequestOptions,
    ) -> reqwest::RequestBuilder {
        let mut req = req;
        if let (Some(u), Some(p)) = (&opts.username, &opts.password) {
            let cred = base64::engine::general_purpose::STANDARD
                .encode(format!("{u}:{p}").as_bytes());
            req = req.header(reqwest::header::AUTHORIZATION, format!("Basic {cred}"));
        } else if let Some(token) = &opts.bearer {
            req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(referrer) = &opts.referrer {
            req = req.header(reqwest::header::REFERER, referrer);
        }
        for (k, v) in &opts.headers {
            if !k.eq_ignore_ascii_case("authorization")
                && !k.eq_ignore_ascii_case("cookie")
                && !k.eq_ignore_ascii_case("host")
            {
                if let (Ok(kh), Ok(vh)) = (
                    reqwest::header::HeaderName::try_from(k.as_str()),
                    reqwest::header::HeaderValue::try_from(v.as_str()),
                ) {
                    req = req.header(kh, vh);
                }
            }
        }
        if !opts.cookies.is_empty() {
            let joined = opts
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            if let Ok(v) = reqwest::header::HeaderValue::try_from(joined) {
                req = req.header(reqwest::header::COOKIE, v);
            }
        }
        req
    }
}

/// Redirect policy: max 10 hops, http/https only, no https→http downgrade.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let target = attempt.url();
        if !matches!(target.scheme(), "http" | "https") {
            return attempt.error("redirect to an unsupported URL scheme");
        }
        let from = attempt
            .previous()
            .last()
            .cloned()
            .unwrap_or_else(|| attempt.url().clone());
        if from.scheme() == "https" && target.scheme() == "http" {
            return attempt.error("refusing to follow a redirect from HTTPS to HTTP");
        }
        attempt.follow()
    })
}

fn proxy_for(s: &Settings) -> Result<Option<reqwest::Proxy>> {
    match s.proxy_mode {
        crate::settings::ProxyMode::None => Ok(None),
        crate::settings::ProxyMode::Custom => {
            let url = s.proxy_url.trim();
            if url.is_empty() {
                return Ok(None);
            }
            let p = reqwest::Proxy::all(url)
                .map_err(|e| EngineError::validation(format!("Invalid proxy URL: {e}")))?;
            Ok(Some(p))
        }
        crate::settings::ProxyMode::System => {
            // http_proxy/https_proxy/all_proxy env vars are honored by
            // reqwest automatically; nothing to configure here.
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl DownloadProtocol for HttpProtocol {
    fn can_handle(&self, url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https")
    }

    async fn probe(&self, url: &Url, opts: &RequestOptions) -> Result<ProbeInfo> {
        let client = self.shared_probe_client().await?;
        let mut req = client.get(url.clone());
        req = self.apply_opts(req, opts);
        // Actual range verification: request the first byte.
        req = req.header(reqwest::header::RANGE, "bytes=0-0");
        let resp = req
            .send()
            .await
            .map_err(|e| classify_connect(&e, url))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let final_url = resp.url().clone();

        let filename = headers
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(crate::urlutil::filename_from_content_disposition);
        let etag = header_str(&headers, "etag");
        let last_modified = header_str(&headers, "last-modified");
        let content_type = header_str(&headers, "content-type");
        let server = header_str(&headers, "server");
        let has_content_encoding = header_str(&headers, "content-encoding")
            .map(|e| !e.is_empty() && e != "identity")
            .unwrap_or(false);

        match status {
            206 => {
                // Ranges supported. Validate Content-Range.
                let cr = header_str(&headers, "content-range").ok_or_else(|| {
                    EngineError::protocol("Server sent 206 without Content-Range.")
                })?;
                let (start, _end, total) = parse_content_range(&cr).ok_or_else(|| {
                    EngineError::protocol(format!("Malformed Content-Range: {cr}"))
                })?;
                if start != 0 {
                    return Err(EngineError::protocol(format!(
                        "Server answered a range request starting at byte {start} instead of 0."
                    )));
                }
                Ok(ProbeInfo {
                    total_bytes: total,
                    ranges_supported: true,
                    filename,
                    etag,
                    last_modified,
                    content_type,
                    server,
                    final_url: Some(final_url.to_string()),
                    has_content_encoding,
                })
            }
            200 => {
                // Server ignored our Range header → no range support.
                // Drop the body without reading it.
                drop(resp);
                let total = header_len(&headers);
                Ok(ProbeInfo {
                    total_bytes: total,
                    ranges_supported: false,
                    filename,
                    etag,
                    last_modified,
                    content_type,
                    server,
                    final_url: Some(final_url.to_string()),
                    has_content_encoding,
                })
            }
            416 => {
                // Range not satisfiable: the file is empty or sizes mismatch.
                let total = header_len(&headers);
                Ok(ProbeInfo {
                    total_bytes: total.or(Some(0)),
                    ranges_supported: true,
                    filename,
                    etag,
                    last_modified,
                    content_type,
                    server,
                    final_url: Some(final_url.to_string()),
                    has_content_encoding,
                })
            }
            429 => Err(http_error_with_retry_after(&headers)),
            401 | 407 => Err(EngineError::auth()),
            other => Err(EngineError::http(other)),
        }
    }

    async fn open_range(
        &self,
        url: &Url,
        range: Option<(u64, Option<u64>)>,
        opts: &RequestOptions,
    ) -> Result<RangeStream> {
        let client = self.make_client(opts).await?;
        let mut req = client.get(url.clone());
        req = self.apply_opts(req, opts);
        if let Some((start, end)) = range {
            let header = match end {
                Some(e) => format!("bytes={start}-{e}"),
                None => format!("bytes={start}-"),
            };
            req = req.header(reqwest::header::RANGE, header);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| classify_connect(&e, url))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let final_url = resp.url().clone();
        let content_length = resp.content_length();
        let has_content_encoding = header_str(&headers, "content-encoding")
            .map(|e| !e.is_empty() && e != "identity")
            .unwrap_or(false);

        match (range, status) {
            (Some((start, _)), 206) => {
                let cr = header_str(&headers, "content-range").ok_or_else(|| {
                    EngineError::protocol("Server sent 206 without Content-Range.")
                })?;
                let (cs, ce, total) = parse_content_range(&cr).ok_or_else(|| {
                    EngineError::protocol(format!("Malformed Content-Range: {cr}"))
                })?;
                if cs != start {
                    return Err(EngineError::protocol(format!(
                        "Segment expected bytes starting at {start} but server sent {cs}. Refusing to write (would corrupt the file)."
                    )));
                }
                if let Some(req_end) = range.and_then(|(_, e)| e) {
                    if let Some(ce) = ce {
                        if ce > req_end {
                            return Err(EngineError::protocol(format!(
                                "Server sent a range ending at {ce} but {req_end} was requested."
                            )));
                        }
                    }
                }
                if has_content_encoding {
                    return Err(EngineError::protocol(
                        "Server sent a Content-Encoding for a range request; refusing to write compressed bytes into a file.",
                    ));
                }
                Ok(RangeStream {
                    status,
                    requested_start: start,
                    content_start: cs,
                    content_end: ce,
                    total_size: total,
                    content_length,
                    final_url,
                    body: Box::pin(resp.bytes_stream()),
                })
            }
            (Some(_), 200) => {
                // Server ignored the Range header mid-download. Never write
                // this response into a segment.
                Err(EngineError::protocol(
                    "Server ignored a byte-range request and returned the whole file. Multi-connection download cannot continue.",
                ))
            }
            (None, 200) => Ok(RangeStream {
                status,
                requested_start: 0,
                content_start: 0,
                content_end: None,
                total_size: content_length.map(|l| l as i64),
                content_length,
                final_url,
                body: Box::pin(resp.bytes_stream()),
            }),
            (None, 206) => {
                // Server sent 206 even though we didn't ask for a range
                // (weird but harmless) — treat like a normal stream.
                let (cs, ce, total) = header_str(&headers, "content-range")
                    .and_then(|cr| parse_content_range(&cr))
                    .unwrap_or((0, None, None));
                Ok(RangeStream {
                    status,
                    requested_start: 0,
                    content_start: cs,
                    content_end: ce,
                    total_size: total,
                    content_length,
                    final_url,
                    body: Box::pin(resp.bytes_stream()),
                })
            }
            (_, 429) => Err(http_error_with_retry_after(&headers)),
            (_, 401) | (_, 407) => Err(EngineError::auth()),
            (_, other) => Err(EngineError::http(other)),
        }
    }
}

fn http_error_with_retry_after(headers: &reqwest::header::HeaderMap) -> EngineError {
    let mut e = EngineError::http(429);
    if let Some(ra) = header_str(headers, "retry-after") {
        if let Ok(secs) = ra.trim().parse::<u64>() {
            e = e.with_retry_after(secs);
        } else if let Ok(instant) =
            httpdate::parse_http_date(&ra)
        {
            let now = std::time::SystemTime::now();
            if let Ok(d) = instant.duration_since(now) {
                e = e.with_retry_after(d.as_secs().max(1));
            }
        }
    }
    e
}

fn header_str(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

fn header_len(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
}

/// Parse `bytes START-END/TOTAL` (TOTAL may be `*`).
fn parse_content_range(cr: &str) -> Option<(u64, Option<u64>, Option<i64>)> {
    let rest = cr.trim().strip_prefix("bytes")?.trim();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok();
    let total = if total.trim() == "*" {
        None
    } else {
        total.trim().parse::<i64>().ok()
    };
    Some((start, end, total))
}

fn classify_connect(err: &reqwest::Error, url: &Url) -> EngineError {
    // Never include the URL's query string (may contain tokens) in messages.
    let mut e = EngineError::from_reqwest(err);
    if e.detail.is_none() {
        e.detail = Some(format!("while connecting to {}", crate::urlutil::host_for_display(url)));
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(
            parse_content_range("bytes 0-0/1024"),
            Some((0, Some(0), Some(1024)))
        );
        assert_eq!(
            parse_content_range("bytes 100-199/*"),
            Some((100, Some(199), None))
        );
        assert_eq!(parse_content_range("bytes 0-0/512"), Some((0, Some(0), Some(512))));
        assert!(parse_content_range("garbage").is_none());
    }
}
