//! The search text is lowercased, so a match line built from it would read
//! `libc::lock_ex` for `libc::LOCK_EX`. This finds each piece a match line
//! shows in the transcript itself, to show it as written. It runs in the
//! background: transcripts run to hundreds of MB.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

/// A line of text and the byte range of the search hit in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub text: String,
    pub hit: (usize, usize),
}

pub struct Request {
    /// The search this belongs to; skipped once another search ran.
    pub generation: u64,
    /// The session (real index) and which part of its match line.
    pub session: usize,
    pub part: usize,
    pub path: PathBuf,
    /// The piece as the search text has it (lowercased).
    pub piece: Piece,
    /// How far into the session's search text the piece is (0.0–1.0); it's
    /// about as far into the transcript.
    pub at: f64,
}

pub struct Found {
    pub generation: u64,
    pub session: usize,
    pub part: usize,
    /// The piece as written.
    pub piece: Piece,
}

/// Start the background lookup. Requests from a search that's no longer
/// `current` are dropped unanswered.
pub fn spawn_worker(found: mpsc::Sender<Found>, current: Arc<AtomicU64>) -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::spawn(move || {
        while let Ok(req) = rx.recv() {
            let live = || current.load(Ordering::Relaxed) == req.generation;
            if !live() {
                continue;
            }
            let looked_up = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                as_written(&req.path, &req.piece, req.at, &live)
            }));
            let Ok(Some(piece)) = looked_up else {
                continue;
            };
            let reply = Found {
                generation: req.generation,
                session: req.session,
                part: req.part,
                piece,
            };
            if found.send(reply).is_err() {
                break;
            }
        }
    });
    tx
}

/// Bytes searched at a time, outward from where the piece likely is.
const CHUNK: usize = 8 << 20;
/// Longest stretch of the piece searched for.
const MAX_NEEDLE: usize = 96;
/// Candidate lines checked before giving up.
const MAX_CANDIDATES: usize = 256;

/// `piece` (lowercased search text) as the transcript at `path` has it, or
/// `None` when it can't be found or `live` turns false.
pub fn as_written(path: &Path, piece: &Piece, at: f64, live: &dyn Fn() -> bool) -> Option<Piece> {
    let (from, to) = needle(piece)?;
    // Case and accents as written, whatever they were.
    let pattern = crate::fold::unfolding_pattern(&piece.text[from..to]);
    let re = regex::bytes::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()?;
    // Read, not mapped: a mapped file that shrinks under us (rewritten by
    // another tool) is a SIGBUS, not an error.
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    let mut checked = 0;
    for (lo, hi) in chunks_outward(len, at, CHUNK) {
        if !live() {
            return None;
        }
        // Overlap the next chunk so a match across the seam is found (as
        // written, accents can make it longer than the folded needle).
        let data = read_range(&mut file, lo, hi + 4 * (to - from)).ok()?;
        for m in re.find_iter(&data) {
            let line = line_around(&mut file, &data, lo, m.start()).ok()?;
            if let Some(found) = in_line(&line, piece) {
                return Some(found);
            }
            checked += 1;
            if checked >= MAX_CANDIDATES {
                return None;
            }
        }
    }
    None
}

/// Bytes `lo..hi` of `file` (fewer at its end).
fn read_range(file: &mut File, lo: usize, hi: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(lo as u64))?;
    let mut out = Vec::with_capacity(hi.saturating_sub(lo));
    file.take(hi.saturating_sub(lo) as u64).read_to_end(&mut out)?;
    Ok(out)
}

/// The whole line of `file` holding byte `pos` of `data` (which is read
/// from file offset `lo`), reading past `data` when the line runs on.
fn line_around(file: &mut File, data: &[u8], lo: usize, pos: usize) -> std::io::Result<Vec<u8>> {
    /// Read this much at a time past `data`; give up on longer lines.
    const STEP: usize = 1 << 20;
    const MAX_LINE: usize = 256 << 20;
    let too_long = || std::io::Error::other("line too long");
    let start = match memchr::memrchr(b'\n', &data[..pos]) {
        Some(i) => lo + i + 1,
        None => {
            let mut until = lo;
            loop {
                if until == 0 {
                    break 0;
                }
                let from = until.saturating_sub(STEP);
                let back = read_range(file, from, until)?;
                if let Some(i) = memchr::memrchr(b'\n', &back) {
                    break from + i + 1;
                }
                until = from;
                if lo - until > MAX_LINE {
                    return Err(too_long());
                }
            }
        }
    };
    let end = match memchr::memchr(b'\n', &data[pos..]) {
        Some(i) => lo + pos + i,
        None => {
            let mut at = lo + data.len();
            loop {
                let ahead = read_range(file, at, at + STEP)?;
                if let Some(i) = memchr::memchr(b'\n', &ahead) {
                    break at + i;
                }
                at += ahead.len();
                if ahead.len() < STEP {
                    break at;
                }
                if at - start > MAX_LINE {
                    return Err(too_long());
                }
            }
        }
    };
    if start >= lo && end <= lo + data.len() {
        return Ok(data[start - lo..end - lo].to_vec());
    }
    read_range(file, start, end)
}

