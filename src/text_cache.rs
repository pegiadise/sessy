use memmap2::Mmap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn text_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("text.bin")
}

/// Mmap of the text cache. Empty (`len() == 0`) when the cache file is missing
/// or empty; callers must check before slicing.
pub struct TextCache {
    mmap: Option<Mmap>,
}

impl TextCache {
    pub fn open(path: &Path) -> Self {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Self { mmap: None },
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(_) => return Self { mmap: None },
        };
        if metadata.len() == 0 {
            return Self { mmap: None };
        }
        let mmap = unsafe { Mmap::map(&file).ok() };
        Self { mmap }
    }

    pub fn len(&self) -> usize {
        self.mmap.as_ref().map(|m| m.len()).unwrap_or(0)
    }

    pub fn slice(&self, offset: u64, len: u32) -> &[u8] {
        let mmap = match &self.mmap {
            Some(m) => m,
            None => return &[],
        };
        let start = offset as usize;
        let end = start.saturating_add(len as usize);
        if end > mmap.len() {
            return &[];
        }
        &mmap[start..end]
    }
}

/// Writes `text.bin` atomically. Returns the per-session (offset, len) pairs
/// in the order of the input.
pub fn write_text_cache(path: &Path, chunks: &[&[u8]]) -> std::io::Result<Vec<(u64, u32)>> {
    let tmp = path.with_extension("bin.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;

    let mut out = Vec::with_capacity(chunks.len());
    let mut offset: u64 = 0;
    for chunk in chunks {
        file.write_all(chunk)?;
        out.push((offset, chunk.len() as u32));
        offset += chunk.len() as u64;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_text_cache_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("text.bin");
        let a = b"hello".as_slice();
        let b = b"world!".as_slice();
        let pairs = write_text_cache(&path, &[a, b]).unwrap();
        assert_eq!(pairs[0], (0, 5));
        assert_eq!(pairs[1], (5, 6));

        let cache = TextCache::open(&path);
        assert_eq!(cache.slice(0, 5), b"hello");
        assert_eq!(cache.slice(5, 6), b"world!");
        assert_eq!(cache.slice(11, 1), b""); // out of range
    }

    #[test]
    fn test_text_cache_missing_file_safe() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does_not_exist.bin");
        let cache = TextCache::open(&path);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.slice(0, 5), b"");
    }
}
