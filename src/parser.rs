use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;

fn ticket_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b[A-Z][A-Z0-9]{1,9}-\d{1,7}\b|(?:^|[^A-Za-z0-9_])#\d{1,7}\b").unwrap()
    })
}

pub fn extract_tickets_into(text: &str, out: &mut std::collections::HashSet<String>) {
    for m in ticket_regex().find_iter(text) {
        let s = m.as_str();
        let trimmed = if let Some(pos) = s.find('#') { &s[pos..] } else { s };
        out.insert(trimmed.to_string());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

/// Who produced a preview line. `Tool` lines are only emitted when tool
/// activity is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
    Tool,
}

impl Speaker {
    /// Render prefix shown before the message text.
    pub fn prefix(self) -> &'static str {
        match self {
            Speaker::User => "USER: ",
            Speaker::Assistant => "ASST: ",
            Speaker::Tool => "TOOL: ",
        }
    }

    /// Lowercased prefix, used when wrapping pre-lowercased text for search.
    pub fn prefix_lc(self) -> &'static str {
        match self {
            Speaker::User => "user: ",
            Speaker::Assistant => "asst: ",
            Speaker::Tool => "tool: ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: Role,
    pub text: String,
}

pub struct HeadMeta {
    pub title: String,
    pub branch: String,
    pub slug: String,
    pub first_timestamp: String,
    pub cwd: String,
}

pub struct TailMeta {
    pub last_human_message: String,
    pub last_timestamp: String,
    pub rename: String,
}

fn is_human_message(entry: &Value) -> bool {
    entry.get("type").and_then(|t| t.as_str()) == Some("user")
        && entry.get("isMeta").and_then(|m| m.as_bool()) != Some(true)
        && entry.get("toolUseResult").is_none()
        && entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .is_some_and(|c| !is_command_noise(c))
}

/// Machine-generated content stored as `type:"user"` turns — slash-command
/// invocations, local command output, background-task notifications. These
/// must not drive titles, "left off", message counts, search text, or
/// preview lines.
fn is_command_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-name>")
        || t.starts_with("<command-message>")
        || t.starts_with("<local-command-stdout>")
        || t.starts_with("<local-command-caveat>")
        || t.starts_with("<task-notification>")
        || t.starts_with("<system-reminder>")
}

