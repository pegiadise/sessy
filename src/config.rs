use crate::app::{Density, SortMode};
use std::path::PathBuf;

/// User configuration loaded from `~/.config/sessy/config.toml`. Every field is
/// optional; a missing file yields all defaults (see `load` for a bad one).
#[derive(Debug)]
pub struct Config {
    /// "current" (launch directory only) or "all" (every project).
    pub scope: String,
    /// "date", "size", "duration", or "messages".
    pub sort: String,
    /// Start with tool-use activity shown in the preview.
    pub show_tool_activity: bool,
    /// What Enter does: "yolo" (resume with --dangerously-skip-permissions)
    /// or "safe" (plain resume).
    pub enter: String,
    /// "comfortable" (three lines per session) or "compact" (one line).
    pub density: String,
    /// Mouse support: wheel scrolling, click to select, double-click to
    /// resume. Off by default: capturing the mouse stops the terminal's own
    /// text selection (most terminals still select with Shift or Option held).
    pub mouse: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scope: "current".to_string(),
            sort: "date".to_string(),
            show_tool_activity: false,
            enter: "yolo".to_string(),
            density: "comfortable".to_string(),
            mouse: false,
        }
    }
}

impl Config {
    pub fn scope_is_all(&self) -> bool {
        self.scope.eq_ignore_ascii_case("all")
    }

    /// Enter skips permission prompts only when the config says exactly
    /// that (the default); a typo means the safe resume.
    pub fn enter_is_yolo(&self) -> bool {
        self.enter.trim().eq_ignore_ascii_case("yolo")
    }

    /// What in the file sessy doesn't understand: unknown keys (`table`
    /// is the parsed file) and values outside the documented ones.
    fn problems(&self, table: &toml::Table) -> Vec<String> {
        const KEYS: &[&str] =
            &["scope", "sort", "show_tool_activity", "enter", "density", "mouse"];
        let mut out: Vec<String> = table
            .keys()
            .filter(|k| !KEYS.contains(&k.as_str()))
            .map(|k| format!("unknown key `{}`", k))
            .collect();
        let check = |key: &str, value: &str, allowed: &[&str], out: &mut Vec<String>| {
            if table.contains_key(key) && !allowed.iter().any(|a| value.trim().eq_ignore_ascii_case(a)) {
                out.push(format!("{} = \"{}\" isn't {}", key, value, allowed.join(" or ")));
            }
        };
        check("scope", &self.scope, &["current", "all"], &mut out);
        check("sort", &self.sort, &["date", "size", "duration", "messages"], &mut out);
        check("enter", &self.enter, &["yolo", "safe"], &mut out);
        check("density", &self.density, &["comfortable", "compact"], &mut out);
        out
    }

    pub fn density(&self) -> Density {
        if self.density.eq_ignore_ascii_case("compact") {
            Density::Compact
        } else {
            Density::Comfortable
        }
    }

    pub fn sort_mode(&self) -> SortMode {
        match self.sort.to_lowercase().as_str() {
            "size" => SortMode::Size,
            "duration" => SortMode::Duration,
            "messages" => SortMode::Messages,
            _ => SortMode::Date,
        }
    }
}

impl Config {
    /// The defaults, but with Enter resuming safely: for a config file that
    /// can't be read, where an `enter = "safe"` may be what's lost.
    fn safe_defaults() -> Self {
        Self { enter: "safe".to_string(), ..Self::default() }
    }
}

/// Parse a config file's text, key by key: a value of the wrong type is
/// reported and left at its default instead of discarding the whole file
/// (`enter = "safe"` with it). Text that isn't TOML at all gives the error.
fn parse(text: &str) -> Result<(Config, Vec<String>), toml::de::Error> {
    let table = toml::from_str::<toml::Table>(text)?;
    let mut cfg = Config::default();
    let mut problems = Vec::new();
    let take_str = |key: &str, slot: &mut String, problems: &mut Vec<String>| match table.get(key) {
        Some(toml::Value::String(s)) => *slot = s.clone(),
        Some(other) => problems.push(format!("{} = {} isn't a quoted word", key, other)),
        None => {}
    };
    take_str("scope", &mut cfg.scope, &mut problems);
    take_str("sort", &mut cfg.sort, &mut problems);
    take_str("density", &mut cfg.density, &mut problems);
    take_str("enter", &mut cfg.enter, &mut problems);
    if table.get("enter").is_some_and(|v| !v.is_str()) {
        // Unreadable: it may have meant "safe".
        cfg.enter = "safe".to_string();
    }
    let take_bool = |key: &str, slot: &mut bool, problems: &mut Vec<String>| match table.get(key) {
        Some(toml::Value::Boolean(b)) => *slot = *b,
        Some(other) => problems.push(format!("{} = {} isn't true or false", key, other)),
        None => {}
    };
    take_bool("show_tool_activity", &mut cfg.show_tool_activity, &mut problems);
    take_bool("mouse", &mut cfg.mouse, &mut problems);
    problems.extend(cfg.problems(&table));
    Ok((cfg, problems))
}

