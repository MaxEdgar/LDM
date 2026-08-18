//! Segment workers: each worker downloads one byte-range of a file and
//! `pwrite`s it at its offset in the shared temporary file.
//!
//! Correctness properties:
//! * Writes use `pwrite64` at fixed offsets — no shared seek state, so any
//!   number of workers can write concurrently without interleaving.
//! * Every range response is validated (`Content-Range` start must match the
//!   requested offset) before the first byte is written; over-sent bytes are
//!   trimmed and never written (spec §85, §147, §148).
//! * Rate limiting happens before each chunk: per-download bucket, then the
//!   global bucket (spec §15, §125).
//! * A paused/cancelled download stops at chunk boundaries; an aborted worker
//!   leaves the file consistent because each `pwrite` is atomic.

use crate::error::EngineError;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// Outcome of a segment worker run.
#[derive(Debug)]
pub enum SegOutcome {
    /// The whole range was written.
    Done,
    /// Stopped because the task is pausing (progress preserved).
    Paused,
    /// Stopped because the task was cancelled.
    Cancelled,
    /// Failed with an error (progress preserved for resumable ranges).
    Failed(EngineError),
}

/// Per-segment shared state (mirrors the `segments` table row).
pub struct SegShared {
    pub id: i64,
    pub download_id: i64,
    /// Inclusive start byte.
    pub start: i64,
    /// Inclusive end byte; `None` = until EOF (unknown total size).
    pub end: Option<i64>,
    pub downloaded: AtomicI64,
    pub status: Mutex<SegStatus>,
    pub attempts: AtomicI64,
    pub last_error: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegStatus {
    Pending,
    Active,
    Completed,
    Failed,
}

impl SegShared {
    pub fn new(id: i64, download_id: i64, start: i64, end: Option<i64>, downloaded: i64) -> Self {
        Self {
            id,
            download_id,
            start,
            end,
            downloaded: AtomicI64::new(downloaded),
            status: Mutex::new(SegStatus::Pending),
            attempts: AtomicI64::new(0),
            last_error: Mutex::new(None),
        }
    }

    pub fn remaining(&self) -> i64 {
        match self.end {
            Some(e) => (e - self.start + 1).saturating_sub(self.downloaded.load(Ordering::SeqCst)),
            None => i64::MAX,
        }
    }
}

/// Shared per-download state used by workers and the task loop.
pub struct TaskShared {
    pub download_id: i64,
    pub url: crate::Url,
    pub opts: crate::protocol::RequestOptions,
    pub protocol: Arc<dyn crate::protocol::DownloadProtocol>,
    pub pause: std::sync::atomic::AtomicBool,
    pub cancel: std::sync::atomic::AtomicBool,
    /// Whether the server supports byte ranges (from the probe).
    pub ranges_supported: std::sync::atomic::AtomicBool,
    pub downloaded: AtomicI64,
    /// Total size; -1 = unknown. Updated when discovered.
    pub total: AtomicI64,
    /// The temporary file all segments write into.
    pub file: tokio::sync::Mutex<tokio::fs::File>,
    /// Per-download rate bucket (0 = unlimited).
    pub per_download_rate: Arc<crate::rate::TokenBucket>,
    /// Global rate bucket (0 = unlimited).
    pub global_rate: Arc<crate::rate::TokenBucket>,
    /// Rolling speed meter (fed by all workers).
    pub speed: Mutex<crate::speed::SpeedMeter>,
}

/// Write `buf` at `offset` in the file, retrying short writes.
/// Uses `pwrite64` so concurrent workers never interfere with each other.
pub fn pwrite_all(file: &tokio::fs::File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let mut written: usize = 0;
    while written < buf.len() {
        // SAFETY: fd is valid; buf is a valid slice; offsets are in-range u64.
        let n = unsafe {
            libc::pwrite64(
                fd,
                buf[written..].as_ptr() as *const libc::c_void,
                buf.len() - written,
                (offset + written as u64) as libc::off64_t,
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write",
            ));
        }
        written += n as usize;
    }
    Ok(())
}

