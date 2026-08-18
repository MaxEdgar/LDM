//! Optional clipboard monitoring (spec §20, §117): polls the clipboard locally
//! (never uploads it), detects a URL, and surfaces it for user confirmation.

/// Local-only clipboard polling. Polling is cheap (only on content change we
/// scan for a URL), and no clipboard history is ever stored.
pub struct ClipboardMonitor {
    clip: arboard::Clipboard,
    last_content: Option<String>,
    last_url: Option<String>,
}

impl ClipboardMonitor {
    pub fn new() -> Result<Self, String> {
        arboard::Clipboard::new()
            .map(|clip| Self {
                clip,
                last_content: None,
                last_url: None,
            })
            .map_err(|e| format!("clipboard unavailable: {e}"))
    }

    /// Poll the clipboard. Returns `(url, filename_hint)` once per distinct
    /// URL; `None` when nothing new was detected.
    pub fn poll(&mut self) -> Option<(String, Option<String>)> {
        let text = self.clip.get_text().ok()?;
        if Some(&text) == self.last_content.as_ref() {
            return None;
        }
        self.last_content = Some(text.clone());
        let found = extract_url(&text);
        if let Some((url, fname)) = &found {
            // Don't re-prompt for the same URL.
            if Some(url) == self.last_url.as_ref() {
                return None;
            }
            self.last_url = Some(url.clone());
            return Some((url.clone(), fname.clone()));
        }
        None
    }
}

/// Find the first http(s) URL in arbitrary text and trim trailing punctuation.
pub fn extract_url(text: &str) -> Option<(String, Option<String>)> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for marker in ["https://", "http://"] {
        let mut idx = 0;
        while let Some(rel) = lower[idx..].find(marker) {
            let start = idx + rel;
            best = match best {
                Some((s, _)) if s <= start => best,
                _ => Some((start, 0)),
            };
            idx = start + marker.len();
        }
    }
    let (start, _) = best?;
    let rest = &text[start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | '}' | '>' | '<' | '`'))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let url = &rest[..end];
    let url = url.trim_end_matches(|c: char| c == '.' || c == ',' || c == ';' || c == ':' || c == '!');
    let parsed = crate::urlutil::validate_url(url).ok()?;
    let fname = crate::urlutil::filename_from_url(&parsed);
    Some((parsed.to_string(), fname))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_urls() {
        let (u, f) = extract_url("Download this: https://example.com/file.zip now!").unwrap();
        assert_eq!(u, "https://example.com/file.zip");
        assert_eq!(f.as_deref(), Some("file.zip"));
        // Trailing punctuation trimmed.
        let (u, _) = extract_url("see https://example.com/a.b?x=1.").unwrap();
        assert_eq!(u, "https://example.com/a.b?x=1");
        // No URL → None.
        assert!(extract_url("nothing here").is_none());
        // Invalid scheme not picked up.
        assert!(extract_url("ftp://example.com/x").is_none());
    }
}
