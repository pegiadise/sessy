use crate::session::SessionMeta;
use crate::text_cache::{text_cache_path, write_text_cache, TextCache};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const INDEX_VERSION: u32 = 3;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: Vec<SessionMeta>,
}

pub fn index_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("index.bin")
}

pub fn claude_projects_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".claude")
        .join("projects")
}

pub fn serialize_index(index: &SessionIndex) -> Vec<u8> {
    bincode::serialize(index).unwrap_or_default()
}

pub fn deserialize_index(bytes: &[u8]) -> Option<SessionIndex> {
    let index: SessionIndex = bincode::deserialize(bytes).ok()?;
    if index.version != INDEX_VERSION {
        return None;
    }
    Some(index)
}

pub fn scan_session_file(path: &Path) -> Option<(SessionMeta, Vec<u8>)> {
    let metadata = fs::metadata(path).ok()?;
    let file_size = metadata.len();
    let file_mtime = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let id = path.file_stem()?.to_str()?.to_string();

    let scan = crate::parser::scan_session(path)?;

    let home_dir = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let project = crate::session::extract_project_name(&scan.head.cwd, &home_dir);

    let (last_message, duration_secs, rename) = if let Some(tail) = &scan.tail {
        let duration = compute_duration(&scan.head.first_timestamp, &tail.last_timestamp);
        (tail.last_human_message.clone(), duration, tail.rename.clone())
    } else {
        (String::new(), 0, String::new())
    };

    let name = if !rename.is_empty() {
        rename
    } else if !scan.head.slug.is_empty() {
        scan.head.slug.clone()
    } else {
        String::new()
    };

    let branch = if scan.head.branch == "HEAD" {
        String::new()
    } else {
        scan.head.branch.clone()
    };

    let name_lc = name.to_lowercase();
    let title_lc = scan.head.title.to_lowercase();
    let project_lc = project.to_lowercase();
    let branch_lc = branch.to_lowercase();

    let text_bytes = scan.human_text_lc.into_bytes();

    let meta = SessionMeta {
        id,
        project,
        branch,
        name,
        title: scan.head.title,
        last_message,
        duration_secs,
        timestamp: file_mtime,
        file_size,
        file_mtime,
        file_path: path.to_path_buf(),
        cwd: scan.head.cwd,
        message_count: None,
        tickets: scan.tickets,
        text_offset: 0, // filled in finalize step
        text_len: text_bytes.len() as u32,
        name_lc,
        title_lc,
        project_lc,
        branch_lc,
    };
    Some((meta, text_bytes))
}

fn compute_duration(first: &str, last: &str) -> u64 {
    use chrono::DateTime;
    let parse = |s: &str| -> Option<i64> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.timestamp())
    };
    match (parse(first), parse(last)) {
        (Some(a), Some(b)) if b >= a => (b - a) as u64,
        _ => 0,
    }
}

pub fn build_index(cached: Option<SessionIndex>, force_rebuild: bool) -> SessionIndex {
    let projects_dir = claude_projects_dir();
    if !projects_dir.exists() {
        return SessionIndex {
            version: INDEX_VERSION,
            sessions: vec![],
        };
    }

    // Open the previous text.bin so we can reuse bytes for unchanged sessions.
    let prev_text = TextCache::open(&text_cache_path());

    let cache_map: std::collections::HashMap<PathBuf, &SessionMeta> = if force_rebuild {
        std::collections::HashMap::new()
    } else {
        cached
            .as_ref()
            .map(|idx| idx.sessions.iter().map(|s| (s.file_path.clone(), s)).collect())
            .unwrap_or_default()
    };

    let mut file_entries: Vec<PathBuf> = Vec::new();
    if let Ok(project_dirs) = fs::read_dir(&projects_dir) {
        for proj_entry in project_dirs.flatten() {
            if !proj_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let proj_dir = proj_entry.path();
            if let Ok(files) = fs::read_dir(&proj_dir) {
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                        && path.is_file()
                    {
                        file_entries.push(path);
                    }
                }
            }
        }
    }

    // For each file: either reuse cached meta + old text bytes, or rescan.
    let scanned: Vec<(SessionMeta, Vec<u8>)> = file_entries
        .par_iter()
        .filter_map(|path| {
            if let Some(cached_entry) = cache_map.get(path) {
                let meta = match fs::metadata(path) {
                    Ok(m) => m,
                    Err(_) => return scan_session_file(path),
                };
                let current_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let current_size = meta.len();
                if cached_entry.file_mtime == current_mtime
                    && cached_entry.file_size == current_size
                {
                    let bytes = prev_text
                        .slice(cached_entry.text_offset, cached_entry.text_len)
                        .to_vec();
                    let mut meta = (*cached_entry).clone();
                    // offsets will be reassigned in the finalize step
                    meta.text_offset = 0;
                    return Some((meta, bytes));
                }
            }
            scan_session_file(path)
        })
        .collect();

    // Serial finalize: write text.bin and patch offsets onto each SessionMeta.
    let chunks: Vec<&[u8]> = scanned.iter().map(|(_, b)| b.as_slice()).collect();
    let offsets = match write_text_cache(&text_cache_path(), &chunks) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("sessy: failed to write text cache: {}", e);
            // If we can't write text.bin, return empty so the next launch retries.
            return SessionIndex {
                version: INDEX_VERSION,
                sessions: vec![],
            };
        }
    };

    let sessions: Vec<SessionMeta> = scanned
        .into_iter()
        .zip(offsets)
        .map(|((mut meta, _), (offset, len))| {
            meta.text_offset = offset;
            meta.text_len = len;
            meta
        })
        .collect();

    SessionIndex {
        version: INDEX_VERSION,
        sessions,
    }
}

