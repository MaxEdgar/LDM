//! Speed measurement with a rolling window (spec §80): instantaneous speed,
//! short-term average (3s), overall average, and peak.

use std::time::{Duration, Instant};

/// Number of samples retained (1 sample per ~200ms → ~3s window).
const WINDOW: usize = 16;
const SAMPLE_EVERY: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    bytes: u64,
}

pub struct SpeedMeter {
    samples: Vec<Sample>,
    total_bytes: u64,
    first_at: Instant,
    peak: u64,
}

impl Default for SpeedMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeedMeter {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            samples: vec![Sample { at: now, bytes: 0 }],
            total_bytes: 0,
            first_at: now,
            peak: 0,
        }
    }

    /// Record `bytes` downloaded since the last call (throttled internally:
    /// a sample is pushed at most every 200ms).
    pub fn record(&mut self, bytes: u64) {
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        let now = Instant::now();
        let last = self.samples.last().copied().unwrap();
        if now.duration_since(last.at) >= SAMPLE_EVERY {
            self.samples.push(Sample { at: now, bytes });
            if self.samples.len() > WINDOW {
                self.samples.remove(0);
            }
        }
    }

    /// Bytes/sec over the most recent sample interval.
    pub fn instant(&self) -> u64 {
        if self.samples.len() < 2 {
            return 0;
        }
        let (a, b) = (self.samples[0], *self.samples.last().unwrap());
        let span = b.at.duration_since(a.at).as_secs_f64();
        if span <= 0.0 {
            return 0;
        }
        let bytes: u64 = self.samples[1..].iter().map(|s| s.bytes).sum();
        (bytes as f64 / span) as u64
    }

    /// Bytes/sec over the whole window (3s).
    pub fn short_avg(&self) -> u64 {
        if self.samples.len() < 2 {
            return 0;
        }
        let (a, b) = (self.samples[0], *self.samples.last().unwrap());
        let span = b.at.duration_since(a.at).as_secs_f64();
        if span <= 0.0 {
            return 0;
        }
        let bytes: u64 = self.samples[1..].iter().map(|s| s.bytes).sum();
        (bytes as f64 / span) as u64
    }

    /// Bytes/sec since the meter was created.
    pub fn overall(&self) -> u64 {
        let span = self.first_at.elapsed().as_secs_f64();
        if span <= 0.0 {
            return 0;
        }
        (self.total_bytes as f64 / span) as u64
    }

    pub fn total(&self) -> u64 {
        self.total_bytes
    }

    pub fn peak(&self) -> u64 {
        self.peak
    }

    /// Called when the segment workers advance; updates peak.
    pub fn observe(&mut self, instantaneous: u64) {
        if instantaneous > self.peak {
            self.peak = instantaneous;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn computes_rates() {
        let mut m = SpeedMeter::new();
        // Simulate 1 MB/s for 1 second
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1100) {
            m.record(256 * 1024);
            tokio::time::sleep(Duration::from_millis(256)).await;
        }
        let s = m.short_avg();
        assert!(s > 500 * 1024 && s < 2 * 1024 * 1024, "short avg {s}");
        assert!(m.overall() > 500 * 1024, "overall {}", m.overall());
    }
}
