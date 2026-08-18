//! Optional download verification: size check plus streaming hash
//! (SHA-256, SHA-512; MD5 for compatibility only — never a security boundary).

use crate::error::{EngineError, Result};
use md5::Md5;
use sha2::{Digest, Sha256, Sha512};
use std::io::Read;
use std::path::Path;

const CHUNK: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashType {
    Sha256,
    Sha512,
    Md5,
}

impl HashType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sha256" | "sha-256" => Some(HashType::Sha256),
            "sha512" | "sha-512" => Some(HashType::Sha512),
            "md5" => Some(HashType::Md5),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            HashType::Sha256 => "SHA-256",
            HashType::Sha512 => "SHA-512",
            HashType::Md5 => "MD5",
        }
    }
}

/// Stream a file and return its hex digest. Never loads the file into memory.
pub fn hash_file(path: &Path, kind: HashType) -> Result<String> {
    let file = std::fs::File::open(path)
        .map_err(|e| EngineError::permission(format!("Cannot open file for hashing: {e}")))?;
    let mut reader = std::io::BufReader::with_capacity(CHUNK, file);
    match kind {
        HashType::Sha256 => {
            let mut h = Sha256::new();
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| EngineError::disk(e.to_string()))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
        HashType::Sha512 => {
            let mut h = Sha512::new();
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| EngineError::disk(e.to_string()))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
        HashType::Md5 => {
            let mut h = Md5::new();
            let mut buf = vec![0u8; CHUNK];
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| EngineError::disk(e.to_string()))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            Ok(hex::encode(h.finalize()))
        }
    }
}

/// Normalize a user-supplied checksum: lowercase, strip whitespace and
/// surrounding quotes, remove `sha256:`/`SHA256 (` prefixes.
pub fn normalize_checksum(input: &str) -> String {
    let mut s = input.trim().trim_matches('"').to_ascii_lowercase();
    if let Some((_, rest)) = s.split_once(':') {
        s = rest.trim().to_string();
    }
    s.chars().filter(|c| c.is_ascii_hexdigit()).collect()
}

/// Verify an expected checksum against a file.
pub fn verify_checksum(path: &Path, kind: HashType, expected: &str) -> Result<bool> {
    let actual = hash_file(path, kind)?;
    Ok(actual.eq_ignore_ascii_case(&normalize_checksum(expected)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn checksum_roundtrip() {
        let dir = std::env::temp_dir().join("ldm-verify-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("data.bin");
        let mut f = std::fs::File::create(&p).unwrap();
        for i in 0..3_000_000u32 {
            f.write_all(&(i as u64).to_le_bytes()).unwrap();
        }
        drop(f);
        let h = hash_file(&p, HashType::Sha256).unwrap();
        assert_eq!(h.len(), 64);
        assert!(verify_checksum(&p, HashType::Sha256, &h).unwrap());
        assert!(!verify_checksum(&p, HashType::Sha256, &"0".repeat(64)).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_checksum("ABC"), "abc");
        assert_eq!(normalize_checksum("sha256: aabb"), "aabb");
        assert_eq!(normalize_checksum("  \"deadbeef\" "), "deadbeef");
    }
}
