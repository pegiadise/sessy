//! Sessions open in a running Claude Code right now. Every interactive
//! Claude Code process keeps `<config>/sessions/<pid>.json` holding its
//! `sessionId` and a `status` ("busy" while it works).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSession {
    pub pid: u32,
    /// Claude is working (vs. waiting for input).
    pub busy: bool,
}

#[derive(Deserialize)]
struct PidFile {
    pid: u32,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    status: String,
}

/// `~/.claude/sessions` (or under `$CLAUDE_CONFIG_DIR`).
pub fn sessions_dir() -> PathBuf {
    crate::index::claude_config_dir().join("sessions")
}

/// Live sessions by session ID. Files left behind by a process that has
/// since exited are ignored.
pub fn scan(dir: &Path) -> HashMap<String, LiveSession> {
    scan_with(dir, pid_alive)
}

fn scan_with(dir: &Path, alive: impl Fn(u32) -> bool) -> HashMap<String, LiveSession> {
    let mut live = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<PidFile>(&text) else {
            continue;
        };
        if file.session_id.is_empty() || !alive(file.pid) {
            continue;
        }
        live.insert(
            file.session_id,
            LiveSession {
                pid: file.pid,
                busy: file.status == "busy",
            },
        );
    }
    live
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // Signal 0 only checks that the process exists; EPERM means it exists
    // but belongs to someone else.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// No cheap liveness check without extra dependencies: a pid file left by
/// a crashed process would mark its session open forever, so live
/// detection is off.
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reads_live_pid_files_and_skips_dead_ones() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).unwrap();
        write(
            "100.json",
            r#"{"pid":100,"sessionId":"aaa","cwd":"/x","status":"busy","kind":"interactive"}"#,
        );
        write(
            "200.json",
            r#"{"pid":200,"sessionId":"bbb","status":"idle"}"#,
        );
        write(
            "300.json",
            r#"{"pid":300,"sessionId":"ccc","status":"busy"}"#,
        );
        write("400.json", "not json");
        write("100.abcdef.key", "secret");

        let live = scan_with(dir.path(), |pid| pid != 300);
        assert_eq!(live.len(), 2);
        assert_eq!(
            live["aaa"],
            LiveSession {
                pid: 100,
                busy: true
            }
        );
        assert_eq!(
            live["bbb"],
            LiveSession {
                pid: 200,
                busy: false
            }
        );
        assert!(!live.contains_key("ccc"), "its process is gone");
    }

    #[test]
    fn scan_of_a_missing_dir_is_empty() {
        assert!(scan(Path::new("/does/not/exist")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn own_process_is_alive() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
    }
}
