use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn bookmarks_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    std::fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("bookmarks.json")
}

pub fn load_bookmarks() -> HashSet<String> {
    load_from(&bookmarks_path())
}

/// The pins saved at `path`. A file that doesn't parse is moved aside to
/// `bookmarks.json.bad` (not left to be overwritten by the next pin) and
/// reads as none.
fn load_from(path: &Path) -> HashSet<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    match serde_json::from_str(&content) {
        Ok(set) => set,
        Err(_) => {
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            HashSet::new()
        }
    }
}

/// Apply one change to the pins saved at `path`, re-read first so another
/// sessy's changes since are kept, and return them all. Written to a temp
/// file and renamed over, so a crash never leaves a half-written file.
pub fn update(path: &Path, change: impl FnOnce(&mut HashSet<String>)) -> HashSet<String> {
    let mut set = load_from(path);
    change(&mut set);
    if let Ok(json) = serde_json::to_string(&set) {
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, json).is_err() || std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_bookmarks_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        update(&path, |set| {
            set.insert("abc-123".to_string());
        });
        let set = update(&path, |set| {
            set.insert("def-456".to_string());
        });
        assert_eq!(set.len(), 2);
        assert_eq!(load_from(&path), set);
    }

    #[test]
    fn test_another_instances_pins_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        update(&path, |set| {
            set.insert("mine".to_string());
        });
        // Another sessy pins something meanwhile.
        fs::write(&path, r#"["mine","theirs"]"#).unwrap();
        let set = update(&path, |set| {
            set.remove("mine");
        });
        assert_eq!(set, HashSet::from(["theirs".to_string()]));
    }

    #[test]
    fn test_a_damaged_file_is_moved_aside_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        fs::write(&path, r#"["a","b""#).unwrap();
        assert!(load_from(&path).is_empty());
        assert_eq!(fs::read_to_string(path.with_extension("json.bad")).unwrap(), r#"["a","b""#);
    }
}