/// Fallback headline for sessions containing only command noise: the slash
/// command that was run, or a generic label.
fn command_noise_title(content: &str) -> String {
    const OPEN: &str = "<command-name>";
    const CLOSE: &str = "</command-name>";
    if let Some(start) = content.find(OPEN) {
        let rest = &content[start + OPEN.len()..];
        if let Some(end) = rest.find(CLOSE) {
            let name = rest[..end].trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    "(command output)".to_string()
}

fn human_message_text(entry: &Value) -> Option<String> {
    entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| {
            let trimmed = s.trim();
            if trimmed.chars().count() > 200 {
                let end = trimmed
                    .char_indices()
                    .nth(200)
                    .map(|(i, _)| i)
                    .unwrap_or(trimmed.len());
                format!("{}…", &trimmed[..end])
            } else {
                trimmed.to_string()
            }
        })
}


/// Pull the text between `<command-args>` and the *following* `</command-args>`.
/// Returns `None` when either tag is missing or the closing tag only appears
/// before the opening one (malformed/truncated content must not panic).
fn extract_command_args(content: &str) -> Option<&str> {
    const OPEN: &str = "<command-args>";
    const CLOSE: &str = "</command-args>";
    let start = content.find(OPEN)? + OPEN.len();
    let end_rel = content[start..].find(CLOSE)?;
    Some(&content[start..start + end_rel])
}

fn push_search_text(out: &mut String, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    out.push_str(&trimmed.to_lowercase());
    out.push('\n');
}

/// Accumulate every piece of human-readable text in an entry into the search
/// text: user messages (string or block form), assistant text and thinking,
/// tool-use string inputs (commands, paths, …), and tool-result output.
/// Command noise is filtered; images and JSON structure are not indexed.
fn append_searchable_text(entry: &Value, out: &mut String) {
    let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    match content {
        Value::String(s) => {
            if !is_command_noise(s) {
                push_search_text(out, s);
            }
        }
        Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            if !is_command_noise(t) {
                                push_search_text(out, t);
                            }
                        }
                    }
                    Some("thinking") => {
                        if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                            push_search_text(out, t);
                        }
                    }
                    Some("tool_use") => {
                        if let Some(input) = block.get("input").and_then(|i| i.as_object()) {
                            for value in input.values() {
                                if let Some(s) = value.as_str() {
                                    push_search_text(out, s);
                                }
                            }
                        }
                    }
                    Some("tool_result") => match block.get("content") {
                        Some(Value::String(s)) => push_search_text(out, s),
                        Some(Value::Array(parts)) => {
                            for part in parts {
                                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                    push_search_text(out, t);
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

pub struct ScanResult {
    pub head: HeadMeta,
    pub tail: Option<TailMeta>,
    /// Lowercased searchable text: user messages, assistant text + thinking,
    /// tool-use string inputs, and tool-result text — everything a person
    /// could have read in the session, minus JSON structure and images.
    pub search_text_lc: String,
    pub tickets: Vec<String>,
    /// Claude-generated session title (`type: "ai-title"`), if present.
    pub ai_title: String,
    /// Last `permissionMode` seen (e.g. "plan", "acceptEdits", "bypassPermissions").
    pub permission_mode: String,
    /// Claude Code `version` field (first non-empty seen).
    pub cc_version: String,
    /// Distinct `attributionSkill` values, sorted.
    pub skills: Vec<String>,
    /// Sorted union of `trackedFileBackups` paths across file-history snapshots.
    pub changed_files: Vec<String>,
    /// Count of non-empty human messages.
    pub message_count: u32,
}

pub fn scan_session(path: &Path) -> Option<ScanResult> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut head: Option<HeadMeta> = None;
    let mut working_head = HeadMeta {
        title: String::new(),
        branch: String::new(),
        slug: String::new(),
        first_timestamp: String::new(),
        cwd: String::new(),
    };
    let mut last_human_message = String::new();
    let mut last_timestamp = String::new();
    let mut rename = String::new();
    let mut fallback_title = String::new();
    let mut search_text_lc = String::new();
    let mut tickets_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ai_title = String::new();
    let mut permission_mode = String::new();
    let mut cc_version = String::new();
    let mut skills_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed_files_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut message_count: u32 = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        extract_tickets_into(&line, &mut tickets_set);

        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        append_searchable_text(&entry, &mut search_text_lc);

        // Modern-format metadata, harvested in the same single pass.
        match entry.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => {
                if let Some(t) = entry.get("aiTitle").and_then(|t| t.as_str()) {
                    ai_title = t.to_string();
                }
            }
            Some("permission-mode") => {
                if let Some(m) = entry.get("permissionMode").and_then(|m| m.as_str()) {
                    permission_mode = m.to_string();
                }
            }
            Some("file-history-snapshot") => {
                if let Some(backups) = entry
                    .get("snapshot")
                    .and_then(|s| s.get("trackedFileBackups"))
                    .and_then(|b| b.as_object())
                {
                    for path in backups.keys() {
                        changed_files_set.insert(path.clone());
                    }
                }
            }
            _ => {}
        }
        if cc_version.is_empty() {
            if let Some(v) = entry.get("version").and_then(|v| v.as_str()) {
                cc_version = v.to_string();
            }
        }
        if let Some(skill) = entry.get("attributionSkill").and_then(|s| s.as_str()) {
            if !skill.is_empty() {
                skills_set.insert(skill.to_string());
            }
        }

        if working_head.branch.is_empty() {
            if let Some(b) = entry.get("gitBranch").and_then(|b| b.as_str()) {
                working_head.branch = b.to_string();
            }
        }
        if working_head.slug.is_empty() {
            if let Some(s) = entry.get("slug").and_then(|s| s.as_str()) {
                working_head.slug = s.to_string();
            }
        }
        if working_head.first_timestamp.is_empty() {
            if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
                working_head.first_timestamp = ts.to_string();
            }
        }
        if working_head.cwd.is_empty() {
            if let Some(c) = entry.get("cwd").and_then(|c| c.as_str()) {
                working_head.cwd = c.to_string();
            }
        }

        if let Some(ts) = entry.get("timestamp").and_then(|t| t.as_str()) {
            last_timestamp = ts.to_string();
        }

        // Remember the first command-noise turn so command-only sessions
        // (someone opened Claude and ran /exit) still get a listable title.
        if fallback_title.is_empty()
            && entry.get("type").and_then(|t| t.as_str()) == Some("user")
        {
            if let Some(c) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if is_command_noise(c) {
                    fallback_title = command_noise_title(c);
                }
            }
        }

        if is_human_message(&entry) {
            if let Some(full) = entry
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                let trimmed = full.trim();
                if !trimmed.is_empty() {
                    // Count only non-sidechain turns, to match the conversation
                    // preview (which skips sidechains) and the `· N msgs` label.
                    if entry.get("isSidechain").and_then(|s| s.as_bool()) != Some(true) {
                        message_count += 1;
                    }
                    if head.is_none() {
                        let title = human_message_text(&entry).unwrap_or_default();
                        head = Some(HeadMeta {
                            title,
                            branch: working_head.branch.clone(),
                            slug: working_head.slug.clone(),
                            first_timestamp: working_head.first_timestamp.clone(),
                            cwd: working_head.cwd.clone(),
                        });
                    }
                    if let Some(text) = human_message_text(&entry) {
                        last_human_message = text;
                    }
                }
            }
        }

        if entry.get("subtype").and_then(|s| s.as_str()) == Some("local_command") {
            if let Some(content) = entry.get("content").and_then(|c| c.as_str()) {
                if content.contains("<command-name>/rename</command-name>") {
                    if let Some(args) = extract_command_args(content) {
                        rename = args.to_string();
                    }
                }
            }
        }
    }

    let head = match head {
        Some(h) => h,
        None if !fallback_title.is_empty() => HeadMeta {
            title: fallback_title,
            branch: working_head.branch.clone(),
            slug: working_head.slug.clone(),
            first_timestamp: working_head.first_timestamp.clone(),
            cwd: working_head.cwd.clone(),
        },
        None => return None,
    };
    let tail = if last_human_message.is_empty() && last_timestamp.is_empty() && rename.is_empty() {
        None
    } else {
        Some(TailMeta {
            last_human_message,
            last_timestamp,
            rename,
        })
    };

    let mut tickets: Vec<String> = tickets_set.into_iter().collect();
    tickets.sort();
    let mut skills: Vec<String> = skills_set.into_iter().collect();
    skills.sort();
    let mut changed_files: Vec<String> = changed_files_set.into_iter().collect();
    changed_files.sort();

    Some(ScanResult {
        head,
        tail,
        search_text_lc,
        tickets,
        ai_title,
        permission_mode,
        cc_version,
        skills,
        changed_files,
        message_count,
    })
}