/// The byte range of `piece.text` to search for: the hit and what's around
/// it, as long as the transcript's JSON writes it verbatim (no quotes,
/// backslashes or control chars, which it escapes).
fn needle(piece: &Piece) -> Option<(usize, usize)> {
    let text = &piece.text;
    let verbatim = |c: char| c != '"' && c != '\\' && !c.is_control();
    // Start from the hit, or its longest verbatim stretch when it has
    // escaped chars in it (`release "crates"`).
    let (hit_from, hit_to) = piece.hit;
    let hit = text.get(hit_from..hit_to)?;
    let (mut from, mut to) = (hit_from, hit_from);
    let mut run_from = hit_from;
    for (i, c) in hit.char_indices() {
        let at = hit_from + i;
        if !verbatim(c) {
            run_from = at + c.len_utf8();
        } else if at + c.len_utf8() - run_from > to - from {
            (from, to) = (run_from, at + c.len_utf8());
        }
    }
    if from == to {
        return None;
    }
    if (from, to) != (hit_from, hit_to) {
        return Some((from, to));
    }
    let mut before = text[..hit_from].chars().rev().peekable();
    let mut after = text[hit_to..].chars().peekable();
    // Grow both ways in turn, so the hit stays near the middle.
    loop {
        let mut grew = false;
        if let Some(&c) = before.peek() {
            if verbatim(c) && to - from + c.len_utf8() <= MAX_NEEDLE {
                from -= c.len_utf8();
                before.next();
                grew = true;
            }
        }
        if let Some(&c) = after.peek() {
            if verbatim(c) && to - from + c.len_utf8() <= MAX_NEEDLE {
                to += c.len_utf8();
                after.next();
                grew = true;
            }
        }
        if !grew {
            return Some((from, to));
        }
    }
}

/// `[lo, hi)` ranges covering `0..len`, starting around `at` (0.0–1.0) and
/// moving outward both ways.
fn chunks_outward(len: usize, at: f64, chunk: usize) -> Vec<(usize, usize)> {
    let center = (at.clamp(0.0, 1.0) * len as f64) as usize;
    let first = center.saturating_sub(chunk / 2).min(len);
    let mut before = first;
    let mut after = (first + chunk).min(len);
    let mut out = vec![(before, after)];
    while before > 0 || after < len {
        if after < len {
            let hi = (after + chunk).min(len);
            out.push((after, hi));
            after = hi;
        }
        if before > 0 {
            let lo = before.saturating_sub(chunk);
            out.push((lo, before));
            before = lo;
        }
    }
    out
}

/// The transcript entry on `line`, searched for `piece`: the piece as
/// written there, when the entry has it.
fn in_line(line: &[u8], piece: &Piece) -> Option<Piece> {
    let entry: serde_json::Value = serde_json::from_slice(line).ok()?;
    let mut found = None;
    crate::parser::for_each_search_piece(&entry, &mut |text| {
        if found.is_none() {
            found = find_in(text.trim(), piece);
        }
    });
    found
}

