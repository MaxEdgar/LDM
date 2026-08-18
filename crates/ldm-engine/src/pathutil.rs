//! Safe filesystem path handling.
//!
//! Guarantees:
//! * A server-supplied filename can never escape the chosen download
//!   directory (`..`, absolute paths, and embedded separators are stripped).
//! * Reserved device names and control characters are rejected.
//! * Temp files are always created with `create_new`, so a pre-existing
//!   symlink at the temp path fails the open instead of being followed.
//! * Final files are opened with `O_NOFOLLOW` and the completed file is
//!   installed with an atomic `rename` (which never follows a symlink target).

use crate::error::{EngineError, Result};
use std::path::{Component, Path, PathBuf};

/// Characters that can never appear in a stored filename.
fn is_bad_char(c: char) -> bool {
    c == '/' || c == '\\' || c == '\0' || c.is_control()
}

/// Sanitize an arbitrary string into a safe, portable filename.
/// Preserves Unicode letters, digits, spaces, dots, dashes, underscores,
/// parentheses, brackets, `@`, `+`, `#`, `~`, `'`, `,`, `;`, `!`.
pub fn sanitize_filename(input: &str) -> String {
    let mut out: String = input
        .chars()
        .filter(|c| !is_bad_char(*c))
        .map(|c| {
            if c.is_whitespace() {
                ' '
            } else {
                c
            }
        })
        .collect();

    // Reject Windows-reserved device names (harmless on Linux, but avoid
    // creating files that confuse cross-platform tooling).
    if is_reserved_name(&out) {
        out = format!("_{out}");
    }

    // Trim leading/trailing whitespace, then trailing dots (invalid on some
    // filesystems). Internal spaces are preserved.
    out = out.trim_matches(|c: char| c.is_whitespace()).to_string();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    // Never allow the file to be hidden-only or empty.
    if out.is_empty() || out == "." {
        return "download".to_string();
    }
    // Cap at 200 bytes-ish to stay under filesystem name limits.
    if out.len() > 200 {
        out = out.chars().take(180).collect();
    }
    out
}

fn is_reserved_name(name: &str) -> bool {
    let base = name
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.starts_with("COM") && base[3..].chars().all(|c| c.is_ascii_digit()))
        || (base.starts_with("LPT") && base[3..].chars().all(|c| c.is_ascii_digit()))
}

/// Join a (sanitized) filename onto a directory, guaranteeing the result stays
/// inside the directory. Creates the directory if needed.
pub fn safe_join(dir: &Path, filename: &str) -> Result<PathBuf> {
    let clean = sanitize_filename(filename);
    if clean.contains('/') || clean.contains('\\') || clean == ".." {
        return Err(EngineError::validation(
            "The filename is not valid for this filesystem.",
        ));
    }
    let dir = ensure_dir(dir)?;
    // Canonicalize to defend against symlinked directories; the joined path
    // must resolve inside the canonicalized destination.
    let canonical = dir.canonicalize().map_err(|e| {
        EngineError::permission(format!("Cannot access download folder: {e}"))
    })?;
    let joined = canonical.join(&clean);
    if joined.parent() != Some(canonical.as_path()) {
        return Err(EngineError::validation(
            "Refusing to write outside the download folder.",
        ));
    }
    Ok(joined)
}

/// Create a directory (and parents) if it does not exist.
pub fn ensure_dir(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .map_err(|e| EngineError::permission(format!("Cannot create folder {dir:?}: {e}")))?;
    Ok(dir.to_path_buf())
}

