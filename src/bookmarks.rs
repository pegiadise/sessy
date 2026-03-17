use std::collections::HashSet;
use std::path::PathBuf;

fn bookmarks_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    std::fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("bookmarks.json")
}

pub fn load_bookmarks() -> HashSet<String> {
    let path = bookmarks_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_bookmarks(bookmarks: &HashSet<String>) {
    let path = bookmarks_path();
    if let Ok(json) = serde_json::to_string(bookmarks) {
        std::fs::write(&path, json).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_bookmarks_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let mut set = HashSet::new();
        set.insert("abc-123".to_string());
        set.insert("def-456".to_string());
        let json = serde_json::to_string(&set).unwrap();
        fs::write(&path, &json).unwrap();
        let loaded: HashSet<String> = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded, set);
    }
}