/// Where `piece` (folded) is in `written` once that's folded the way the
/// search text is: that stretch of `written`, as written.
pub fn find_in(written: &str, piece: &Piece) -> Option<Piece> {
    let (folded, origin) = crate::fold::fold_with_origin(written);
    let at = folded.find(&piece.text)?;
    let map = |offset: usize| origin.get(at + offset).copied();
    let from = map(0)?;
    let to = map(piece.text.len())?;
    let hit = (map(piece.hit.0)? - from, map(piece.hit.1)? - from);
    Some(Piece {
        text: written.get(from..to)?.to_string(),
        hit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(text: &str, hit: &str) -> Piece {
        let at = text.find(hit).unwrap();
        Piece {
            text: text.to_string(),
            hit: (at, at + hit.len()),
        }
    }

    fn transcript(lines: &[serde_json::Value]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            use std::io::Write;
            writeln!(file, "{}", line).unwrap();
        }
        file
    }

    #[test]
    fn finds_the_piece_as_written() {
        let file = transcript(&[
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "Use flock for the Run Lock"}}),
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "Done.\nNow: libc::flock(f.as_raw_fd(), libc::LOCK_EX); \"quoted\""}
            ]}}),
        ]);
        let p = piece("now: libc::flock(f.as_raw_fd(), libc::lock_ex); \"quoted\"", "flock");
        let found = as_written(file.path(), &p, 0.5, &|| true).unwrap();
        assert_eq!(found.text, "Now: libc::flock(f.as_raw_fd(), libc::LOCK_EX); \"quoted\"");
        assert_eq!(&found.text[found.hit.0..found.hit.1], "flock");
    }

    #[test]
    fn keeps_offsets_right_across_case_changes_that_resize() {
        // 'İ' lowercases to two chars; Greek final sigma to one.
        let written = "İstanbul ΟΔΟΣ Flock";
        let lower = crate::fold::fold(written);
        let p = piece(&lower, "flock");
        let found = find_in(written, &p).unwrap();
        assert_eq!(found.text, written);
        assert_eq!(&found.text[found.hit.0..found.hit.1], "Flock");
    }

    #[test]
    fn skips_lines_that_only_share_the_needle() {
        // The quotes keep the needle down to the word, which line 1 has too.
        let file = transcript(&[
            serde_json::json!({"type": "user", "message": {"content": "FLOCK alone"}}),
            serde_json::json!({"type": "user", "message": {"content": "say \"Flock\" twice"}}),
        ]);
        let p = piece("say \"flock\" twice", "flock");
        assert_eq!(needle(&p), Some((5, 10)));
        let found = as_written(file.path(), &p, 0.0, &|| true).unwrap();
        assert_eq!(found.text, "say \"Flock\" twice");
    }

    #[test]
    fn gives_up_when_stale_or_missing() {
        let file = transcript(&[serde_json::json!({"type": "user", "message": {"content": "Flock"}})]);
        assert_eq!(as_written(file.path(), &piece("flock", "flock"), 0.0, &|| false), None);
        assert_eq!(as_written(file.path(), &piece("absent", "absent"), 0.0, &|| true), None);
        assert_eq!(as_written(Path::new("/no/such/file"), &piece("flock", "flock"), 0.0, &|| true), None);
    }

    #[test]
    fn needle_grows_around_the_hit_but_not_past_escapes() {
        let p = piece("say \"hi\" to the flock today", "flock");
        let (from, to) = needle(&p).unwrap();
        assert_eq!(&p.text[from..to], " to the flock today");
    }

    #[test]
    fn needle_inside_a_hit_with_escapes_is_its_longest_plain_stretch() {
        let p = piece("cut the release \"crates\" now", "release \"crates");
        let (from, to) = needle(&p).unwrap();
        assert_eq!(&p.text[from..to], "release ");
        assert_eq!(needle(&piece("a \"\" b", "\"\"")), None);
    }

    #[test]
    fn finds_a_hit_with_escapes_in_it() {
        let file = transcript(&[serde_json::json!({"type": "user", "message": {"content": "Cut the Release \"Crates\" now"}})]);
        let p = piece("cut the release \"crates\" now", "release \"crates");
        let found = as_written(file.path(), &p, 0.0, &|| true).unwrap();
        assert_eq!(found.text, "Cut the Release \"Crates\" now");
        assert_eq!(&found.text[found.hit.0..found.hit.1], "Release \"Crates");
    }

    #[test]
    fn reads_lines_longer_than_a_chunk_and_across_chunk_seams() {
        let long = "pad ".repeat(700_000); // 2.8 MB, past the 1 MB read step
        let file = transcript(&[
            serde_json::json!({"type": "user", "message": {"content": "first"}}),
            serde_json::json!({"type": "user", "message": {"content": format!("{}the Flock {}", long, long)}}),
            serde_json::json!({"type": "user", "message": {"content": "last"}}),
        ]);
        let p = piece("pad the flock pad", "flock");
        let found = as_written(file.path(), &p, 0.5, &|| true).unwrap();
        assert_eq!(found.text, "pad the Flock pad");
        // A chunk smaller than the line: the hit's line is read whole.
        let mut f = File::open(file.path()).unwrap();
        let len = f.metadata().unwrap().len() as usize;
        let mid = len / 2;
        let data = read_range(&mut f, mid, mid + 10).unwrap();
        let line = line_around(&mut f, &data, mid, 5).unwrap();
        assert!(line.starts_with(b"{") && line.ends_with(b"}"), "a whole JSON line");
        assert!(in_line(&line, &p).is_some());
    }

    #[test]
    fn finds_accented_text_from_its_folded_form() {
        let file = transcript(&[serde_json::json!({"type": "user", "message": {"content": "Στην Οδό Πανεπιστημίου, café"}})]);
        let folded = crate::fold::fold("Στην Οδό Πανεπιστημίου, café");
        let p = piece(&folded, "πανεπιστημιου");
        let found = as_written(file.path(), &p, 0.0, &|| true).unwrap();
        assert_eq!(found.text, "Στην Οδό Πανεπιστημίου, café");
        assert_eq!(&found.text[found.hit.0..found.hit.1], "Πανεπιστημίου");
    }

    #[test]
    fn chunks_cover_everything_once() {
        let chunks = chunks_outward(100, 0.5, 30);
        assert_eq!(chunks[0], (35, 65));
        let mut covered: Vec<(usize, usize)> = chunks.clone();
        covered.sort();
        assert_eq!(covered, vec![(0, 5), (5, 35), (35, 65), (65, 95), (95, 100)]);
        assert_eq!(chunks_outward(0, 0.5, 30), vec![(0, 0)]);
    }
}
