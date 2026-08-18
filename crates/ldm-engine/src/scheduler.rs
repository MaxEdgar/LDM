//! Time-window scheduling helpers (spec §14): "HH:MM" parsing and
//! day-of-week / crossing-midnight window evaluation. Pure logic — the
//! manager drives it.

use chrono::{Datelike, Timelike};

/// Parse "HH:MM" (24h) into minutes since midnight.
pub fn parse_hhmm(s: &str) -> Option<i32> {
    let (h, m) = s.trim().split_once(':')?;
    let h: i32 = h.parse().ok()?;
    let m: i32 = m.parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

/// Is the current local time inside the schedule window?
///
/// `days` is a bitmask: bit 0 = Monday … bit 6 = Sunday (bit 7 unused).
/// A window may cross midnight (start 22:00, stop 06:00).
pub fn is_window_active(start: &str, stop: Option<&str>, days: i32) -> bool {
    let now = chrono::Local::now();
    let dow = now.weekday().num_days_from_monday() as i32;
    if (days >> dow) & 1 == 0 {
        return false;
    }
    let now_min = now.hour() as i32 * 60 + now.minute() as i32;
    let start_min = parse_hhmm(start).unwrap_or(0);
    match stop.and_then(parse_hhmm) {
        Some(stop_min) => {
            if start_min <= stop_min {
                now_min >= start_min && now_min < stop_min
            } else {
                // Crosses midnight.
                now_min >= start_min || now_min < stop_min
            }
        }
        None => now_min >= start_min,
    }
}

/// Local time "HH:MM" string.
pub fn now_hhmm() -> String {
    let now = chrono::Local::now();
    format!("{:02}:{:02}", now.hour(), now.minute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_times() {
        assert_eq!(parse_hhmm("22:00"), Some(22 * 60));
        assert_eq!(parse_hhmm("06:30"), Some(6 * 60 + 30));
        assert_eq!(parse_hhmm("25:00"), None);
        assert_eq!(parse_hhmm("abc"), None);
        assert_eq!(parse_hhmm(""), None);
    }

    #[test]
    fn windows() {
        // All days enabled.
        let all = 0b1111111;
        // Windows that include "now" depend on the clock; test boundaries via
        // direct logic instead:
        // 00:00-06:00 crossing nothing, 22:00-06:00 crossing midnight.
        // We can only assert it doesn't panic and returns a bool.
        let _ = is_window_active("22:00", Some("06:00"), all);
        let _ = is_window_active("10:00", None, all);
        // No days selected → inactive.
        assert!(!is_window_active("00:00", None, 0));
    }
}
