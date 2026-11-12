use crate::session::SessionMeta;
use crate::text_cache::{
    append_text_cache, commit_text_cache, text_cache_path, write_text_cache_tmp, TextCache,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const INDEX_VERSION: u32 = 9;

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionIndex {
    pub version: u32,
    pub sessions: Vec<SessionMeta>,
    /// Size of the `text.bin` written alongside this index. A mismatch on load
    /// means the pair is out of sync (crash, or two instances racing), and the
    /// recorded text offsets can't be trusted.
    pub text_cache_len: u64,
}

pub fn index_cache_path() -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("sessy");
    fs::create_dir_all(&cache_dir).ok();
    cache_dir.join("index.bin")
}

/// Encode an absolute directory path the way Claude Code names project dirs:
/// every non-alphanumeric character becomes `-` (not just `/` — dots,
/// underscores, spaces, and unicode too, e.g. `/a/.worktrees/x` → `-a--worktrees-x`).
pub fn encode_project_path(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `~/.claude`, or `$CLAUDE_CONFIG_DIR` when Claude Code has been pointed at
/// another config directory.
pub fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".claude")
        })
}

/// `~/.claude/projects` (under `$CLAUDE_CONFIG_DIR` when set).
pub fn claude_projects_dir() -> PathBuf {
    claude_config_dir().join("projects")
}

/// Index build progress, shared with whoever reports it: how many session
/// files need a (re)scan and how many of those are done.
#[derive(Debug, Default)]
pub struct Progress {
    pub total: std::sync::atomic::AtomicUsize,
    pub done: std::sync::atomic::AtomicUsize,
}

/// Whether a `.jsonl` file in a project dir is a resumable session. Old
/// Claude Code versions wrote subagent transcripts (`agent-<id>.jsonl`) next
/// to the sessions; those can't be resumed.
fn is_session_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && !path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("agent-"))
}

/// bincode 2 with the `legacy` config writes the same bytes as bincode 1's
/// `serialize`/`deserialize`, so caches written by older sessy builds still load.
pub fn serialize_index(index: &SessionIndex) -> Vec<u8> {
    bincode::serde::encode_to_vec(index, bincode::config::legacy()).unwrap_or_default()
}

pub fn deserialize_index(bytes: &[u8]) -> Option<SessionIndex> {
    // A damaged file (or a layout change without a version bump) can read
    // as a huge length; unlimited, decoding it aborts on the allocation,
    // on every start. Limited, it's just a cache to rebuild.
    const LIMIT: usize = 256 << 20;
    let config = bincode::config::legacy().with_limit::<LIMIT>();
    let (index, _): (SessionIndex, usize) =
        bincode::serde::decode_from_slice(bytes, config).ok()?;
    if index.version != INDEX_VERSION {
        return None;
    }
    Some(index)
}

/// What a session's files look like now: the transcript's size and mtime,
/// and its subagents'.
struct FileStats {
    size: u64,
    mtime: i64,
    /// What `SessionMeta::file_mtime` holds, compared to notice a change: a
    /// hash of the transcript's and the subagents' mtimes (to the
    /// nanosecond), the subagents' names and sizes, and their descriptions'
    /// stamps.
    changed: i64,
    subagents: Vec<crate::subagents::SubagentFile>,
}

fn file_stats(path: &Path) -> Option<FileStats> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let (subagents, dir_mtime) = crate::subagents::list(path);
    let mut h = Fnv::default();
    h.add(&crate::subagents::mtime_nanos(&metadata).to_le_bytes());
    h.add(&dir_mtime.to_le_bytes());
    for f in &subagents {
        h.add(f.path.file_name().map_or(&[][..], |n| n.as_encoded_bytes()));
        h.add(&f.size.to_le_bytes());
        h.add(&f.mtime.to_le_bytes());
        h.add(&f.meta.to_le_bytes());
    }
    Some(FileStats {
        size: metadata.len(),
        mtime,
        changed: h.0 as i64,
        subagents,
    })
}

/// FNV-1a, for change checks (not security).
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf29ce484222325)
    }
}

impl Fnv {
    fn add(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
}

/// The subagent files a text was built from: name, size, mtime, and its
/// description file's stamp.
fn fingerprint(files: &[crate::subagents::SubagentFile]) -> Vec<(String, u64, i64, i64)> {
    files
        .iter()
        .map(|f| {
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned());
            (name.unwrap_or_default(), f.size, f.mtime, f.meta)
        })
        .collect()
}

/// Where a session's scan stopped, to carry on from there when only more
/// was written (a session in use grows on every turn; a big one takes
/// seconds to read whole). Kept in `resume.bin` next to the index, not in
/// it, so the index stays readable by sessy versions that predate this.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resume {
    /// The cached text this goes with; anything else and it's ignored.
    text_offset: u64,
    text_len: u32,
    /// Bytes of the transcript read (whole lines), and a hash of the bytes
    /// just before that point, to notice a file rewritten rather than grown.
    scanned: u64,
    tail_check: u64,
    /// Bytes of the text that come from those lines.
    main_len: u32,
    /// The subagent files its subagent text was built from.
    subagents: Vec<(String, u64, i64, i64)>,
    scanner: crate::parser::Scanner,
}