/// User + assistant text messages, with tool use filtered out. Used by export.
pub fn extract_conversation(path: &Path) -> Vec<ConversationMessage> {
    extract_conversation_ext(path, false)
        .into_iter()
        .map(|(speaker, text)| ConversationMessage {
            role: match speaker {
                Speaker::User => Role::User,
                _ => Role::Assistant,
            },
            text,
        })
        .collect()
}

/// Extract the conversation as `(speaker, text)` pairs. When `include_tools` is
/// true, assistant `tool_use` blocks are surfaced as `Speaker::Tool` lines with
/// a compact summary (e.g. "Edit src/auth.rs", "Bash cargo test").
pub fn extract_conversation_ext(path: &Path, include_tools: bool) -> Vec<(Speaker, String)> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if entry.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
            continue;
        }

        let entry_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match entry_type {
            "user" if is_human_message(&entry) => {
                if let Some(text) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        messages.push((Speaker::User, trimmed.to_string()));
                    }
                }
            }
            "assistant" => {
                if let Some(content) = entry
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    let trimmed = text.trim();
                                    if !trimmed.is_empty() {
                                        messages.push((Speaker::Assistant, trimmed.to_string()));
                                    }
                                }
                            }
                            Some("tool_use") if include_tools => {
                                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                messages.push((Speaker::Tool, summarize_tool_use(name, block.get("input"))));
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    messages
}

