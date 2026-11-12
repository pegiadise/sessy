//! The session search box: free text plus `key:value` filters, e.g.
//! `auth bug project:web is:open after:7d`.
//!
//! Only known keys are filters. A known key with a value that doesn't parse
//! (`on:click`, `after:soon`) is searched as text, so nothing typed is lost;
//! a known key with no value yet (`project:` mid-typing) is ignored.

use crate::session::SessionMeta;
use chrono::{Local, NaiveDate, TimeZone};

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Project name contains this.
    Project(String),
    /// Branch contains this.
    Branch(String),
    /// A changed file's path contains this.
    File(String),
    /// Pinned (`is:pinned`).
    Pinned,
    /// Open in a running Claude Code (`is:open`).
    Open,
    /// Has a linked pull request (`is:pr`).
    HasPr,
    /// Last active at or after this Unix time.
    After(i64),
    /// Last active before this Unix time.
    Before(i64),
    /// Last active on this local day.
    On(NaiveDate),
}

impl Filter {
    pub fn matches(&self, s: &SessionMeta, pinned: bool, open: bool) -> bool {
        match self {
            Filter::Project(v) => s.project_lc.contains(v.as_str()),
            Filter::Branch(v) => s.branch_lc.contains(v.as_str()),
            Filter::File(v) => s.changed_files_lc.contains(v.as_str()),
            Filter::Pinned => pinned,
            Filter::Open => open,
            Filter::HasPr => !s.prs.is_empty(),
            Filter::After(t) => s.timestamp >= *t,
            Filter::Before(t) => s.timestamp < *t,
            Filter::On(day) => local_day(s.timestamp) == Some(*day),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Query {
    /// The words left once filters are taken out: lowercased, single-spaced.
    pub text: String,
    pub filters: Vec<Filter>,
}

/// Split the search box into text and filters. `now` (Unix seconds) anchors
/// relative times like `after:7d`.
pub fn parse(input: &str, now: i64) -> Query {
    let mut words = Vec::new();
    let mut filters = Vec::new();
    for token in input.split_whitespace() {
        match classify(token, now) {
            Token::Filter(f) => filters.push(f),
            Token::Pending => {}
            Token::Text => words.push(token.to_lowercase()),
        }
    }
    Query {
        text: words.join(" "),
        filters,
    }
}

/// Whether `token` is a filter (or the start of one), for colouring it in
/// the search box.
pub fn is_filter_token(token: &str) -> bool {
    !matches!(classify(token, now_secs()), Token::Text)
}

/// The keys `classify` knows, for the search box's hint and the help screen.
pub const KEYS: &[&str] = &["project", "branch", "file", "is", "after", "before", "on"];

enum Token {
    Filter(Filter),
    /// A known key still waiting for its value.
    Pending,
    Text,
}

fn classify(token: &str, now: i64) -> Token {
    let Some((key, value)) = token.split_once(':') else {
        return Token::Text;
    };
    let key = key.to_ascii_lowercase();
    let known = KEYS.contains(&key.as_str()) || key == "since";
    if !known {
        return Token::Text;
    }
    if value.is_empty() {
        return Token::Pending;
    }
    // `file:///…` (a URL), `file::open` (a Rust path): text, not filters.
    if value.starts_with(':') || value.starts_with("//") {
        return Token::Text;
    }
    let v = value.to_lowercase();
    let filter = match key.as_str() {
        "project" => Some(Filter::Project(v)),
        "branch" => Some(Filter::Branch(v)),
        "file" => Some(Filter::File(v)),
        "is" => match v.as_str() {
            "pinned" | "pin" => Some(Filter::Pinned),
            "open" | "live" => Some(Filter::Open),
            "pr" => Some(Filter::HasPr),
            _ => None,
        },
        "after" | "since" => moment(&v, now).map(Filter::After),
        "before" => moment(&v, now).map(Filter::Before),
        "on" => day(&v, now).map(Filter::On),
        _ => None,
    };
    match filter {
        Some(f) => Token::Filter(f),
        None => Token::Text,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn local_day(timestamp: i64) -> Option<NaiveDate> {
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => Some(dt.date_naive()),
        _ => None,
    }
}

/// `today`, `yesterday` or `YYYY-MM-DD`, as a local day.
fn day(v: &str, now: i64) -> Option<NaiveDate> {
    let today = local_day(now)?;
    match v {
        "today" => Some(today),
        "yesterday" => today.pred_opt(),
        _ => NaiveDate::parse_from_str(v, "%Y-%m-%d").ok(),
    }
}

/// A point in time: a day (its local midnight) or a span back from `now`
/// (`1h`, `7d`, `2w`, `1m`, as `--recent` takes).
fn moment(v: &str, now: i64) -> Option<i64> {
    if let Some(d) = day(v, now) {
        // The day's first instant; where a DST change skips midnight, the
        // first hour that exists.
        return (0..3).find_map(|h| {
            Local
                .from_local_datetime(&d.and_hms_opt(h, 0, 0)?)
                .earliest()
                .map(|t| t.timestamp())
        });
    }
    let secs = crate::index::parse_recent_filter(v)?;
    Some(now.saturating_sub(i64::try_from(secs).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, m, d, h, 0, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn text_and_filters_separate() {
        let now = at(2026, 11, 12, 15);
        let q = parse("Auth  BUG project:Web is:open after:7d", now);
        assert_eq!(q.text, "auth bug");
        assert_eq!(
            q.filters,
            vec![
                Filter::Project("web".into()),
                Filter::Open,
                Filter::After(now - 7 * 86400),
            ]
        );
    }

    #[test]
    fn unknown_keys_and_unparsable_values_stay_text() {
        let now = at(2026, 11, 12, 15);
        let q = parse("https://github.com/x on:click is:weird after:soon file:///a file::open", now);
        assert_eq!(
            q.text,
            "https://github.com/x on:click is:weird after:soon file:///a file::open"
        );
        assert!(q.filters.is_empty());
    }

    #[test]
    fn a_key_without_its_value_yet_is_ignored() {
        let q = parse("deploy project:", at(2026, 11, 12, 15));
        assert_eq!(q.text, "deploy");
        assert!(q.filters.is_empty());
        assert!(is_filter_token("project:"));
        assert!(is_filter_token("is:open"));
        assert!(!is_filter_token("on:click"));
    }

    #[test]
    fn days_and_dates() {
        let now = at(2026, 11, 12, 15);
        let q = parse("on:yesterday before:2026-11-01 since:today", now);
        assert_eq!(
            q.filters,
            vec![
                Filter::On(NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()),
                Filter::Before(at(2026, 11, 1, 0)),
                Filter::After(at(2026, 11, 12, 0)),
            ]
        );
    }

    #[test]
    fn filters_match_sessions() {
        let mut s: SessionMeta = serde_json::from_value(serde_json::json!({
            "id": "a", "project": "Web", "branch": "feat/x", "name": "", "title": "",
            "last_message": "", "duration_secs": 0, "timestamp": at(2026, 11, 10, 9),
            "file_size": 0, "file_mtime": 0, "file_path": "/x.jsonl", "cwd": "",
            "message_count": 0, "tickets": [], "text_offset": 0, "text_len": 0,
            "name_lc": "", "title_lc": "", "project_lc": "web", "branch_lc": "feat/x",
            "permission_mode": "", "cc_version": "", "skills": [], "changed_files": [],
            "changed_files_lc": "src/auth.rs", "prs": [], "recap": ""
        }))
        .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        assert!(Filter::Project("we".into()).matches(&s, false, false));
        assert!(Filter::Branch("feat".into()).matches(&s, false, false));
        assert!(Filter::File("auth.rs".into()).matches(&s, false, false));
        assert!(Filter::On(day).matches(&s, false, false));
        assert!(!Filter::On(day.succ_opt().unwrap()).matches(&s, false, false));
        assert!(Filter::After(at(2026, 11, 10, 0)).matches(&s, false, false));
        assert!(!Filter::Before(at(2026, 11, 10, 0)).matches(&s, false, false));
        assert!(Filter::Pinned.matches(&s, true, false));
        assert!(!Filter::Open.matches(&s, true, false));
        assert!(!Filter::HasPr.matches(&s, false, false));
        s.prs = vec![crate::parser::PrLink {
            repo: "r".into(),
            number: 1,
            url: "u".into(),
        }];
        assert!(Filter::HasPr.matches(&s, false, false));
    }
}