/// Download one segment until done, paused, cancelled, or failed.
pub async fn run_segment(shared: Arc<TaskShared>, seg: Arc<SegShared>) -> SegOutcome {
    if shared.cancel.load(Ordering::SeqCst) {
        return SegOutcome::Cancelled;
    }
    if shared.pause.load(Ordering::SeqCst) {
        return SegOutcome::Paused;
    }

    let initial = seg.downloaded.load(Ordering::SeqCst);
    let mut offset = seg.start + initial;
    let unknown_total = shared.total.load(Ordering::SeqCst) < 0;

    // For unknown total sizes, the first open is a plain GET; retries resume
    // from the current offset with a Range header. When the server does not
    // support ranges at all, never send a Range header (spec §9, §84).
    let ranges_ok = shared.ranges_supported.load(Ordering::SeqCst);
    let range: Option<(u64, Option<u64>)> = if !ranges_ok {
        None
    } else if unknown_total {
        if initial == 0 {
            None
        } else {
            Some((offset.max(0) as u64, None))
        }
    } else {
        Some((offset.max(0) as u64, seg.end.map(|e| e.max(0) as u64)))
    };

    let mut stream = match shared
        .protocol
        .open_range(&shared.url, range, &shared.opts)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            seg.attempts.fetch_add(1, Ordering::SeqCst);
            *seg.last_error.lock().unwrap() = Some(e.message.clone());
            return SegOutcome::Failed(e);
        }
    };

    // Adopt total size when the server reveals it during the open.
    if let Some(t) = stream.total_size {
        let known = shared.total.load(Ordering::SeqCst);
        if known >= 0 && t != known {
            return SegOutcome::Failed(EngineError::remote_changed().with_detail(format!(
                "expected {known} bytes but server now reports {t}"
            )));
        }
        if known < 0 {
            shared.total.store(t, Ordering::SeqCst);
        }
    } else if let Some(cl) = stream.content_length {
        if unknown_total {
            shared.total.store(cl.max(0) as i64, Ordering::SeqCst);
        }
    }

    loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return SegOutcome::Cancelled;
        }
        if shared.pause.load(Ordering::SeqCst) {
            return SegOutcome::Paused;
        }

        match stream.body.next().await {
            Some(Ok(bytes)) => {
                if bytes.is_empty() {
                    continue;
                }
                // Trim any bytes the server sends beyond the requested range
                // (spec §148: "returns too many bytes" must not corrupt).
                let mut n = bytes.len() as i64;
                if let Some(end) = seg.end {
                    let max = (end - offset + 1).max(0);
                    if n > max {
                        n = max;
                    }
                    if n <= 0 {
                        // Server sent past the requested end; the range is done.
                        return SegOutcome::Done;
                    }
                }
                let chunk = &bytes[..n as usize];

                // Rate limiting: per-download bucket first, then global.
                shared
                    .per_download_rate
                    .acquire(chunk.len() as u64)
                    .await;
                shared.global_rate.acquire(chunk.len() as u64).await;

                // Re-check pause/cancel after potentially waiting on the limiter.
                if shared.cancel.load(Ordering::SeqCst) {
                    return SegOutcome::Cancelled;
                }
                if shared.pause.load(Ordering::SeqCst) {
                    return SegOutcome::Paused;
                }

                let write_res = {
                    let f = shared.file.lock().await;
                    pwrite_all(&f, chunk, offset.max(0) as u64)
                };
                if let Err(e) = write_res {
                    let err = EngineError::from(e);
                    *seg.last_error.lock().unwrap() = Some(err.message.clone());
                    return SegOutcome::Failed(err);
                }
                offset += n;
                seg.downloaded.fetch_add(n, Ordering::SeqCst);
                shared.downloaded.fetch_add(n, Ordering::SeqCst);
                shared.speed.lock().unwrap().record(n as u64);
            }
            Some(Err(e)) => {
                let err = EngineError::from_reqwest(&e);
                seg.attempts.fetch_add(1, Ordering::SeqCst);
                *seg.last_error.lock().unwrap() = Some(err.message.clone());
                return SegOutcome::Failed(err);
            }
            None => {
                // EOF.
                if let Some(end) = seg.end {
                    if offset <= end {
                        let err = EngineError::network(
                            "The server closed the connection before the segment finished.",
                        );
                        seg.attempts.fetch_add(1, Ordering::SeqCst);
                        *seg.last_error.lock().unwrap() = Some(err.message.clone());
                        return SegOutcome::Failed(err);
                    }
                }
                if unknown_total {
                    shared.total.store(offset, Ordering::SeqCst);
                }
                return SegOutcome::Done;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pwrite_offsets() {
        let dir = std::env::temp_dir().join("ldm-pwrite-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seg.bin");
        std::fs::remove_file(&path).ok();
        let file = tokio::fs::File::create(&path).await.unwrap();
        // Concurrent writes at distinct offsets must not interleave.
        pwrite_all(&file, b"AAAA", 0).unwrap();
        pwrite_all(&file, b"BBBB", 4).unwrap();
        pwrite_all(&file, b"CC", 8).unwrap();
        drop(file);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data, b"AAAABBBBCC");
        std::fs::remove_dir_all(&dir).ok();
    }
}
