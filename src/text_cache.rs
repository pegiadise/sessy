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

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    let (tmp, offsets) = write_text_cache_tmp(path, chunks)?;
    commit_text_cache(&tmp, path)?;
    Ok(offsets)
}

/// First half of `write_text_cache`: write the chunks to a temp file next to
/// `path`. The caller drops any mmap of the old file (Windows can't replace
/// a mapped file) and then calls `commit_text_cache`.
pub fn write_text_cache_tmp(
    path: &Path,
    chunks: &[&[u8]],
) -> std::io::Result<(PathBuf, Vec<(u64, u32)>)> {
    // Per-process temp name: two sessy instances rebuilding at once must not
    // interleave writes into the same file.
    let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<Vec<(u64, u32)>> {
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
        Ok(out)
    };
    match write() {
        Ok(out) => Ok((tmp, out)),
        Err(e) => {
            // A half-written temp file (disk full) is never picked up again.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Second half of `write_text_cache`: move the temp file into place.
pub fn commit_text_cache(tmp: &Path, path: &Path) -> std::io::Result<()> {
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::remove_file(tmp);
        return Err(e);
    }
    Ok(())
}

/// Append `chunks` to `text.bin`, which must be exactly `expected_len` bytes
/// long now, leaving every existing offset valid. Returns the new chunks'
/// (offset, len) pairs. Fails (and the caller rewrites the file) when the
/// file isn't the expected size before or after, e.g. because another sessy
/// appended at the same time.
pub fn append_text_cache(
    path: &Path,
    expected_len: u64,
    chunks: &[&[u8]],
) -> std::io::Result<Vec<(u64, u32)>> {
    let changed = || std::io::Error::other("text cache changed underneath");
    let mut file = OpenOptions::new().append(true).open(path)?;
    if file.metadata()?.len() != expected_len {
        return Err(changed());
    }
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(chunks.len());
    let mut offset = expected_len;
    let mut buf = Vec::with_capacity(total);
    for chunk in chunks {
        buf.extend_from_slice(chunk);
        out.push((offset, chunk.len() as u32));
        offset += chunk.len() as u64;
    }
    if !buf.is_empty() {
        // One write, so a concurrent appender can't land in the middle.
        file.write_all(&buf)?;
        file.sync_data()?;
    }
    if file.metadata()?.len() != offset {
        return Err(changed());
    }
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
    fn test_append_keeps_old_offsets_and_checks_the_size() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("text.bin");
        write_text_cache(&path, &[b"hello".as_slice()]).unwrap();
        let pairs = append_text_cache(&path, 5, &[b"abc".as_slice(), b"de".as_slice()]).unwrap();
        assert_eq!(pairs, vec![(5, 3), (8, 2)]);
        let cache = TextCache::open(&path);
        assert_eq!(cache.slice(0, 5), b"hello");
        assert_eq!(cache.slice(8, 2), b"de");
        // The index expected a different file: refuse rather than misplace.
        assert!(append_text_cache(&path, 4, &[b"x".as_slice()]).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10);
        // Nothing new: fine, nothing written.
        assert_eq!(append_text_cache(&path, 10, &[]).unwrap(), vec![]);
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