const RESUME_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct ResumeFile {
    version: u32,
    sessions: std::collections::HashMap<PathBuf, Resume>,
}

fn resume_path(text_path: &Path) -> PathBuf {
    text_path.with_file_name("resume.bin")
}

fn load_resume(path: &Path) -> std::collections::HashMap<PathBuf, Resume> {
    const LIMIT: usize = 256 << 20;
    let config = bincode::config::legacy().with_limit::<LIMIT>();
    fs::read(path)
        .ok()
        .and_then(|bytes| bincode::serde::decode_from_slice::<ResumeFile, _>(&bytes, config).ok())
        .filter(|(f, _)| f.version == RESUME_VERSION)
        .map(|(f, _)| f.sessions)
        .unwrap_or_default()
}

fn save_resume(path: &Path, sessions: std::collections::HashMap<PathBuf, Resume>) {
    let file = ResumeFile { version: RESUME_VERSION, sessions };
    let Ok(bytes) = bincode::serde::encode_to_vec(&file, bincode::config::legacy()) else {
        return;
    };
    let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
    if fs::write(&tmp, bytes).is_err() || fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// FNV-1a of the (up to) 4 KB before byte `at` of `path`.
fn tail_check(path: &Path, at: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let from = at.saturating_sub(4096);
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::with_capacity((at - from) as usize);
    file.take(at - from).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 != at - from {
        return None;
    }
    let mut h = Fnv::default();
    h.add(&buf);
    Some(h.0)
}

/// Read a session whole: its metadata, its search text (its own, then its
/// subagents'), and where to carry on next time.
fn scan_full(path: &Path, stats: &FileStats) -> Option<(SessionMeta, Vec<u8>, Option<Resume>)> {
    let part = crate::parser::scan_from(path, crate::parser::Scanner::default(), 0)?;
    let scan = part.current.result()?;
    let mut text = part.text.into_bytes();
    text.extend_from_slice(crate::subagents::search_text(&stats.subagents).as_bytes());
    let resume = tail_check(path, part.scanned).map(|tail_check| Resume {
        text_offset: 0,
        text_len: 0,
        scanned: part.scanned,
        tail_check,
        main_len: part.text_complete as u32,
        subagents: fingerprint(&stats.subagents),
        scanner: part.resumable,
    });
    let meta = session_meta(path, stats, scan, text.len())?;
    Some((meta, text, resume))
}

/// Carry on reading a session from where `resume` stopped, reusing its
/// cached text (`old`): `None` when that isn't safe (the transcript was
/// rewritten, not just added to), and it has to be read whole.
fn scan_resumed(
    path: &Path,
    stats: &FileStats,
    cached: &SessionMeta,
    resume: &Resume,
    old: &[u8],
) -> Option<(SessionMeta, Vec<u8>, Option<Resume>)> {
    let main_len = resume.main_len as usize;
    if resume.text_offset != cached.text_offset
        || resume.text_len != cached.text_len
        || old.len() != cached.text_len as usize
        || main_len > old.len()
        || stats.size < resume.scanned
        || tail_check(path, resume.scanned)? != resume.tail_check
    {
        return None;
    }
    let part = crate::parser::scan_from(path, resume.scanner.clone(), resume.scanned)?;
    let scan = part.current.result()?;
    let subagents = fingerprint(&stats.subagents);
    let mut text = Vec::with_capacity(old.len() + part.text.len());
    text.extend_from_slice(&old[..main_len]);
    text.extend_from_slice(part.text.as_bytes());
    if subagents == resume.subagents {
        // The old subagent text, past the main text (and any unfinished
        // last line read then).
        let split = main_len + crate::subagents::split(&old[main_len..]);
        text.extend_from_slice(&old[split..]);
    } else {
        text.extend_from_slice(crate::subagents::search_text(&stats.subagents).as_bytes());
    }
    let resume = tail_check(path, part.scanned).map(|tail_check| Resume {
        text_offset: 0,
        text_len: 0,
        scanned: part.scanned,
        tail_check,
        main_len: (main_len + part.text_complete) as u32,
        subagents,
        scanner: part.resumable,
    });
    let meta = session_meta(path, stats, scan, text.len())?;
    Some((meta, text, resume))
}

/// Scan one session file whole (tests).
#[cfg(test)]
pub fn scan_session_file(path: &Path) -> Option<(SessionMeta, Vec<u8>)> {
    let stats = file_stats(path)?;
    scan_full(path, &stats).map(|(meta, text, _)| (meta, text))
}

fn session_meta(
    path: &Path,
    stats: &FileStats,
    scan: crate::parser::ScanResult,
    text_len: usize,
) -> Option<SessionMeta> {
    let file_size = stats.size;
    let file_mtime = stats.mtime;
    let id = path.file_stem()?.to_str()?.to_string();
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
    // When it was last active: its last timestamped entry. Claude Code also
    // appends bookkeeping (costs, the last prompt) later without one, which
    // moves the mtime weeks after the conversation ended.
    let last_active = scan
        .tail
        .as_ref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t.last_timestamp).ok())
        .map_or(file_mtime, |t| t.timestamp());

    // Headline priority: an explicit /rename wins, then Claude's AI title,
    // then the random word-slug, then nothing.
    let name = if !rename.is_empty() {
        rename
    } else if !scan.ai_title.is_empty() {
        scan.ai_title.clone()
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

    // Folded like the search text (`fold`: case and accents).
    use crate::fold::fold;
    let name_lc = fold(&name);
    let title_lc = fold(&scan.head.title);
    let project_lc = fold(&project);
    let branch_lc = fold(&branch);
    let changed_files_lc = fold(&scan.changed_files.join("\n"));

    let meta = SessionMeta {
        id,
        project,
        branch,
        name,
        title: scan.head.title,
        last_message,
        duration_secs,
        timestamp: last_active,
        file_size,
        // The check for changes: subagents' too.
        file_mtime: stats.changed,
        file_path: path.to_path_buf(),
        cwd: scan.head.cwd,
        message_count: scan.message_count,
        tickets: scan.tickets,
        text_offset: 0, // filled in finalize step
        text_len: text_len as u32,
        name_lc,
        title_lc,
        project_lc,
        branch_lc,
        permission_mode: scan.permission_mode,
        cc_version: scan.cc_version,
        skills: scan.skills,
        changed_files: scan.changed_files,
        changed_files_lc,
        prs: scan.prs,
        recap: scan.recap,
    };
    Some(meta)
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

/// One session after the scan: still as cached (its text already in
/// `text.bin` at its recorded offset), or (re)scanned with its text. Either
/// way with where its scan can carry on from.
enum Scanned {
    Reused(SessionMeta, Option<Resume>),
    Fresh(SessionMeta, Vec<u8>, Option<Resume>),
}

/// Build the index from the session files, reusing `cached` for files that
/// haven't changed. Returns the index and whether it differs from `cached`
/// (i.e. needs saving).
///
/// `text.bin` is only appended to: unchanged sessions keep their bytes and
/// offsets, new and changed ones go on the end. It's rewritten from scratch
/// (compacting away text of changed and deleted sessions) when that dead
/// text outweighs the live text, or when appending isn't safe.
pub fn build_index(
    cached: Option<SessionIndex>,
    force_rebuild: bool,
    progress: Option<&Progress>,
) -> (SessionIndex, bool) {
    build_index_at(
        &claude_projects_dir(),
        &text_cache_path(),
        cached,
        force_rebuild,
        progress,
    )
}

/// `build_index` over an explicit projects directory and `text.bin`.
fn build_index_at(
    projects_dir: &Path,
    text_path: &Path,
    cached: Option<SessionIndex>,
    force_rebuild: bool,
    progress: Option<&Progress>,
) -> (SessionIndex, bool) {
    // No (readable) projects folder: nothing to list, and nothing to save
    // either — an empty index would throw away the cache for the next run.
    if fs::read_dir(projects_dir).is_err() {
        let empty = SessionIndex {
            version: INDEX_VERSION,
            sessions: vec![],
            text_cache_len: 0,
        };
        return (empty, false);
    }

    // Open the previous text.bin so we can reuse bytes for unchanged sessions.
    let prev_text = TextCache::open(text_path);
    // The cached offsets only mean something for the text.bin they were
    // written with.
    let cached = cached.filter(|c| !force_rebuild && prev_text.len() as u64 == c.text_cache_len);

    let cache_map: std::collections::HashMap<PathBuf, &SessionMeta> = cached
        .as_ref()
        .map(|idx| idx.sessions.iter().map(|s| (s.file_path.clone(), s)).collect())
        .unwrap_or_default();

    let mut file_entries: Vec<PathBuf> = Vec::new();
    if let Ok(project_dirs) = fs::read_dir(projects_dir) {
        for proj_entry in project_dirs.flatten() {
            if !proj_entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let proj_dir = proj_entry.path();
            if let Ok(files) = fs::read_dir(&proj_dir) {
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if is_session_file(&path) && path.is_file() {
                        file_entries.push(path);
                    }
                }
            }
        }
    }

    // Where each cached session's scan stopped (only with a usable cache:
    // carrying on needs its text).
    let resume_file = resume_path(text_path);
    let mut resumes = if cached.is_some() {
        load_resume(&resume_file)
    } else {
        std::collections::HashMap::new()
    };

    let stats: Vec<(PathBuf, FileStats)> = file_entries
        .into_par_iter()
        .filter_map(|path| file_stats(&path).map(|st| (path, st)))
        .collect();

    // Count what needs a rescan up front so progress can show a total.
    let unchanged = |path: &PathBuf, st: &FileStats| {
        cache_map
            .get(path)
            .is_some_and(|c| c.file_mtime == st.changed && c.file_size == st.size)
    };
    if let Some(p) = progress {
        let stale = stats.iter().filter(|(path, st)| !unchanged(path, st)).count();
        p.total.store(stale, std::sync::atomic::Ordering::Relaxed);
    }

    // For each file: reuse the cached meta (and its text in place), carry
    // on its scan from where it stopped, or read it whole.
    let scanned: Vec<Scanned> = stats
        .par_iter()
        .filter_map(|(path, st)| {
            let cached_entry = cache_map.get(path);
            let resume = resumes.get(path);
            if unchanged(path, st) {
                return cached_entry.map(|c| Scanned::Reused((*c).clone(), resume.cloned()));
            }
            let resumed = cached_entry.zip(resume).and_then(|(c, r)| {
                scan_resumed(path, st, c, r, prev_text.slice(c.text_offset, c.text_len))
            });
            let result = resumed.or_else(|| scan_full(path, st));
            if let Some(p) = progress {
                p.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            result.map(|(m, t, r)| Scanned::Fresh(m, t, r))
        })
        .collect();
    resumes.clear();

    let reused = scanned.iter().filter(|s| matches!(s, Scanned::Reused(..))).count();
    let cached_count = cached.as_ref().map_or(0, |c| c.sessions.len());
    let cached_len = cached.as_ref().map_or(0, |c| c.text_cache_len);
    if cached.is_some() && reused == scanned.len() && reused == cached_count {
        // Nothing new, changed or gone: the cache on disk is already right.
        let sessions = scanned
            .into_iter()
            .filter_map(|s| match s {
                Scanned::Reused(m, _) => Some(m),
                Scanned::Fresh(..) => None,
            })
            .collect();
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions,
            text_cache_len: cached_len,
        };
        return (index, false);
    }

    let fresh_bytes: u64 = scanned
        .iter()
        .map(|s| match s {
            Scanned::Fresh(_, t, _) => t.len() as u64,
            Scanned::Reused(..) => 0,
        })
        .sum();
    let live_bytes: u64 = fresh_bytes
        + scanned
            .iter()
            .map(|s| match s {
                Scanned::Reused(m, _) => m.text_len as u64,
                Scanned::Fresh(..) => 0,
            })
            .sum::<u64>();

    // Append when the result stays at least half live text.
    if cached.is_some() && live_bytes * 2 >= cached_len + fresh_bytes {
        let chunks: Vec<&[u8]> = scanned
            .iter()
            .filter_map(|s| match s {
                Scanned::Fresh(_, t, _) => Some(t.as_slice()),
                Scanned::Reused(..) => None,
            })
            .collect();
        if let Ok(offsets) = append_text_cache(text_path, cached_len, &chunks) {
            let mut offsets = offsets.into_iter();
            let sessions = scanned
                .into_iter()
                .map(|s| match s {
                    Scanned::Reused(m, r) => (m, r),
                    Scanned::Fresh(mut m, _, r) => {
                        if let Some((offset, len)) = offsets.next() {
                            m.text_offset = offset;
                            m.text_len = len;
                        }
                        (m, r)
                    }
                })
                .collect();
            return finish(sessions, cached_len + fresh_bytes, &resume_file);
        }
        // Appending failed (another instance got there first?): rewrite,
        // copying from the mapping of the file the offsets belong to.
        return rewrite_text_cache(scanned, prev_text, text_path);
    }

    rewrite_text_cache(scanned, prev_text, text_path)
}