pub fn load_cached_index() -> Option<SessionIndex> {
    let path = index_cache_path();
    let bytes = fs::read(&path).ok()?;
    let index = deserialize_index(&bytes)?;
    let text_cache = TextCache::open(&text_cache_path());
    let max_end: u64 = index
        .sessions
        .iter()
        .map(|s| s.text_offset + s.text_len as u64)
        .max()
        .unwrap_or(0);
    if (text_cache.len() as u64) < max_end {
        return None;
    }
    Some(index)
}

pub fn save_index(index: &SessionIndex) {
    let path = index_cache_path();
    let bytes = serialize_index(index);
    fs::write(&path, bytes).ok();
}

pub fn parse_recent_filter(s: &str) -> Option<u64> {
    let s = s.trim();
    let unit = s.chars().last()?;
    let num_str = &s[..s.len() - unit.len_utf8()];
    let num: u64 = num_str.parse().ok()?;
    match unit {
        'h' => Some(num * 3600),
        'd' => Some(num * 86400),
        'w' => Some(num * 7 * 86400),
        'm' => Some(num * 30 * 86400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn test_scan_session_file_simple() {
        let result = scan_session_file(&fixture_path("simple_session.jsonl"));
        let (meta, text) = result.expect("should produce SessionMeta + text");
        assert_eq!(meta.title, "build a cool thing");
        assert_eq!(meta.last_message, "looks good, ship it");
        assert_eq!(meta.branch, "main");
        assert!(meta.duration_secs > 0);
        assert_eq!(meta.title_lc, "build a cool thing");
        assert_eq!(meta.branch_lc, "main");
        assert!(!text.is_empty());
        assert!(text.iter().all(|b| !(*b as char).is_uppercase()));
    }

    #[test]
    fn test_scan_session_file_empty_returns_none() {
        let result = scan_session_file(&fixture_path("empty_session.jsonl"));
        assert!(result.is_none(), "empty session should be filtered out");
    }

    #[test]
    fn test_index_serialization_roundtrip() {
        let sessions = vec![SessionMeta {
            id: "abc-123".to_string(),
            project: "test".to_string(),
            branch: "main".to_string(),
            name: String::new(),
            title: "hello world".to_string(),
            last_message: "goodbye".to_string(),
            duration_secs: 300,
            timestamp: 1710300000,
            file_size: 1024,
            file_mtime: 1710300000,
            file_path: PathBuf::from("/tmp/test.jsonl"),
            cwd: "/Users/me/code/test".to_string(),
            message_count: None,
            tickets: vec![],
            text_offset: 0,
            text_len: 0,
            name_lc: String::new(),
            title_lc: "hello world".to_string(),
            project_lc: "test".to_string(),
            branch_lc: "main".to_string(),
        }];
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions,
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_some());
        let restored = restored.unwrap();
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].title, "hello world");
        assert_eq!(restored.sessions[0].last_message, "goodbye");
    }

    #[test]
    fn test_index_deserialization_wrong_version_returns_none() {
        let index = SessionIndex {
            version: 999,
            sessions: vec![],
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_none());
    }

    #[test]
    fn test_parse_recent_filter() {
        assert_eq!(parse_recent_filter("7d"), Some(7 * 86400));
        assert_eq!(parse_recent_filter("1h"), Some(3600));
        assert_eq!(parse_recent_filter("2w"), Some(14 * 86400));
        assert_eq!(parse_recent_filter("1m"), Some(30 * 86400));
        assert_eq!(parse_recent_filter("garbage"), None);
    }
}
