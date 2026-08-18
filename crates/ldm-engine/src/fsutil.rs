//! Filesystem helpers: free space, atomic rename, durable flush.

use crate::error::{EngineError, Result};
use std::path::Path;

/// Free bytes on the filesystem containing `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    let dir = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf());
    let c_path = std::ffi::CString::new(dir.to_string_lossy().as_bytes())
        .map_err(|_| EngineError::validation("Invalid path."))?;
    // SAFETY: c_path is a valid NUL-terminated path string.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: stat is a valid pointer to a statvfs struct.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(EngineError::unknown(format!(
            "Cannot stat filesystem for {}: {}",
            dir.display(),
            std::io::Error::last_os_error()
        )));
    }
    // f_bavail * f_frsize avoids confusion on filesystems where f_bsize > f_frsize.
    let free = stat.f_bavail as u64 * stat.f_frsize as u64;
    Ok(free)
}

/// Returns a human-readable summary of available space.
pub fn free_space_text(path: &Path) -> String {
    free_space(path)
        .map(|b| crate::model::fmt::bytes(b as i64))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Check that `path`'s filesystem has at least `needed` free bytes.
/// `needed == 0` (unknown size) skips the hard check but still reports.
pub fn ensure_space(path: &Path, needed: i64) -> Result<()> {
    if needed <= 0 {
        return Ok(());
    }
    let free = free_space(path)?;
    if (free as i64) < needed {
        return Err(EngineError::disk(format!(
            "Not enough disk space: {:.1} GB required, {:.1} GB available.",
            needed as f64 / 1e9,
            free as f64 / 1e9
        )));
    }
    Ok(())
}

/// Atomically move `from` to `to` (same filesystem).
pub fn atomic_rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(|e| {
        EngineError::unknown(format!(
            "Cannot move {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })
}

/// fsync a file handle (durability before rename/complete).
pub fn sync_file(file: &std::fs::File) -> Result<()> {
    file.sync_all().map_err(|e| EngineError::disk(e.to_string()))
}

/// Open a file with O_NOFOLLOW for the final component (defense in depth
/// against symlink swaps on the partial file).
pub fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_works() {
        let free = free_space(&std::env::temp_dir()).expect("statvfs works");
        assert!(free > 0);
    }
}