/// Write a fresh, compact `text.bin` (reused sessions' text copied from
/// `prev_text`) and the index pointing into it.
fn rewrite_text_cache(
    scanned: Vec<Scanned>,
    prev_text: TextCache,
    text_path: &Path,
) -> (SessionIndex, bool) {
    let chunks: Vec<&[u8]> = scanned
        .iter()
        .map(|s| match s {
            Scanned::Reused(m, _) => prev_text.slice(m.text_offset, m.text_len),
            Scanned::Fresh(_, t, _) => t.as_slice(),
        })
        .collect();
    let written = write_text_cache_tmp(text_path, &chunks);
    // Windows can't replace a file that's still mapped.
    drop(chunks);
    drop(prev_text);
    let offsets = written.and_then(|(tmp, offsets)| {
        commit_text_cache(&tmp, text_path)?;
        Ok(offsets)
    });
    let metas = scanned.into_iter().map(|s| match s {
        Scanned::Reused(m, r) | Scanned::Fresh(m, _, r) => (m, r),
    });
    let offsets = match offsets {
        Ok(o) => o,
        Err(e) => {
            eprintln!("sessy: failed to write text cache: {}", e);
            // Without text.bin, full-text search just finds nothing; keep the
            // sessions browsable. An impossible cache length forces a rescan
            // next time.
            let sessions = metas
                .map(|(mut meta, _)| {
                    meta.text_offset = 0;
                    meta.text_len = 0;
                    meta
                })
                .collect();
            let index = SessionIndex {
                version: INDEX_VERSION,
                sessions,
                text_cache_len: u64::MAX,
            };
            return (index, true);
        }
    };

    let text_cache_len = offsets
        .last()
        .map(|&(offset, len)| offset + len as u64)
        .unwrap_or(0);
    let sessions = metas
        .zip(offsets)
        .map(|((mut meta, resume), (offset, len))| {
            meta.text_offset = offset;
            meta.text_len = len;
            (meta, resume)
        })
        .collect();
    finish(sessions, text_cache_len, &resume_path(text_path))
}

