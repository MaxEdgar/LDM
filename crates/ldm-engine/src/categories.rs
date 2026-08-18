//! Download categories: built-in extension-based classification plus
//! user-defined categories with default directories.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub dir_path: Option<String>,
    pub extensions: Vec<String>,
    pub is_builtin: bool,
}

pub const BUILTIN_CATEGORIES: &[(&str, &[&str])] = &[
    ("General", &[]),
    (
        "Documents",
        &[".pdf", ".doc", ".docx", ".odt", ".txt", ".rtf", ".md", ".epub", ".xls", ".xlsx", ".ppt", ".pptx", ".csv"],
    ),
    (
        "Compressed",
        &[
            ".zip", ".rar", ".7z", ".tar", ".gz", ".tgz", ".xz", ".bz2", ".zst", ".iso",
            ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".gz", ".tbz2", ".txz",
        ],
    ),
    (
        "Programs",
        &[".exe", ".msi", ".deb", ".rpm", ".apk", ".dmg", ".appimage", ".flatpak", ".run", ".sh", ".pkg"],
    ),
    (
        "Videos",
        &[".mp4", ".mkv", ".webm", ".mov", ".avi", ".flv", ".wmv", ".m4v", ".mpg", ".mpeg", ".ts", ".3gp"],
    ),
    (
        "Music",
        &[".mp3", ".flac", ".wav", ".aac", ".ogg", ".opus", ".m4a", ".wma", ".mid"],
    ),
    (
        "Images",
        &[".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".bmp", ".tiff", ".ico", ".heic", ".avif"],
    ),
    ("Other", &[]),
];

/// Extension → category name.
pub fn category_for_extension(ext: &str) -> String {
    let ext = ext.to_ascii_lowercase();
    for (name, exts) in BUILTIN_CATEGORIES {
        if exts.contains(&ext.as_str()) {
            return name.to_string();
        }
    }
    "General".to_string()
}

/// Get the extension (with dot) of a filename, treating multi-part extensions
/// like `.tar.gz` as one.
pub fn extension_of(filename: &str) -> String {
    let lower = filename.to_ascii_lowercase();
    for e in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(e) {
            return e.to_string();
        }
    }
    match lower.rfind('.') {
        Some(i) if i > 0 => lower[i..].to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        assert_eq!(category_for_extension(".mp4"), "Videos");
        assert_eq!(category_for_extension(".tar.gz"), "Compressed");
        assert_eq!(category_for_extension(".pdf"), "Documents");
        assert_eq!(category_for_extension(".deb"), "Programs");
        assert_eq!(category_for_extension(".png"), "Images");
        assert_eq!(category_for_extension(".xyzabc"), "General");
        // A bare .bin is a generic binary blob, not a program.
        assert_eq!(category_for_extension(".bin"), "General");
        assert_eq!(extension_of("file.tar.gz"), ".tar.gz");
        assert_eq!(extension_of("a.MP4"), ".mp4");
    }
}
