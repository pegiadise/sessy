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
/// activity is requested; `Recap` is Claude Code's "while you were away"
/// summary, pinned to the top of the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    User,
    Assistant,
    Tool,
    Recap,
}

impl Speaker {
    /// Render prefix shown before the message text.
    pub fn prefix(self) -> &'static str {
        match self {
            Speaker::User => "USER: ",
            Speaker::Assistant => "ASST: ",
            Speaker::Tool => "TOOL: ",
            Speaker::Recap => "RECAP ",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversationMessage {
    pub role: Role,
    pub text: String,
}

/// A pull request Claude Code linked to the session (`type: "pr-link"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrLink {
    pub repo: String,
    pub number: u64,
    pub url: String,
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

fn flag(entry: &Value, key: &str) -> bool {
    entry.get(key).and_then(|v| v.as_bool()) == Some(true)
}

/// Text of a turn the person actually typed, or `None` for everything else
/// stored as `type:"user"`: tool results, injected/meta turns, compaction
/// summaries, messages from other sessions or background tasks, interrupt
/// markers, and command noise. Handles both content forms — a plain string,
/// and a block array (text + pasted images). A slash command typed with
/// arguments counts as a human turn ("/loop fix the tests").
pub fn human_text(entry: &Value) -> Option<String> {
    if entry.get("type").and_then(|t| t.as_str()) != Some("user")
        || flag(entry, "isMeta")
        || flag(entry, "isCompactSummary")
        || flag(entry, "isVisibleInTranscriptOnly")
        || entry.get("toolUseResult").is_some()
    {
        return None;
    }
    // Newer Claude Code tags every turn with its origin; only "human" is typed.
    if let Some(kind) = entry
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(|k| k.as_str())
    {
        if kind != "human" {
            return None;
        }
    }
    let content = entry.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(blocks) => {
            let mut parts: Vec<&str> = Vec::new();
            let mut has_image = false;
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            parts.push(t);
                        }
                    }
                    Some("image") => has_image = true,
                    Some("tool_result") => return None,
                    _ => {}
                }
            }
            let joined = parts.join("\n").trim().to_string();
            if joined.is_empty() && has_image {
                "[Image]".to_string()
            } else {
                joined
            }
        }
        _ => return None,
    };
    if text.is_empty() || text.starts_with("[Request interrupted by user") {
        return None;
    }
    if is_command_noise(&text) {
        return command_invocation(&text);
    }
    Some(text)
}

/// Machine-generated content stored as `type:"user"` turns — slash-command
/// invocations, local command output, bash-mode I/O, background-task
/// notifications. These must not drive titles, "left off", message counts,
/// search text, or preview lines (slash commands with arguments excepted, see
/// `command_invocation`).
fn is_command_noise(text: &str) -> bool {
    const NOISE: &[&str] = &[
        "<command-name>",
        "<command-message>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<task-notification>",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-memory-input>",
        "<user-prompt-submit-hook>",
    ];
    let t = text.trim_start();
    NOISE.iter().any(|p| t.starts_with(p))
}

/// Content between `<tag>` and the *following* `</tag>`. Returns `None` when
/// either tag is missing or the closing tag only appears before the opening
/// one (malformed/truncated content must not panic).
fn extract_tag<'a>(content: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)? + open.len();
    let end_rel = content[start..].find(&close)?;
    Some(&content[start..start + end_rel])
}

fn extract_command_args(content: &str) -> Option<&str> {
    extract_tag(content, "command-args")
}

/// "/name args" for a slash command typed with arguments — that's what the
/// person wrote. `None` for argument-less commands (/clear, /model, …).
fn command_invocation(content: &str) -> Option<String> {
    let args = extract_command_args(content)?.trim();
    if args.is_empty() {
        return None;
    }
    let name = extract_tag(content, "command-name")?.trim();
    if name.is_empty() {
        return None;
    }
    Some(format!("{} {}", name, args))
}

/// Fallback headline for sessions containing only command noise: the slash
/// command that was run, or a generic label.
fn command_noise_title(content: &str) -> String {
    match extract_tag(content, "command-name").map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "(command output)".to_string(),
    }
}

