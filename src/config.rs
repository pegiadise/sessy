use crate::app::SortMode;
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scope: "current".to_string(),
            sort: "date".to_string(),
            show_tool_activity: false,
            enter: "yolo".to_string(),
        }
    }
}

impl Config {
    pub fn scope_is_all(&self) -> bool {
        self.scope.eq_ignore_ascii_case("all")
    }

    pub fn enter_is_yolo(&self) -> bool {
        !matches!(
            self.enter.to_lowercase().as_str(),
            "safe" | "resume" | "normal"
        )
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
        return match toml::from_str::<Config>(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => {
                let line = e
                    .span()
                    .map(|span| text[..span.start.min(text.len())].lines().count().max(1));
                let shown = match dirs::home_dir() {
                    Some(home) => match path.strip_prefix(&home) {
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => path.display().to_string(),
                    },
                    None => path.display().to_string(),
                };
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
    }

    #[test]
    fn test_config_parse() {
        let c = parse_config(
            "scope = \"all\"\nsort = \"messages\"\nshow_tool_activity = true\nenter = \"safe\"\n",
        );
        assert!(c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Messages);
        assert!(c.show_tool_activity);
        assert!(!c.enter_is_yolo());
    }

    #[test]
    fn test_config_malformed_falls_back_to_defaults() {
        let c = parse_config("this is not ]][[ valid toml");
        assert!(!c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Date);
    }
}