/// The index for `sessions` (their text in place), with where each one's
/// scan stopped saved for next time, pointing at that text.
fn finish(
    sessions: Vec<(SessionMeta, Option<Resume>)>,
    text_cache_len: u64,
    resume_file: &Path,
) -> (SessionIndex, bool) {
    let mut resumes = std::collections::HashMap::new();
    let sessions = sessions
        .into_iter()
        .map(|(meta, resume)| {
            if let Some(mut r) = resume {
                r.text_offset = meta.text_offset;
                r.text_len = meta.text_len;
                resumes.insert(meta.file_path.clone(), r);
            }
            meta
        })
        .collect();
    save_resume(resume_file, resumes);
    let index = SessionIndex {
        version: INDEX_VERSION,
        sessions,
        text_cache_len,
    };
    (index, true)
}

pub fn load_cached_index() -> Option<SessionIndex> {
    let path = index_cache_path();
    let bytes = fs::read(&path).ok()?;
    let index = deserialize_index(&bytes)?;
    let text_len = fs::metadata(text_cache_path()).map(|m| m.len()).unwrap_or(0);
    if text_len != index.text_cache_len {
        return None;
    }
    Some(index)
}

pub fn save_index(index: &SessionIndex) {
    let path = index_cache_path();
    let bytes = serialize_index(index);
    // Write-then-rename so a crash or a concurrent instance never leaves a
    // half-written index behind.
    let tmp = path.with_extension(format!("bin.{}.tmp", std::process::id()));
    let written = fs::File::create(&tmp).and_then(|mut f| {
        use std::io::Write;
        f.write_all(&bytes)?;
        // On disk before the rename makes it the index, as text.bin is.
        f.sync_all()
    });
    if written.is_err() || fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// Held while one sessy instance loads, builds and saves the index, so two
/// starting at once never interleave writes to `index.bin`/`text.bin` (the
/// pair is only checked by size, which can't tell an appended layout from
/// a rewritten one). Released when dropped. Best effort: without a lock
/// (no cache dir, not unix) instances simply don't wait for each other.
pub struct CacheLock {
    _file: Option<fs::File>,
}

pub fn lock_cache() -> CacheLock {
    let path = index_cache_path().with_file_name("index.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok();
    #[cfg(unix)]
    if let Some(f) = &file {
        use std::os::unix::io::AsRawFd;
        // Waits for another instance to finish its build (usually ms). One
        // that's suspended (Ctrl+Z) or stuck would hold it indefinitely:
        // say what's being waited for.
        unsafe {
            if libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 {
                eprintln!("sessy: waiting for another sessy to finish indexing…");
                libc::flock(f.as_raw_fd(), libc::LOCK_EX);
            }
        }
    }
    CacheLock { _file: file }
}

/// Remove temp files that a killed or crashed instance left in the cache
/// dir (`*.tmp`, more than an hour old: a live one is seconds old).
pub fn sweep_stale_temp_files() {
    let Some(dir) = index_cache_path().parent().map(Path::to_path_buf) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let hour = std::time::Duration::from_secs(3600);
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("tmp") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > hour);
        if old {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn parse_recent_filter(s: &str) -> Option<u64> {
    let s = s.trim();
    let unit = s.chars().last()?;
    let num_str = &s[..s.len() - unit.len_utf8()];
    let num: u64 = num_str.parse().ok()?;
    let unit_secs: u64 = match unit {
        'h' => 3600,
        'd' => 86400,
        'w' => 7 * 86400,
        'm' => 30 * 86400,
        _ => return None,
    };
    // Reject absurd windows instead of overflowing into a bogus cutoff.
    num.checked_mul(unit_secs).filter(|&secs| secs <= i64::MAX as u64)
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
    fn test_scan_session_file_uses_ai_title() {
        let (meta, _text) = scan_session_file(&fixture_path("modern_session.jsonl"))
            .expect("should produce SessionMeta + text");
        // No /rename and no slug → aiTitle becomes the headline name.
        assert_eq!(meta.name, "Add JWT login endpoint");
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.permission_mode, "plan");
        assert_eq!(meta.cc_version, "2.1.0");
        assert!(
            meta.changed_files_lc.contains("src/auth.rs"),
            "changed_files_lc: {:?}",
            meta.changed_files_lc
        );
        assert!(meta.skills.contains(&"brainstorming".to_string()));
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
            message_count: 0,
            tickets: vec![],
            text_offset: 0,
            text_len: 0,
            name_lc: String::new(),
            title_lc: "hello world".to_string(),
            project_lc: "test".to_string(),
            branch_lc: "main".to_string(),
            permission_mode: String::new(),
            cc_version: String::new(),
            skills: vec![],
            changed_files: vec![],
            changed_files_lc: String::new(),
            prs: vec![],
            recap: String::new(),
        }];
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions,
            text_cache_len: 0,
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
            text_cache_len: 0,
        };
        let bytes = serialize_index(&index);
        let restored = deserialize_index(&bytes);
        assert!(restored.is_none());
    }

    #[test]
    fn test_encode_project_path_plain() {
        assert_eq!(
            encode_project_path("/Users/me/code/foo"),
            "-Users-me-code-foo"
        );
    }

    #[test]
    fn test_encode_project_path_dots_underscores_unicode() {
        // Claude Code replaces every non-alphanumeric char, not just `/`.
        assert_eq!(
            encode_project_path("/Users/me/code/acme/web/.worktrees/abc-6370"),
            "-Users-me-code-acme-web--worktrees-abc-6370"
        );
        assert_eq!(encode_project_path("/srv/my_app.v2"), "-srv-my-app-v2");
        assert_eq!(encode_project_path("/home/ρώτα με"), "-home--------");
    }

    #[test]
    fn test_parse_recent_filter() {
        assert_eq!(parse_recent_filter("7d"), Some(7 * 86400));
        assert_eq!(parse_recent_filter("1h"), Some(3600));
        assert_eq!(parse_recent_filter("2w"), Some(14 * 86400));
        assert_eq!(parse_recent_filter("1m"), Some(30 * 86400));
        assert_eq!(parse_recent_filter("garbage"), None);
        assert_eq!(parse_recent_filter("d"), None);
        assert_eq!(parse_recent_filter("99999999999999999999d"), None);
        assert_eq!(parse_recent_filter("999999999999999w"), None);
    }

    #[test]
    fn a_damaged_index_is_rebuilt_not_fatal() {
        let index = SessionIndex {
            version: INDEX_VERSION,
            sessions: vec![],
            text_cache_len: 7,
        };
        let mut bytes = serialize_index(&index);
        assert!(deserialize_index(&bytes).is_some());
        // The sessions vec's length, as a huge number.
        bytes[4..12].copy_from_slice(&(1u64 << 62).to_le_bytes());
        assert!(deserialize_index(&bytes).is_none());
    }

    #[test]
    fn a_missing_projects_folder_leaves_the_cache_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (index, changed) =
            build_index_at(&dir.path().join("nope"), &dir.path().join("text.bin"), None, false, None);
        assert!(index.sessions.is_empty());
        assert!(!changed, "nothing to save over the cache");
    }

    #[test]
    fn listed_date_is_the_last_activity_not_the_mtime() {
        let dir = tempfile::tempdir().unwrap();
        write_session(dir.path(), "a", "hello");
        // Bookkeeping appended weeks later, with no timestamp.
        let path = dir.path().join("a.jsonl");
        let mut body = fs::read_to_string(&path).unwrap();
        body.push_str("{\"type\":\"last-prompt\",\"lastPrompt\":\"thanks\"}\n");
        fs::write(&path, body).unwrap();
        let (meta, _) = scan_session_file(&path).unwrap();
        let last = chrono::DateTime::parse_from_rfc3339("2026-11-10T10:05:00Z").unwrap();
        assert_eq!(meta.timestamp, last.timestamp());
        assert_ne!(meta.file_mtime, 0, "the change fingerprint is kept");
    }

    fn write_session(dir: &Path, id: &str, words: &str) {
        let line = |ts: &str, text: &str| {
            serde_json::json!({
                "type": "user",
                "timestamp": ts,
                "cwd": "/work/app",
                "message": {"content": text},
            })
            .to_string()
        };
        let body = format!(
            "{}\n{}\n",
            line("2026-11-10T10:00:00Z", words),
            line("2026-11-10T10:05:00Z", "thanks")
        );
        fs::write(dir.join(format!("{}.jsonl", id)), body).unwrap();
    }

    fn text_of<'a>(cache: &'a TextCache, idx: &SessionIndex, id: &str) -> &'a str {
        let s = idx.sessions.iter().find(|s| s.id == id).unwrap();
        std::str::from_utf8(cache.slice(s.text_offset, s.text_len)).unwrap()
    }

    /// Bump a file's mtime so the index sees it as changed.
    fn touch_later(path: &Path) {
        let file = fs::OpenOptions::new().append(true).open(path).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        file.set_modified(later).unwrap();
    }

    #[test]
    fn test_incremental_build_appends_and_skips_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        write_session(&proj, "a", "alpha words");
        write_session(&proj, "b", "bravo words");

        let (first, changed) = build_index_at(&projects, &text, None, false, None);
        assert!(changed);
        assert_eq!(first.sessions.len(), 2);
        let len_after_first = fs::metadata(&text).unwrap().len();
        assert_eq!(first.text_cache_len, len_after_first);

        // Nothing changed: no writes, same offsets.
        let a_offset = first.sessions.iter().find(|s| s.id == "a").unwrap().text_offset;
        let (second, changed) = build_index_at(&projects, &text, Some(first), false, None);
        assert!(!changed, "an unchanged index needn't be saved");
        assert_eq!(fs::metadata(&text).unwrap().len(), len_after_first);
        let a = second.sessions.iter().find(|s| s.id == "a").unwrap();
        assert_eq!(a.text_offset, a_offset);

        // One session changes: its new text is appended, the other stays put.
        write_session(&proj, "b", "bravo charlie words");
        touch_later(&proj.join("b.jsonl"));
        let (third, changed) = build_index_at(&projects, &text, Some(second), false, None);
        assert!(changed);
        assert!(fs::metadata(&text).unwrap().len() > len_after_first, "appended");
        assert_eq!(third.text_cache_len, fs::metadata(&text).unwrap().len());
        let a = third.sessions.iter().find(|s| s.id == "a").unwrap();
        assert_eq!(a.text_offset, a_offset, "unchanged sessions keep their bytes");
        let cache = TextCache::open(&text);
        assert!(text_of(&cache, &third, "a").contains("alpha words"));
        assert!(text_of(&cache, &third, "b").contains("bravo charlie words"));

        // A deleted session drops out (and needs a save).
        fs::remove_file(proj.join("a.jsonl")).unwrap();
        let (fourth, changed) = build_index_at(&projects, &text, Some(third), false, None);
        assert!(changed);
        assert_eq!(fourth.sessions.len(), 1);
    }

    #[test]
    fn test_mostly_dead_text_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        write_session(&proj, "a", &"long text ".repeat(200));
        write_session(&proj, "b", "short");
        let (idx, _) = build_index_at(&projects, &text, None, false, None);
        // "a" shrinks to almost nothing: its old 2 KB would be mostly dead.
        write_session(&proj, "a", "tiny");
        touch_later(&proj.join("a.jsonl"));
        let (idx, _) = build_index_at(&projects, &text, Some(idx), false, None);
        let live: u64 = idx.sessions.iter().map(|s| s.text_len as u64).sum();
        assert_eq!(fs::metadata(&text).unwrap().len(), live, "rewritten without the dead text");
        let cache = TextCache::open(&text);
        assert!(text_of(&cache, &idx, "a").contains("tiny"));
        assert!(text_of(&cache, &idx, "b").contains("short"));
    }

    #[test]
    fn test_cache_for_another_text_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        write_session(&proj, "a", "alpha");
        let (mut idx, _) = build_index_at(&projects, &text, None, false, None);
        idx.text_cache_len += 1; // as if text.bin was replaced since
        let (idx, changed) = build_index_at(&projects, &text, Some(idx), false, None);
        assert!(changed, "rescanned instead of trusting stale offsets");
        let cache = TextCache::open(&text);
        assert!(text_of(&cache, &idx, "a").contains("alpha"));
    }

    fn user_line(text: &str) -> String {
        serde_json::json!({"type": "user", "timestamp": "2026-11-10T11:00:00Z", "cwd": "/work/app",
                           "message": {"content": text}})
        .to_string()
            + "\n"
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
        touch_later(path);
    }

    fn session<'a>(idx: &'a SessionIndex, id: &str) -> &'a SessionMeta {
        idx.sessions.iter().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn test_a_grown_session_carries_on_from_where_its_scan_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        let path = proj.join("a.jsonl");
        // Longer than the 4 KB the rewrite check covers.
        let padding: String = (0..40).map(|i| user_line(&format!("filler {} {}", i, "x".repeat(100)))).collect();
        fs::write(&path, user_line("first ask") + &padding).unwrap();
        let (idx, _) = build_index_at(&projects, &text, None, false, None);
        assert!(resume_path(&text).exists());

        // The first prompt edited in place (same length): a whole read would
        // see it, carrying on doesn't read it again. That's how this test
        // tells the two apart.
        let body = fs::read_to_string(&path).unwrap().replacen("first ask", "FIRST ASK", 1);
        fs::write(&path, body).unwrap();
        append(&path, &user_line("second ask"));
        let (idx, changed) = build_index_at(&projects, &text, Some(idx), false, None);
        assert!(changed);
        let a = session(&idx, "a");
        assert_eq!(a.title, "first ask", "carried on, not read again");
        assert_eq!(a.last_message, "second ask");
        assert_eq!(a.message_count, 42);
        let cache = TextCache::open(&text);
        let t = text_of(&cache, &idx, "a");
        assert!(t.starts_with("first ask\n") && t.ends_with("second ask\n"), "old text, then the new");

        // No resume.bin (an older sessy rewrote the cache): read whole.
        fs::remove_file(resume_path(&text)).unwrap();
        append(&path, &user_line("third ask"));
        let (idx, _) = build_index_at(&projects, &text, Some(idx), false, None);
        assert_eq!(session(&idx, "a").title, "FIRST ASK");
        assert_eq!(session(&idx, "a").message_count, 43);

        // Rewritten rather than grown (the end changed): read whole.
        let body = fs::read_to_string(&path).unwrap().replace("third ask", "third ASK");
        fs::write(&path, body + &user_line("fourth ask")).unwrap();
        touch_later(&path);
        let (idx, _) = build_index_at(&projects, &text, Some(idx), false, None);
        let cache = TextCache::open(&text);
        assert!(text_of(&cache, &idx, "a").contains("third ask\nfourth ask"));
        assert_eq!(session(&idx, "a").message_count, 44);
    }

    #[test]
    fn test_an_unfinished_last_line_is_counted_once_it_is_whole() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        let path = proj.join("a.jsonl");
        let whole = user_line("half written");
        let (head, tail) = whole.split_at(whole.len() - 10);
        fs::write(&path, user_line("first") + head).unwrap();
        let (idx, _) = build_index_at(&projects, &text, None, false, None);
        assert_eq!(session(&idx, "a").message_count, 1, "the unfinished line isn't JSON yet");
        append(&path, tail);
        let (idx, _) = build_index_at(&projects, &text, Some(idx), false, None);
        assert_eq!(session(&idx, "a").message_count, 2);
        assert_eq!(session(&idx, "a").last_message, "half written");
        let cache = TextCache::open(&text);
        assert_eq!(text_of(&cache, &idx, "a"), "first\nhalf written\n");
    }

    #[test]
    fn test_subagents_are_searched_and_noticed_when_they_change() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let proj = projects.join("-work-app");
        fs::create_dir_all(&proj).unwrap();
        let text = dir.path().join("text.bin");
        let path = proj.join("a.jsonl");
        fs::write(&path, user_line("audit the index")).unwrap();
        let (idx, _) = build_index_at(&projects, &text, None, false, None);

        // A subagent starts: noticed though the transcript didn't change.
        let sub = proj.join("a/subagents");
        fs::create_dir_all(&sub).unwrap();
        let said = serde_json::json!({"type": "assistant", "isSidechain": true,
            "message": {"content": [{"type": "text", "text": "Found a flock race"}]}});
        fs::write(sub.join("agent-x.jsonl"), format!("{}\n", said)).unwrap();
        fs::write(sub.join("agent-x.meta.json"), r#"{"description":"Check the Locks"}"#).unwrap();
        let (idx, changed) = build_index_at(&projects, &text, Some(idx), false, None);
        assert!(changed);
        let cache = TextCache::open(&text);
        assert_eq!(text_of(&cache, &idx, "a"), "audit the index\n\u{1}check the locks\nfound a flock race\n");

        // The transcript grows, the subagent doesn't: its text is kept.
        append(&path, &user_line("thanks"));
        let (idx, _) = build_index_at(&projects, &text, Some(idx), false, None);
        let cache = TextCache::open(&text);
        assert_eq!(
            text_of(&cache, &idx, "a"),
            "audit the index\nthanks\n\u{1}check the locks\nfound a flock race\n"
        );
        assert_eq!(session(&idx, "a").message_count, 2, "subagents' turns aren't the person's");

        // Its description is rewritten, nothing else: read again.
        fs::write(sub.join("agent-x.meta.json"), r#"{"description":"Check the flock calls"}"#).unwrap();
        let (idx, changed) = build_index_at(&projects, &text, Some(idx), false, None);
        assert!(changed);
        let cache = TextCache::open(&text);
        assert_eq!(
            text_of(&cache, &idx, "a"),
            "audit the index\nthanks\n\u{1}check the flock calls\nfound a flock race\n"
        );
    }

    #[test]
    fn test_agent_transcripts_are_not_sessions() {
        assert!(is_session_file(Path::new("/p/-a/1b2c.jsonl")));
        assert!(!is_session_file(Path::new("/p/-a/agent-a5c3eb79.jsonl")));
        assert!(!is_session_file(Path::new("/p/-a/notes.txt")));
    }
}
