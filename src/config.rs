use crate::app::{Density, SortMode};
use serde::Deserialize;
use std::path::PathBuf;

/// User configuration loaded from `~/.config/sessy/config.toml`. Every field is
/// optional; a missing or malformed file yields all defaults.
#[derive(Debug, Deserialize)]
#[serde(default)]
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

/// Parse config from a TOML string, falling back to defaults on any error.
pub fn parse_config(s: &str) -> Config {
    toml::from_str(s).unwrap_or_default()
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

/// Load the first config file found. A file that exists but doesn't parse
/// yields defaults plus a message saying why, so a typo isn't silently
/// ignored.
pub fn load() -> (Config, Option<String>) {
    for path in config_paths() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let shown = match dirs::home_dir() {
            Some(home) => match path.strip_prefix(&home) {
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => path.display().to_string(),
            },
            None => path.display().to_string(),
        };
        return match toml::from_str::<Config>(&text) {
            Ok(cfg) => {
                let problems = toml::from_str::<toml::Table>(&text)
                    .map(|table| cfg.problems(&table))
                    .unwrap_or_default();
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
                    Config::default(),
                    Some(format!(
                        "Config ignored — {}{} in {}",
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
        let cfg: Config = toml::from_str(text).unwrap();
        cfg.problems(&toml::from_str(text).unwrap())
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
    fn test_config_malformed_falls_back_to_defaults() {
        let c = parse_config("this is not ]][[ valid toml");
        assert!(!c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Date);
    }
}

