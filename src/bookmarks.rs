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

/// The pins saved at `path` (none when there's no file). A file that
/// doesn't parse (or isn't text) is moved aside to `bookmarks.json.bad`, not
/// left to be overwritten by the next pin, and reads as none.
fn load_from(path: &Path) -> HashSet<String> {
    read(path).unwrap_or_default()
}

/// `None`: there is a file but it can't be read now (permissions, I/O);
/// it's left alone.
fn read(path: &Path) -> Option<HashSet<String>> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(HashSet::new()),
        Err(_) => return None,
    };
    match serde_json::from_slice(&content) {
        Ok(set) => Some(set),
        Err(_) => {
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Some(HashSet::new())
        }
    }
}

/// The pins saved at `path`, or why they can't be read: for `--purge`,
/// which must not take a damaged or unreadable file for "nothing pinned".
pub fn load_strict(path: &Path) -> Result<HashSet<String>, String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashSet::new()),
        Err(e) => Err(format!("can't read {}: {}", path.display(), e)),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is damaged ({})", path.display(), e)),
    }
}

/// Apply one change to the pins saved at `path`, re-read first so another
/// sessy's changes since are kept, and return them all. Written to a temp
/// file and renamed over, so a crash never leaves a half-written file.
/// When the file can't be read, it isn't written either (that would lose
/// the pins in it): `None`, and the change is only in memory.
pub fn update(path: &Path, change: impl FnOnce(&mut HashSet<String>)) -> Option<HashSet<String>> {
    let mut set = read(path)?;
    change(&mut set);
    if let Ok(json) = serde_json::to_string(&set) {
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, json).is_err() || std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Some(set)
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
        })
        .unwrap();
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
        })
        .unwrap();
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

    #[test]
    fn test_a_file_that_isnt_text_is_moved_aside_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let damaged = b"[\"pin-a\",\"pin-\xff\"]";
        fs::write(&path, damaged).unwrap();
        update(&path, |set| {
            set.insert("new".to_string());
        });
        assert_eq!(fs::read(path.with_extension("json.bad")).unwrap(), damaged);
    }

    #[test]
    fn test_strict_loading_tells_no_pins_from_unreadable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        assert_eq!(load_strict(&path), Ok(HashSet::new()));
        fs::write(&path, "").unwrap();
        assert!(load_strict(&path).is_err(), "an empty file isn't \"no pins\"");
        assert!(path.exists(), "and it's left where it is");
        fs::write(&path, r#"["a"]"#).unwrap();
        assert_eq!(load_strict(&path), Ok(HashSet::from(["a".to_string()])));
    }

    #[cfg(unix)]
    #[test]
    fn test_a_file_that_cant_be_read_isnt_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        fs::write(&path, r#"["a","b"]"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        if fs::read(&path).is_ok() {
            return; // running as root: nothing is unreadable
        }
        assert_eq!(update(&path, |set| { set.insert("c".to_string()); }), None);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"["a","b"]"#);
    }
}