/// Drop control characters (ANSI escapes in pasted logs, stray `\r`) that
/// would corrupt the terminal or shift columns. Tabs become spaces.
pub fn sanitize_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            // Skip CSI sequences whole (ESC [ … final byte) so no residue like
            // "[31m" is left behind.
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Collapse a message to a single display line for titles and "left off":
/// `<pasted_content …>` wrapper tags removed (their text kept), all whitespace
/// runs (including newlines) folded to one space, control characters dropped,
/// and capped at `max_chars` with an ellipsis.
pub fn one_line(text: &str, max_chars: usize) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("<pasted_content") {
        stripped.push_str(&rest[..pos]);
        stripped.push(' ');
        rest = match rest[pos..].find('>') {
            Some(end) => &rest[pos + end + 1..],
            None => "",
        };
    }
    stripped.push_str(rest);
    let stripped = stripped.replace("</pasted_content>", " ");

    let stripped = sanitize_line(&stripped.replace(['\n', '\t'], " "));

    let mut out = String::with_capacity(stripped.len().min(max_chars * 4));
    let mut count = 0;
    for word in stripped.split_whitespace() {
        if count > 0 {
            out.push(' ');
            count += 1;
        }
        for c in word.chars() {
            if count >= max_chars {
                let kept = out.trim_end().len();
                out.truncate(kept);
                out.push('…');
                return out;
            }
            out.push(c);
            count += 1;
        }
    }
    out
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
/// Command noise is filtered (a slash command's arguments are kept); images
/// and JSON structure are not indexed.
fn append_searchable_text(entry: &Value, out: &mut String) {
    let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    match content {
        Value::String(s) => {
            if !is_command_noise(s) {
                push_search_text(out, s);
            } else if let Some(cmd) = command_invocation(s) {
                push_search_text(out, &cmd);
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

/// Claude Code's "while you were away" recap text, without the settings hint
/// it appends.
fn recap_text(entry: &Value) -> Option<String> {
    if entry.get("type").and_then(|t| t.as_str()) != Some("system")
        || entry.get("subtype").and_then(|s| s.as_str()) != Some("away_summary")
    {
        return None;
    }
    let content = entry.get("content").and_then(|c| c.as_str())?.trim();
    let content = content
        .strip_suffix("(disable recaps in /config)")
        .unwrap_or(content)
        .trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
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
    /// Sorted union of tracked file paths across file-history snapshots and deltas.
    pub changed_files: Vec<String>,
    /// Count of human messages outside sidechains.
    pub message_count: u32,
    /// Linked pull requests, in the order they were first linked.
    pub prs: Vec<PrLink>,
    /// Latest "while you were away" recap, if any.
    pub recap: String,
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
    let mut custom_title = String::new();
    let mut legacy_rename = String::new();
    let mut fallback_title = String::new();
    let mut search_text_lc = String::new();
    let mut tickets_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ai_title = String::new();
    let mut permission_mode = String::new();
    let mut cc_version = String::new();
    let mut skills_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed_files_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut message_count: u32 = 0;
    let mut prs: Vec<PrLink> = Vec::new();
    let mut recap = String::new();

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
                    ai_title = one_line(t, 200);
                }
            }
            // `/rename` (and Claude Code's own naming) persists the title here.
            Some("custom-title") => {
                if let Some(t) = entry.get("customTitle").and_then(|t| t.as_str()) {
                    custom_title = one_line(t, 200);
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
            // Newer Claude Code records edits one file at a time.
            Some("file-history-delta") => {
                if let Some(p) = entry.get("trackingPath").and_then(|p| p.as_str()) {
                    changed_files_set.insert(p.to_string());
                }
            }
            Some("pr-link") => {
                let number = entry.get("prNumber").and_then(|n| n.as_u64());
                let url = entry.get("prUrl").and_then(|u| u.as_str()).unwrap_or("");
                if let Some(number) = number {
                    if !prs.iter().any(|p| p.number == number && p.url == url) {
                        prs.push(PrLink {
                            repo: entry
                                .get("prRepository")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string(),
                            number,
                            url: url.to_string(),
                        });
                    }
                    // `#123` finds the session through ticket search.
                    tickets_set.insert(format!("#{}", number));
                }
            }
            _ => {}
        }
        if let Some(r) = recap_text(&entry) {
            recap = one_line(&r, 1000);
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

        // Sidechain turns belong to subagents, not the person; the preview
        // skips them, so titles and counts must too.
        if !flag(&entry, "isSidechain") {
            if let Some(text) = human_text(&entry) {
                let line = one_line(&text, 200);
                message_count += 1;
                if head.is_none() {
                    head = Some(HeadMeta {
                        title: line.clone(),
                        branch: working_head.branch.clone(),
                        slug: working_head.slug.clone(),
                        first_timestamp: working_head.first_timestamp.clone(),
                        cwd: working_head.cwd.clone(),
                    });
                }
                last_human_message = line;
            }
        }

        // Pre-`custom-title` Claude Code only recorded /rename as a local
        // command. A bare `/rename` (auto-generated name) has empty args and
        // must not clear a name set earlier.
        if entry.get("subtype").and_then(|s| s.as_str()) == Some("local_command") {
            if let Some(content) = entry.get("content").and_then(|c| c.as_str()) {
                if content.contains("<command-name>/rename</command-name>") {
                    if let Some(args) = extract_command_args(content) {
                        let args = one_line(args, 200);
                        if !args.is_empty() {
                            legacy_rename = args;
                        }
                    }
                }
            }
        }
    }

    let rename = if custom_title.is_empty() {
        legacy_rename
    } else {
        custom_title
    };

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
        prs,
        recap,
    })
}

/// User + assistant text messages, with tool use filtered out. Used by export.
pub fn extract_conversation(path: &Path) -> Vec<ConversationMessage> {
    extract_conversation_ext(path, false)
        .into_iter()
        .filter_map(|(speaker, text)| {
            let role = match speaker {
                Speaker::User => Role::User,
                Speaker::Assistant => Role::Assistant,
                Speaker::Tool | Speaker::Recap => return None,
            };
            Some(ConversationMessage { role, text })
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

        if flag(&entry, "isSidechain") {
            continue;
        }

        match entry.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "user" => {
                if let Some(text) = human_text(&entry) {
                    messages.push((Speaker::User, text));
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
            [
                "file_path",
                "path",
                "command",
                "pattern",
                "query",
                "url",
                "skill",
                "description",
                "prompt",
            ]
            .iter()
            .find_map(|k| inp.get(*k).and_then(|v| v.as_str()))
        })
        .unwrap_or("");
    let detail = one_line(detail, 120);
    if detail.is_empty() {
        name.to_string()
    } else {
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
    fn test_scan_current_format() {
        let r = scan_session(&fixture_path("current_format_session.jsonl")).expect("should scan");
        // A slash command typed with arguments is what the person wrote.
        assert_eq!(r.head.title, "/loop fix the flaky tests");
        // /loop, the image message, the pasted message. Not: the meta
        // expansion, the interrupt marker, the compaction summary, the task
        // notification, or the message from another session.
        assert_eq!(r.message_count, 3);
        let tail = r.tail.expect("tail");
        assert_eq!(
            tail.last_human_message, "FAILED spec/cart_spec.rb:12 this one too",
            "pasted_content tags stripped, newlines folded"
        );
        // Latest custom-title wins; a bare /rename afterwards doesn't clear it.
        assert_eq!(tail.rename, "auth-rework");
        assert_eq!(r.ai_title, "Fix flaky specs");
        assert!(r.changed_files.contains(&"spec/auth_spec.rb".to_string()));
        assert_eq!(
            r.prs,
            vec![PrLink {
                repo: "acme/shop".into(),
                number: 42,
                url: "https://github.com/acme/shop/pull/42".into(),
            }]
        );
        assert!(r.tickets.contains(&"#42".to_string()), "PR number is ticket-searchable");
        assert_eq!(
            r.recap,
            "Both flaky specs are fixed and PR #42 is open. Next: merge after CI."
        );
        assert!(r.search_text_lc.contains("/loop fix the flaky tests"));
    }

    #[test]
    fn test_extract_current_format_conversation() {
        let lines = extract_conversation_ext(&fixture_path("current_format_session.jsonl"), false);
        let users: Vec<&str> = lines
            .iter()
            .filter(|(sp, _)| *sp == Speaker::User)
            .map(|(_, t)| t.as_str())
            .collect();
        assert_eq!(users.len(), 3, "got {:?}", users);
        assert_eq!(users[0], "/loop fix the flaky tests");
        assert_eq!(users[1], "why is this red? [Image #1]");
        assert!(!users.iter().any(|u| u.contains("being continued")));
        assert!(!users.iter().any(|u| u.contains("another session")));
        // Multi-line assistant text keeps its line breaks for the preview.
        assert!(lines.iter().any(|(sp, t)| *sp == Speaker::Assistant && t.contains("\n\n1. `auth_spec`")));
    }

    #[test]
    fn test_image_only_message_is_a_turn() {
        let entry: Value = serde_json::from_str(
            r#"{"type":"user","message":{"content":[{"type":"image","source":{}}]}}"#,
        )
        .unwrap();
        assert_eq!(human_text(&entry).as_deref(), Some("[Image]"));
    }

    #[test]
    fn test_sanitize_strips_ansi_and_controls() {
        assert_eq!(sanitize_line("\u{1b}[31mred\u{1b}[0m\r"), "red");
        assert_eq!(sanitize_line("a\tb"), "a    b");
        assert_eq!(sanitize_line("bell\u{7}"), "bell");
    }

    #[test]
    fn test_one_line_cleanup() {
        assert_eq!(one_line("a\n\n  b\tc", 200), "a b c");
        assert_eq!(one_line("abc def", 4), "abc…");
        assert_eq!(one_line("x\u{1b}[31my\u{1b}[0m", 200), "xy");
        assert_eq!(one_line("<pasted_content id=\"1\">log</pasted_content> fix", 200), "log fix");
        assert_eq!(one_line("<pasted_content id=\"1\" truncated", 200), "");
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