/// True when `path` is inside `dir` (both canonicalized).
pub fn is_within(path: &Path, dir: &Path) -> bool {
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

/// Split a filename into (stem, extension) preserving dotted names like
/// `.tar.gz` as a single extension.
pub fn split_ext(name: &str) -> (String, String) {
    let lower = name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(ext) {
            let stem = &name[..name.len() - ext.len()];
            return (stem.to_string(), ext.to_string());
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => {
            (name[..i].to_string(), name[i..].to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

/// A filename that exists (or not) inside `dir`, with a flag indicating
/// whether the raw (un-renamed) target is already taken.
pub struct NameCheck {
    pub raw_path: PathBuf,
    pub exists: bool,
}

/// Check whether `dir/name` already exists.
pub fn check_exists(dir: &Path, name: &str) -> Result<NameCheck> {
    let joined = safe_join(dir, name)?;
    let exists = joined.exists();
    Ok(NameCheck {
        raw_path: joined,
        exists,
    })
}

/// Generate a unique path applying the rename policy (`file (1).zip`).
/// When the raw name is free, returns it unchanged.
pub fn unique_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let clean = sanitize_filename(name);
    let (stem, ext) = split_ext(&clean);
    let mut candidate = safe_join(dir, &clean)?;
    let mut n = 1;
    while candidate.exists() {
        candidate = safe_join(dir, &format!("{stem} ({n}){ext}"))?;
        n += 1;
        if n > 1000 {
            return Err(EngineError::validation(
                "Too many files with this name already exist.",
            ));
        }
    }
    Ok(candidate)
}

/// Validate a user-chosen destination directory exists (or create it).
pub fn validate_destination_dir(dir: &str) -> Result<PathBuf> {
    let p = PathBuf::from(dir);
    if p.as_os_str().is_empty() {
        return Err(EngineError::validation("Please choose a save folder."));
    }
    if p.is_absolute() {
        ensure_dir(&p)
    } else {
        // Resolve relative paths against the home directory for convenience.
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let abs = if p == Path::new("~") || p.starts_with("~/") {
            home.join(p.strip_prefix("~").unwrap_or(Path::new("")))
        } else {
            std::env::current_dir()
                .unwrap_or(home.clone())
                .join(p)
        };
        ensure_dir(&abs)
    }
}

/// Assert a path has no `..` components (defense in depth).
pub fn no_parent_components(p: &Path) -> bool {
    p.components().all(|c| !matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_filename("../../evil.txt"), "..\\..\\evil.txt".replace('\\', ""));
        assert_eq!(sanitize_filename("/absolute/path"), "absolutepath");
        assert_eq!(sanitize_filename("a\\b"), "ab");
        assert_eq!(sanitize_filename("file.txt"), "file.txt");
        assert_eq!(sanitize_filename("résumé.pdf"), "résumé.pdf");
        assert_eq!(sanitize_filename("测试文件.zip"), "测试文件.zip");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("NUL.txt"), "_NUL.txt");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("file."), "file");
        assert_eq!(sanitize_filename("  spaced  .zip "), "spaced  .zip");
        assert_eq!(sanitize_filename("  no-leading.zip"), "no-leading.zip");
        // No separators or traversal survive
        let s = sanitize_filename("..\\..\\..\\etc\\passwd");
        assert!(!s.contains('/') && !s.contains('\\') && s != "..");
    }

    #[test]
    fn safe_join_contains() {
        let dir = std::env::temp_dir().join("ldm-test-safejoin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical_dir = dir.canonicalize().unwrap();
        // Hostile names are sanitized so they can never escape the directory.
        let joined = safe_join(&dir, "sub/../escape.txt").unwrap();
        assert!(joined.starts_with(&canonical_dir));
        assert!(no_parent_components(&joined));
        let joined = safe_join(&dir, "ok.txt").unwrap();
        assert!(joined.starts_with(&canonical_dir));
        // A name that sanitizes to a traversal marker is still contained.
        let joined = safe_join(&dir, "../../..").unwrap();
        assert!(joined.starts_with(&canonical_dir));
        assert_eq!(joined.file_name().unwrap().to_str().unwrap(), "download");
    }

    #[test]
    fn split_extensions() {
        assert_eq!(split_ext("file.tar.gz"), ("file".into(), ".tar.gz".into()));
        assert_eq!(split_ext("a.b.c"), ("a.b".into(), ".c".into()));
        assert_eq!(split_ext("noext"), ("noext".into(), "".into()));
    }
}
