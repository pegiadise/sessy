//! Deleted sessions are moved here instead of being removed, so a delete can
//! be undone. Each delete gets its own `<unix-secs>-<session-id>/` folder
//! holding the transcript and its companion directory; folders older than
//! `KEEP_DAYS` are pruned at startup.

use std::fs;
use std::path::{Path, PathBuf};

pub const KEEP_DAYS: u64 = 7;

/// `<cache>/sessy/trash`.
pub fn trash_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy")
        .join("trash")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The directory Claude Code keeps next to a transcript (`<id>/`: subagent
/// transcripts, tool results).
fn companion_dir(transcript: &Path) -> PathBuf {
    transcript.with_extension("")
}

/// A new, empty folder under `root` for one delete. Never reuses an
/// existing one: two deletes of the same ID within a second get their own.
fn new_slot(root: &Path, session_id: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let base = format!("{}-{}", now_secs(), session_id);
    for n in 0..100 {
        let slot = if n == 0 {
            root.join(&base)
        } else {
            root.join(format!("{}-{}", base, n))
        };
        match fs::create_dir(&slot) {
            Ok(()) => return Ok(slot),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free trash folder",
    ))
}

/// Move `transcript` (and its companion directory) into a fresh folder under
/// `root`. Returns that folder. Nothing is removed on failure, so the caller
/// can fall back to deleting (e.g. when the trash is on another disk).
pub fn move_to_trash(
    transcript: &Path,
    root: &Path,
    session_id: &str,
) -> std::io::Result<PathBuf> {
    let slot = new_slot(root, session_id)?;
    let name = transcript.file_name().unwrap_or_default();
    if let Err(e) = fs::rename(transcript, slot.join(name)) {
        let _ = fs::remove_dir(&slot);
        return Err(e);
    }
    let companion = companion_dir(transcript);
    if companion.is_dir() {
        if let Some(dir_name) = companion.file_name() {
            // If it won't move, it stays where it is (orphaned but intact)
            // rather than being deleted for good.
            let _ = fs::rename(&companion, slot.join(dir_name));
        }
    }
    Ok(slot)
}

/// Put a trashed transcript (and companion directory) back at `transcript`.
pub fn restore(slot: &Path, transcript: &Path) -> std::io::Result<()> {
    let name = transcript.file_name().unwrap_or_default();
    if transcript.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a session with that ID exists again",
        ));
    }
    if let Some(parent) = transcript.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(slot.join(name), transcript)?;
    let companion = companion_dir(transcript);
    if let Some(dir_name) = companion.file_name() {
        let kept = slot.join(dir_name);
        if kept.is_dir() && !companion.exists() {
            let _ = fs::rename(kept, &companion);
        }
    }
    // Only removes the folder when everything went back; anything left in
    // it waits for the prune.
    let _ = fs::remove_dir(slot);
    Ok(())
}

/// Remove trash folders older than `KEEP_DAYS`.
pub fn prune(root: &Path) {
    prune_older_than(root, now_secs().saturating_sub(KEEP_DAYS * 86400));
}

fn prune_older_than(root: &Path, cutoff: u64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(secs) = name
            .to_str()
            .and_then(|n| n.split('-').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if secs < cutoff {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_restore_round_trip_with_companion_dir() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("projects/-p");
        fs::create_dir_all(project.join("abc/subagents")).unwrap();
        let transcript = project.join("abc.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        fs::write(project.join("abc/subagents/x.jsonl"), "{}\n").unwrap();
        let root = dir.path().join("trash");

        let slot = move_to_trash(&transcript, &root, "abc").unwrap();
        assert!(!transcript.exists());
        assert!(!project.join("abc").exists());
        assert!(slot.join("abc.jsonl").is_file());
        assert!(slot.join("abc/subagents/x.jsonl").is_file());

        restore(&slot, &transcript).unwrap();
        assert_eq!(fs::read_to_string(&transcript).unwrap(), "{}\n");
        assert!(project.join("abc/subagents/x.jsonl").is_file());
        assert!(!slot.exists(), "the trash folder is cleaned up");
    }

    #[test]
    fn restore_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("abc.jsonl");
        fs::write(&transcript, "old\n").unwrap();
        let slot = move_to_trash(&transcript, &dir.path().join("trash"), "abc").unwrap();
        fs::write(&transcript, "new\n").unwrap();
        assert!(restore(&slot, &transcript).is_err());
        assert_eq!(fs::read_to_string(&transcript).unwrap(), "new\n");
    }

    #[test]
    fn same_id_deleted_twice_in_a_second_keeps_both() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("trash");
        let a = dir.path().join("p1/abc.jsonl");
        let b = dir.path().join("p2/abc.jsonl");
        for (path, body) in [(&a, "one\n"), (&b, "two\n")] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let slot_a = move_to_trash(&a, &root, "abc").unwrap();
        let slot_b = move_to_trash(&b, &root, "abc").unwrap();
        assert_ne!(slot_a, slot_b);
        restore(&slot_a, &a).unwrap();
        restore(&slot_b, &b).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "one\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "two\n");
    }

    #[test]
    fn restore_keeps_a_companion_it_could_not_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("abc.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        fs::create_dir_all(dir.path().join("abc")).unwrap();
        fs::write(dir.path().join("abc/old.txt"), "old").unwrap();
        let slot = move_to_trash(&transcript, &dir.path().join("trash"), "abc").unwrap();
        // Something recreated the companion dir meanwhile.
        fs::create_dir_all(dir.path().join("abc")).unwrap();
        restore(&slot, &transcript).unwrap();
        assert!(slot.join("abc/old.txt").is_file(), "left in the trash, not deleted");
    }

    #[test]
    fn prune_drops_only_old_folders() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["100-old", "5000-new", "notes"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        prune_older_than(dir.path(), 1000);
        assert!(!dir.path().join("100-old").exists());
        assert!(dir.path().join("5000-new").exists());
        assert!(
            dir.path().join("notes").exists(),
            "unrecognised folders are left alone"
        );
    }
}
