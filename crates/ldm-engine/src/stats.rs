//! Aggregate statistics shown on the UI (spec §48).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total_downloaded: i64,
    pub counts: Vec<(String, i64)>,
}

impl Stats {
    pub fn count(&self, status: &str) -> i64 {
        self.counts
            .iter()
            .find(|(s, _)| s == status)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    }
}