/// Build a one-line summary of a tool call: the tool name plus its most telling
/// argument (file path, command, pattern, …), truncated.
fn summarize_tool_use(name: &str, input: Option<&Value>) -> String {
    let detail = input
        .and_then(|inp| {
            ["file_path", "path", "command", "pattern", "query", "url", "description"]
                .iter()
                .find_map(|k| inp.get(*k).and_then(|v| v.as_str()))
        })
        .unwrap_or("")
        .trim()
        .replace('\n', " ");
    if detail.is_empty() {
        name.to_string()
    } else {
        let detail: String = detail.chars().take(120).collect();
        format!("{} {}", name, detail)
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
    fn test_extract_conversation_filters_correctly() {
        let messages = extract_conversation(&fixture_path("simple_session.jsonl"));
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].text, "build a cool thing");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].text, "Sure, let me help you build that.");
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].text, "looks good, ship it");
        assert_eq!(messages[3].role, Role::Assistant);
        assert_eq!(messages[3].text, "Done! Everything is deployed.");
    }

    #[test]
    fn test_extract_with_tools_includes_tool_lines() {
        let with = extract_conversation_ext(&fixture_path("complex_session.jsonl"), true);
        let without = extract_conversation_ext(&fixture_path("complex_session.jsonl"), false);
        assert!(
            with.iter().any(|(sp, _)| *sp == Speaker::Tool),
            "include_tools=true should surface a Tool line"
        );
        assert!(
            !without.iter().any(|(sp, _)| *sp == Speaker::Tool),
            "include_tools=false must not surface Tool lines"
        );
    }

    #[test]
    fn test_extract_conversation_skips_sidechain() {
        let messages = extract_conversation(&fixture_path("complex_session.jsonl"));
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        assert!(!texts.contains(&"This is a sidechain message."));
    }

    #[test]
    fn test_extract_conversation_skips_meta_user() {
        let messages = extract_conversation(&fixture_path("complex_session.jsonl"));
        let texts: Vec<&str> = messages.iter().map(|m| m.text.as_str()).collect();
        assert!(!texts.contains(&"skill loaded: auth-helper"));
        assert!(!texts.iter().any(|t| t.contains("local-command-caveat")));
    }

    #[test]
    fn test_extract_tickets_positive() {
        let mut out = std::collections::HashSet::new();
        extract_tickets_into("see PROJ-123 and ABC-78 please", &mut out);
        assert!(out.contains("PROJ-123"));
        assert!(out.contains("ABC-78"));
    }

    #[test]
    fn test_extract_tickets_hash_form() {
        let mut out = std::collections::HashSet::new();
        extract_tickets_into("fixes #456 and refs #7", &mut out);
        assert!(out.contains("#456"));
        assert!(out.contains("#7"));
    }

    #[test]
    fn test_extract_tickets_negative() {
        let mut out = std::collections::HashSet::new();
        extract_tickets_into("lowercase-99 and A-99 and proj-123", &mut out);
        assert!(out.is_empty(), "got: {:?}", out);
    }

    #[test]
    fn test_extract_tickets_dedupes() {
        let mut out = std::collections::HashSet::new();
        extract_tickets_into("PROJ-1 PROJ-1 PROJ-1", &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_extract_tickets_word_boundaries() {
        let mut out = std::collections::HashSet::new();
        extract_tickets_into("xPROJ-1y and (PROJ-2)", &mut out);
        assert!(!out.contains("PROJ-1"));
        assert!(out.contains("PROJ-2"));
    }

    #[test]
    fn test_scan_session_simple() {
        let result = scan_session(&fixture_path("simple_session.jsonl"));
        let result = result.expect("should scan");
        assert_eq!(result.head.title, "build a cool thing");
        assert_eq!(result.head.branch, "main");
        let tail = result.tail.expect("should have tail");
        assert_eq!(tail.last_human_message, "looks good, ship it");
        assert!(
            result.search_text_lc.contains("build a cool thing"),
            "got: {:?}",
            result.search_text_lc
        );
        assert!(
            result.search_text_lc.contains("looks good, ship it"),
            "got: {:?}",
            result.search_text_lc
        );
        assert!(
            result.search_text_lc.chars().all(|c: char| !c.is_uppercase()),
            "should be lowercased"
        );
    }

    #[test]
    fn test_scan_session_empty_returns_none() {
        let result = scan_session(&fixture_path("empty_session.jsonl"));
        assert!(result.is_none());
    }

    #[test]
    fn test_scan_session_extracts_tickets() {
        let result = scan_session(&fixture_path("session_with_tickets.jsonl"));
        let result = result.expect("should scan");
        assert!(result.tickets.contains(&"PROJ-123".to_string()), "got {:?}", result.tickets);
        assert!(result.tickets.contains(&"ABC-78".to_string()), "got {:?}", result.tickets);
        assert!(result.tickets.contains(&"#456".to_string()), "got {:?}", result.tickets);
        // sorted + deduped
        let mut sorted = result.tickets.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, result.tickets);
    }

    #[test]
    fn test_extract_command_args_well_formed() {
        assert_eq!(
            extract_command_args("<command-args>new name</command-args>"),
            Some("new name")
        );
    }

    #[test]
    fn test_extract_command_args_close_before_open_no_panic() {
        // A closing tag before the opening tag used to slice with start > end.
        let content = "<command-name>/rename</command-name></command-args>junk<command-args>";
        assert_eq!(extract_command_args(content), None);
    }

    #[test]
    fn test_extract_command_args_missing_close() {
        assert_eq!(extract_command_args("<command-args>truncated"), None);
    }

    #[test]
    fn test_scan_session_malformed_rename_line_no_panic() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Valid human message so the scan yields a result…
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"hello"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        // …then a local_command whose </command-args> precedes <command-args>.
        writeln!(
            f,
            r#"{{"type":"user","subtype":"local_command","content":"<command-name>/rename</command-name></command-args>junk<command-args>","toolUseResult":{{}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should still scan");
        assert_eq!(result.head.title, "hello");
        let tail = result.tail.expect("tail");
        assert_eq!(tail.rename, "", "malformed rename must be ignored");
    }

    #[test]
    fn test_scan_session_non_utf8_and_truncated_lines_no_panic() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("garbage.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Invalid UTF-8 bytes, truncated JSON, unknown types with odd shapes.
        f.write_all(&[0xff, 0xfe, 0x80, b'\n']).unwrap();
        f.write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"tr\n").unwrap();
        f.write_all(b"{\"type\":\"future-thing\",\"message\":42}\n").unwrap();
        f.write_all(b"{\"type\":\"user\",\"message\":{\"content\":[\"array form\"]}}\n").unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"survivor"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan despite garbage");
        assert_eq!(result.head.title, "survivor");
        assert_eq!(result.message_count, 1);
    }

    #[test]
    fn test_command_noise_skipped_for_title_and_count() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("noisy.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        // Slash command + its output arrive before the real first message.
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<command-name>/model</command-name><command-args></command-args>"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<local-command-stdout>Set model to opus</local-command-stdout>"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"fix the login bug"}},"timestamp":"2026-01-01T00:01:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<task-notification><task-id>x1</task-id>done</task-notification>"}},"timestamp":"2026-01-01T00:02:00Z"}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan");
        assert_eq!(result.head.title, "fix the login bug");
        assert_eq!(result.message_count, 1, "noise turns must not count");
        let tail = result.tail.expect("tail");
        assert_eq!(
            tail.last_human_message, "fix the login bug",
            "task notification must not become the left-off line"
        );
        assert!(
            !result.search_text_lc.contains("command-name"),
            "noise must not enter the search text"
        );
    }

    #[test]
    fn test_command_only_session_gets_fallback_title() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cmd_only.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<command-name>/exit</command-name>"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"<local-command-stdout>Bye!</local-command-stdout>"}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("command-only session must stay listable");
        assert_eq!(result.head.title, "/exit");
        assert_eq!(result.message_count, 0);
    }

    #[test]
    fn test_search_text_includes_assistant_text() {
        let result = scan_session(&fixture_path("complex_session.jsonl")).expect("should scan");
        assert!(
            result.search_text_lc.contains("i'll set up jwt auth."),
            "assistant text must be searchable, got: {:?}",
            result.search_text_lc
        );
    }

    #[test]
    fn test_search_text_includes_array_form_user_text() {
        let result = scan_session(&fixture_path("complex_session.jsonl")).expect("should scan");
        assert!(
            result.search_text_lc.contains("skill loaded: auth-helper"),
            "array-form user text must be searchable, got: {:?}",
            result.search_text_lc
        );
    }

    #[test]
    fn test_search_text_includes_tool_results_inputs_and_thinking() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("deep.jsonl");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":"start"}},"timestamp":"2026-01-01T00:00:00Z"}}"#
        )
        .unwrap();
        // Assistant turn: thinking + tool_use with string inputs.
        writeln!(
            f,
            r#"{{"type":"assistant","message":{{"content":[{{"type":"thinking","thinking":"maybe the flag is inverted"}},{{"type":"tool_use","id":"t1","name":"Bash","input":{{"command":"cargo build --release"}}}}]}}}}"#
        )
        .unwrap();
        // Tool result in string form…
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"error[E0502]: cannot borrow"}}]}},"toolUseResult":{{}}}}"#
        )
        .unwrap();
        // …and in block-array form.
        writeln!(
            f,
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t2","content":[{{"type":"text","text":"warning: unused variable `zeta`"}}]}}]}},"toolUseResult":{{}}}}"#
        )
        .unwrap();
        let result = scan_session(&path).expect("should scan");
        for needle in [
            "maybe the flag is inverted",
            "cargo build --release",
            "error[e0502]: cannot borrow",
            "warning: unused variable `zeta`",
        ] {
            assert!(
                result.search_text_lc.contains(needle),
                "search text must contain {:?}, got: {:?}",
                needle,
                result.search_text_lc
            );
        }
        // Deep content must not affect the human message count.
        assert_eq!(result.message_count, 1);
    }

    #[test]
    fn test_extract_conversation_missing_file_returns_empty() {
        let messages = extract_conversation(Path::new("/does/not/exist.jsonl"));
        assert!(messages.is_empty());
    }

    #[test]
    fn test_scan_modern_extracts_metadata() {
        let result = scan_session(&fixture_path("modern_session.jsonl")).expect("should scan");
        assert_eq!(result.ai_title, "Add JWT login endpoint");
        assert_eq!(result.permission_mode, "plan");
        assert_eq!(result.cc_version, "2.1.0");
        assert!(
            result.skills.contains(&"brainstorming".to_string()),
            "skills: {:?}",
            result.skills
        );
        assert!(
            result.skills.contains(&"test-driven-development".to_string()),
            "skills: {:?}",
            result.skills
        );
        // sorted union of trackedFileBackups keys
        assert_eq!(
            result.changed_files,
            vec!["src/auth.rs".to_string(), "src/lib.rs".to_string()]
        );
        // two human messages: "add login endpoint" and "ship it"
        assert_eq!(result.message_count, 2);
    }
}