/// Parse config from a TOML string. Text that isn't TOML yields the
/// defaults with the safe Enter.
pub fn parse_config(s: &str) -> Config {
    parse(s).map(|(cfg, _)| cfg).unwrap_or_else(|_| Config::safe_defaults())
}

/// Where the config may live, in priority order: `$XDG_CONFIG_HOME/sessy`,
/// `~/.config/sessy` (the documented location, on every OS), then the
/// platform config dir (`~/Library/Application Support/sessy` on macOS).
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        paths.push(PathBuf::from(xdg).join("sessy").join("config.toml"));
    }
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config").join("sessy").join("config.toml"));
    }
    if let Some(dir) = dirs::config_dir() {
        paths.push(dir.join("sessy").join("config.toml"));
    }
    paths.dedup();
    paths
}

/// Load the first config file found. A file that exists but can't be read
/// or parsed yields defaults plus a message saying why, so a typo isn't
/// silently ignored, and Enter resumes safely until it's fixed: the file
/// may have said `enter = "safe"`.
pub fn load() -> (Config, Option<String>) {
    for path in config_paths() {
        let shown = match dirs::home_dir() {
            Some(home) => match path.strip_prefix(&home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => path.display().to_string(),
            },
            None => path.display().to_string(),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                let warning = format!("Config ignored, Enter resumes safely — can't read {}: {}", shown, e);
                return (Config::safe_defaults(), Some(warning));
            }
        };
        return match parse(&text) {
            Ok((cfg, problems)) => {
                let warning = (!problems.is_empty())
                    .then(|| format!("Config: {} in {}", problems.join(", "), shown));
                (cfg, warning)
            }
            Err(e) => {
                let line = e
                    .span()
                    .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
                let at = line.map(|l| format!(" (line {})", l)).unwrap_or_default();
                (
                    Config::safe_defaults(),
                    Some(format!(
                        "Config ignored, Enter resumes safely — {}{} in {}",
                        e.message().trim(),
                        at,
                        shown
                    )),
                )
            }
        };
    }
    (Config::default(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let c = parse_config("");
        assert!(!c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Date);
        assert!(!c.show_tool_activity);
        assert!(c.enter_is_yolo());
        assert_eq!(c.density(), Density::Comfortable);
    }

    #[test]
    fn test_config_parse() {
        let c = parse_config(
            "scope = \"all\"\nsort = \"messages\"\nshow_tool_activity = true\n\
             enter = \"safe\"\ndensity = \"compact\"\n",
        );
        assert!(c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Messages);
        assert!(c.show_tool_activity);
        assert!(!c.enter_is_yolo());
        assert_eq!(c.density(), Density::Compact);
    }

    fn problems(text: &str) -> Vec<String> {
        parse(text).unwrap().1
    }

    #[test]
    fn test_only_yolo_means_yolo() {
        for typo in ["sfae", "Safe ", "off", "no", "resume"] {
            let c = parse_config(&format!("enter = \"{}\"", typo));
            assert!(!c.enter_is_yolo(), "{:?} must not skip permissions", typo);
        }
        assert!(parse_config("enter = \" YOLO\"").enter_is_yolo());
    }

    #[test]
    fn test_unknown_keys_and_values_are_reported() {
        assert!(problems("enter = \"safe\"\nscope = \"all\"").is_empty());
        assert_eq!(
            problems("Enter = \"safe\"\nsort = \"newest\"\nenter = \"sfae\""),
            [
                "unknown key `Enter`",
                "sort = \"newest\" isn't date or size or duration or messages",
                "enter = \"sfae\" isn't yolo or safe",
            ]
        );
    }

    #[test]
    fn test_config_malformed_falls_back_to_defaults_with_the_safe_enter() {
        let c = parse_config("this is not ]][[ valid toml");
        assert!(!c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Date);
        assert!(!c.enter_is_yolo(), "it may have said enter = \"safe\"");
        // Bare words aren't TOML: the whole file is unreadable.
        assert!(!parse_config("enter = \"safe\"\nmouse = yes\n").enter_is_yolo());
    }

    #[test]
    fn test_a_wrong_type_drops_only_its_own_key() {
        let (c, problems) = parse(
            "enter = \"safe\"\nshow_tool_activity = \"true\"\nscope = \"all\"\nmouse = 1\n",
        )
        .unwrap();
        assert!(!c.enter_is_yolo(), "enter = \"safe\" is kept");
        assert!(c.scope_is_all());
        assert!(!c.show_tool_activity && !c.mouse);
        assert_eq!(
            problems,
            ["show_tool_activity = \"true\" isn't true or false", "mouse = 1 isn't true or false"]
        );
        let (c, problems) = parse("enter = 0\n").unwrap();
        assert!(!c.enter_is_yolo(), "an enter that isn't a word may have meant safe");
        assert_eq!(problems, ["enter = 0 isn't a quoted word"]);
    }
}

