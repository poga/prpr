//! User config. Everything is optional; missing keys fall back to defaults.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Theme,
    pub window_size: usize,
    pub show_commit_strip: bool,
    pub show_sha_margin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Mocha,
    Latte,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::Mocha,
            window_size: default_window_size(),
            show_commit_strip: true,
            show_sha_margin: false,
        }
    }
}

pub fn default_window_size() -> usize {
    7
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    colors: RawColors,
    #[serde(default)]
    commit_attribution: RawCommit,
    #[serde(default)]
    ui: RawUi,
}
#[derive(Debug, Default, Deserialize)]
struct RawColors {
    #[serde(default)]
    theme: Option<Theme>,
}
#[derive(Debug, Default, Deserialize)]
struct RawCommit {
    #[serde(default)]
    window_size: Option<usize>,
}
#[derive(Debug, Default, Deserialize)]
struct RawUi {
    #[serde(default)]
    show_commit_strip: Option<bool>,
    #[serde(default)]
    show_sha_margin: Option<bool>,
}

/// Locate the config file path. Returns `None` if no XDG config dir is
/// resolvable (very rare).
pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "pprr")
        .map(|d| d.config_dir().join("config.toml"))
}

/// Load the config from `config_path()`, merging with defaults. If the file
/// doesn't exist, returns `Config::default()`.
pub fn load() -> Result<Config> {
    let Some(path) = config_path() else {
        return Ok(Config::default());
    };
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(merge(Config::default(), raw))
}

fn merge(mut cfg: Config, raw: RawConfig) -> Config {
    if let Some(t) = raw.colors.theme {
        cfg.theme = t;
    }
    if let Some(n) = raw.commit_attribution.window_size {
        cfg.window_size = n;
    }
    if let Some(b) = raw.ui.show_commit_strip {
        cfg.show_commit_strip = b;
    }
    if let Some(b) = raw.ui.show_sha_margin {
        cfg.show_sha_margin = b;
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_full_toml() {
        let toml = r#"
            [colors]
            theme = "latte"
            [commit_attribution]
            window_size = 5
            [ui]
            show_commit_strip = false
            show_sha_margin = true
        "#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = merge(Config::default(), raw);
        assert_eq!(cfg.theme, Theme::Latte);
        assert_eq!(cfg.window_size, 5);
        assert_eq!(cfg.show_commit_strip, false);
        assert_eq!(cfg.show_sha_margin, true);
    }

    #[test]
    fn empty_toml_yields_defaults() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(merge(Config::default(), raw), Config::default());
    }

    #[test]
    fn partial_toml_only_overrides_present_keys() {
        let toml = "[commit_attribution]\nwindow_size = 3";
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = merge(Config::default(), raw);
        assert_eq!(cfg.window_size, 3);
        assert_eq!(cfg.theme, Theme::Mocha); // unchanged
    }
}
