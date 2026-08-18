//! Token-bucket bandwidth limiter.
//!
//! Supports global, per-download, and scheduled limits simultaneously:
//! a download acquires tokens from its own bucket first, then from the global
//! bucket. Limits can be changed at runtime without restarting downloads.
//!
//! The bucket refills continuously (a background refill task every 50ms) and
//! waiters are woken with `tokio::sync::Notify` — no busy polling.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

const REFILL_INTERVAL: Duration = Duration::from_millis(50);

/// A token bucket measuring bytes. `rate` is bytes/second; 0 = unlimited.
pub struct TokenBucket {
    rate: AtomicU64,
    /// Current token count in bytes (stored as u64 of fractional bytes*256 to
    /// allow sub-byte refills without floats).
    tokens_fp: AtomicU64,
    /// Fractional accumulator for refill.
    refill_acc: AtomicU64,
    notify: Notify,
    unlimited: AtomicBool,
}

const FP: u64 = 256; // fixed-point scale

impl TokenBucket {
    pub fn new(rate_bytes_per_sec: u64) -> Arc<Self> {
        let bucket = Arc::new(Self {
            rate: AtomicU64::new(rate_bytes_per_sec),
            tokens_fp: AtomicU64::new(0),
            refill_acc: AtomicU64::new(0),
            notify: Notify::new(),
            unlimited: AtomicBool::new(rate_bytes_per_sec == 0),
        });
        Self::spawn_refiller(bucket.clone());
        bucket
    }

    fn spawn_refiller(bucket: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REFILL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                bucket.refill();
            }
        });
    }

    fn refill(&self) {
        if self.unlimited.load(Ordering::Relaxed) {
            return;
        }
        let rate = self.rate.load(Ordering::Relaxed);
        let add_fp = (rate as u128 * REFILL_INTERVAL.as_millis() as u128 * FP as u128 / 1000) as u64;
        // Accumulate remainder
        let acc = self.refill_acc.fetch_add(add_fp, Ordering::Relaxed);
        let whole = acc / FP;
        if whole > 0 {
            self.refill_acc.fetch_sub(whole * FP, Ordering::Relaxed);
            let cap = rate.saturating_mul(2); // burst up to 2s worth
            let cur = self.tokens_fp.load(Ordering::Relaxed);
            let next = cur.saturating_add(whole.saturating_mul(FP)).min(cap.saturating_mul(FP));
            self.tokens_fp.store(next, Ordering::Relaxed);
        }
        if self.tokens_fp.load(Ordering::Relaxed) > 0 {
            self.notify.notify_waiters();
        }
    }

    /// Set a new rate in bytes/sec (0 = unlimited). Applied live.
    pub fn set_rate(&self, rate_bytes_per_sec: u64) {
        self.rate.store(rate_bytes_per_sec, Ordering::Relaxed);
        self.unlimited
            .store(rate_bytes_per_sec == 0, Ordering::Relaxed);
        if rate_bytes_per_sec == 0 {
            self.notify.notify_waiters();
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Wait until `amount` bytes can be consumed, then consume them.
    pub async fn acquire(&self, amount: u64) {
        if self.unlimited.load(Ordering::Relaxed) || amount == 0 {
            return;
        }
        loop {
            let tokens = self.tokens_fp.load(Ordering::Relaxed);
            let need = amount.saturating_mul(FP);
            if tokens >= need {
                if self
                    .tokens_fp
                    .compare_exchange_weak(tokens, tokens - need, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    return;
                }
                continue;
            }
            // Wait until tokens are available or rate becomes unlimited.
            let notified = self.notify.notified();
            // Re-check before parking to avoid a missed wakeup.
            if self.unlimited.load(Ordering::Relaxed)
                || self.tokens_fp.load(Ordering::Relaxed) >= need
            {
                continue;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_never_blocks() {
        let b = TokenBucket::new(0);
        tokio::time::timeout(Duration::from_millis(500), b.acquire(1 << 30))
            .await
            .expect("should not block");
    }

    #[tokio::test]
    async fn limits_throughput() {
        let b = TokenBucket::new(1024 * 1024); // 1 MB/s
        let start = std::time::Instant::now();
        let mut total: u64 = 0;
        // acquire 2MB over ~2s
        for _ in 0..8 {
            b.acquire(256 * 1024).await;
            total += 256 * 1024;
        }
        let elapsed = start.elapsed().as_millis();
        assert!(total == 2 * 1024 * 1024);
        // 2MB at 1MB/s ≈ 2s; allow generous tolerance for CI timing.
        assert!(elapsed >= 1200, "elapsed {elapsed}ms");
    }

    #[tokio::test]
    async fn rate_change_applies() {
        let b = TokenBucket::new(1 << 20);
        b.acquire(1 << 20).await; // drain initial burst
        b.set_rate(0);
        tokio::time::timeout(Duration::from_millis(300), b.acquire(1 << 30))
            .await
            .expect("unlimited after set_rate(0)");
    }
}
