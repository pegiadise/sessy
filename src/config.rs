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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scope: "current".to_string(),
            sort: "date".to_string(),
            show_tool_activity: false,
        }
    }
}

impl Config {
    pub fn scope_is_all(&self) -> bool {
        self.scope.eq_ignore_ascii_case("all")
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

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sessy")
        .join("config.toml")
}

/// Load config from the standard path, or defaults if absent/unreadable.
pub fn load() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => parse_config(&s),
        Err(_) => Config::default(),
    }
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
    }

    #[test]
    fn test_config_parse() {
        let c = parse_config("scope = \"all\"\nsort = \"messages\"\nshow_tool_activity = true\n");
        assert!(c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Messages);
        assert!(c.show_tool_activity);
    }

    #[test]
    fn test_config_malformed_falls_back_to_defaults() {
        let c = parse_config("this is not ]][[ valid toml");
        assert!(!c.scope_is_all());
        assert_eq!(c.sort_mode(), SortMode::Date);
    }
}

