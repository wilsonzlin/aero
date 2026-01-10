use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Deterministically hash a directory by walking it in sorted order and hashing:
/// - the relative path (UTF-8, forward slashes)
/// - file length (u64 LE)
/// - file contents
pub fn sha256_dir(path: &Path) -> Result<String> {
    let mut entries: Vec<(PathBuf, PathBuf)> = WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let abs = entry.path().to_path_buf();
            let rel = entry
                .path()
                .strip_prefix(path)
                .unwrap_or(entry.path())
                .to_path_buf();
            (rel, abs)
        })
        .collect();

    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut hasher = Sha256::new();
    for (rel, abs) in entries {
        let rel_str = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        hasher.update(rel_str.as_bytes());
        hasher.update([0u8]);

        let metadata = std::fs::metadata(&abs).with_context(|| {
            format!("Failed to read metadata for hashing: {}", abs.display())
        })?;
        let len = metadata.len();
        hasher.update(len.to_le_bytes());

        let mut file = File::open(&abs)
            .with_context(|| format!("Failed to open for hashing: {}", abs.display()))?;
        let mut buf = [0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dir_hash_is_deterministic() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a").join("c.txt"), b"c").unwrap();

        let h1 = sha256_dir(dir.path()).unwrap();
        let h2 = sha256_dir(dir.path()).unwrap();
        assert_eq!(h1, h2);
    }
}
