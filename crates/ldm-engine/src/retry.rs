//! Retry policy: exponential backoff with jitter, per-error-kind decisions,
//! and `Retry-After` header support (spec §17, §141, §142, §210).

use crate::error::{EngineError, ErrorKind};
use rand::Rng;
use std::time::Duration;

/// Decision made by the retry policy for a failed attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry after the given delay.
    Retry { delay: Duration },
    /// Give up; mark the download failed.
    GiveUp,
    /// Pause the download and wait for the user.
    Pause,
    /// Network appears offline; wait for connectivity before retrying.
    WaitForNetwork,
}

/// Exponential backoff: base * factor^attempt, plus up to `jitter` fraction
/// of random delay, capped at `max`.
pub struct Backoff {
    pub base: Duration,
    pub factor: f64,
    pub max: Duration,
    pub jitter: f64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(2),
            factor: 2.0,
            max: Duration::from_secs(300),
            jitter: 0.3,
        }
    }
}

impl Backoff {
    /// Delay for attempt `n` (0-based).
    pub fn delay_for(&self, attempt: u32) -> Duration {
        let exp = self.base.as_secs_f64() * self.factor.powi(attempt as i32);
        let jittered = exp * (1.0 + self.jitter * rand::thread_rng().gen::<f64>());
        let secs = jittered.min(self.max.as_secs_f64()).max(0.05);
        Duration::from_secs_f64(secs)
    }
}

/// Classify an error into a retry decision given the remaining attempts and
/// an optional `Retry-After` value supplied by the server.
pub fn decide(
    err: &EngineError,
    attempt: u32,
    max_retries: i32,
    retry_after: Option<u64>,
    backoff: &Backoff,
) -> RetryDecision {
    // `Retry-After` carried on the error takes precedence over the argument.
    let retry_after = err.retry_after.or(retry_after);
    if max_retries <= 0 || attempt as i32 >= max_retries {
        return RetryDecision::GiveUp;
    }
    let retryable = matches!(
        err.kind,
        ErrorKind::Network
            | ErrorKind::Timeout
            | ErrorKind::Dns
            | ErrorKind::Tls
            | ErrorKind::Protocol
    ) || (err.kind == ErrorKind::Http
        && matches!(
            err.code.as_str(),
            "http_error"
        )
        && http_status_retryable(&err.detail));

    match err.kind {
        ErrorKind::Cancelled => RetryDecision::GiveUp,
        ErrorKind::Authentication | ErrorKind::Validation => RetryDecision::GiveUp,
        // Redirect loops / policy rejections never fix themselves.
        ErrorKind::Protocol if err.code == "redirect_error" => RetryDecision::GiveUp,
        ErrorKind::Disk => RetryDecision::Pause,
        ErrorKind::RemoteChanged => RetryDecision::GiveUp,
        ErrorKind::Offline => RetryDecision::WaitForNetwork,
        ErrorKind::Permission => RetryDecision::Pause,
        ErrorKind::Http => {
            if let Some(detail) = &err.detail {
                if let Some(status) = parse_http_status(detail) {
                    // 429 has explicit Retry-After; 5xx retryable.
                    if status == 429 {
                        let delay = retry_after
                            .map(Duration::from_secs)
                            .unwrap_or_else(|| backoff.delay_for(attempt));
                        return RetryDecision::Retry { delay };
                    }
                    if status >= 500 && status <= 599 {
                        return RetryDecision::Retry {
                            delay: backoff.delay_for(attempt),
                        };
                    }
                }
            }
            RetryDecision::GiveUp
        }
        _ if retryable => RetryDecision::Retry {
            delay: backoff.delay_for(attempt),
        },
        _ => RetryDecision::GiveUp,
    }
}

fn http_status_retryable(detail: &Option<String>) -> bool {
    detail
        .as_deref()
        .and_then(parse_http_status)
        .map(|s| s == 429 || (500..=599).contains(&s))
        .unwrap_or(false)
}

fn parse_http_status(detail: &str) -> Option<u16> {
    // detail is "HTTP 503" etc.
    detail
        .trim()
        .strip_prefix("HTTP ")
        .and_then(|s| s.parse::<u16>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn err(kind: ErrorKind, code: &str, detail: Option<&str>) -> EngineError {
        EngineError::new(kind, code, "msg").with_detail(detail.unwrap_or(""))
    }

    #[test]
    fn backoff_grows() {
        let b = Backoff::default();
        let d0 = b.delay_for(0);
        let d1 = b.delay_for(1);
        let d2 = b.delay_for(2);
        assert!(d0 < d1 && d1 < d2, "{d0:?} {d1:?} {d2:?}");
        // jitter keeps values in a sane range
        assert!(d0.as_secs() >= 1 && d0.as_secs() <= 4, "d0 {d0:?}");
    }

    #[test]
    fn retry_decisions() {
        let b = Backoff::default();
        // 404 → give up
        let e = err(ErrorKind::Http, "http_error", Some("HTTP 404"));
        assert_eq!(decide(&e, 0, 5, None, &b), RetryDecision::GiveUp);
        // 503 → retry
        let e = err(ErrorKind::Http, "http_error", Some("HTTP 503"));
        assert!(matches!(
            decide(&e, 0, 5, None, &b),
            RetryDecision::Retry { .. }
        ));
        // 429 → retry honoring Retry-After
        let e = err(ErrorKind::Http, "http_error", Some("HTTP 429"));
        assert_eq!(
            decide(&e, 0, 5, Some(60), &b),
            RetryDecision::Retry {
                delay: Duration::from_secs(60)
            }
        );
        // timeout → retry
        assert!(matches!(
            decide(&err(ErrorKind::Timeout, "t", None), 0, 5, None, &b),
            RetryDecision::Retry { .. }
        ));
        // auth → give up
        assert_eq!(
            decide(&err(ErrorKind::Authentication, "a", None), 0, 5, None, &b),
            RetryDecision::GiveUp
        );
        // disk → pause
        assert_eq!(
            decide(&err(ErrorKind::Disk, "d", None), 0, 5, None, &b),
            RetryDecision::Pause
        );
        // offline → wait for network
        assert_eq!(
            decide(&err(ErrorKind::Offline, "o", None), 0, 5, None, &b),
            RetryDecision::WaitForNetwork
        );
        // exhausted retries → give up
        assert_eq!(
            decide(&err(ErrorKind::Timeout, "t", None), 5, 5, None, &b),
            RetryDecision::GiveUp
        );
    }
}
