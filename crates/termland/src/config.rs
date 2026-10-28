//! Configuration for termland.
//!
//! Loaded from `$HOME/.config/termland/config.toml`. Every field is optional;
//! missing values fall back to the compiled-in defaults below, so a first run
//! needs no config file at all.

use crate::layout::Dir;
use ratatui::style::Color;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// The default config path.
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".config/termland/config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// A single key binding: the key that triggers it and the action it runs.
#[derive(Clone, Debug, Deserialize)]
pub struct KeyBind {
    pub key: String,
    pub action: String,
}

/// One workspace definition: which apps to spawn and how to arrange them.
#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceDef {
    pub apps: Option<Vec<String>>,
    pub layout: Option<String>, // "horizontal" | "vertical" | "single"
}

/// The full config file.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub leader: Option<String>,
    pub mouse: Option<bool>,
    pub scrollback: Option<usize>,
    pub workspaces: Option<Vec<WorkspaceDef>>,
    pub keys: Option<Vec<KeyBind>>,
    pub colors: Option<Colors>,
}

/// Theme colors (all optional; missing ones keep the compiled default).
#[derive(Clone, Debug, Deserialize)]
pub struct Colors {
    pub bg: Option<String>,
    pub bg_alt: Option<String>,
    pub fg: Option<String>,
    pub fg_dim: Option<String>,
    pub accent: Option<String>,
    pub accent_fg: Option<String>,
    pub border: Option<String>,
    pub warn: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            leader: Some("ctrl-space".to_string()),
            mouse: Some(true),
            scrollback: Some(2000),
            workspaces: None,
            keys: None,
            colors: None,
        }
    }
}

impl Config {
    /// Load from disk, falling back to defaults if the file is missing or
    /// unparseable (a distro must start even with a broken config).
    pub fn load() -> Self {
        Self::load_reporting().0
    }

    /// Load, and also say *why* the file was rejected so the UI can surface
    /// it. A config error that only reaches stderr is invisible when the
    /// program owns the whole screen.
    pub fn load_reporting() -> (Self, Option<String>) {
        let path = default_path();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                let msg = format!("cannot read {}: {e}; using defaults", path.display());
                crate::log::write(&msg);
                return (Self::default(), Some(msg));
            }
        };
        match toml::from_str(&text) {
            Ok(c) => (c, None),
            Err(e) => {
                let msg = format!("bad config at {}: {e}; using defaults", path.display());
                crate::log::write(&msg);
                (Self::default(), Some(msg))
            }
        }
    }

    /// Resolve the leader key string into crossterm modifiers + code.
    pub fn leader_key(
        &self,
    ) -> (
        ratatui::crossterm::event::KeyModifiers,
        ratatui::crossterm::event::KeyCode,
    ) {
        match self.leader.as_deref() {
            Some("ctrl-space") | Some("ctrl+space") => (
                ratatui::crossterm::event::KeyModifiers::CONTROL,
                ratatui::crossterm::event::KeyCode::Char(' '),
            ),
            Some("ctrl-a") | Some("ctrl+a") => (
                ratatui::crossterm::event::KeyModifiers::CONTROL,
                ratatui::crossterm::event::KeyCode::Char('a'),
            ),
            _ => (
                ratatui::crossterm::event::KeyModifiers::CONTROL,
                ratatui::crossterm::event::KeyCode::Char(' '),
            ),
        }
    }

    /// Parse a hex color string like "#1e282c" or a named color.
    pub fn color(&self, key: &str, fallback: Color) -> Color {
        let hex = self.colors.as_ref().and_then(|c| match key {
            "bg" => c.bg.as_deref(),
            "bg_alt" => c.bg_alt.as_deref(),
            "fg" => c.fg.as_deref(),
            "fg_dim" => c.fg_dim.as_deref(),
            "accent" => c.accent.as_deref(),
            "accent_fg" => c.accent_fg.as_deref(),
            "border" => c.border.as_deref(),
            "warn" => c.warn.as_deref(),
            _ => None,
        });
        match hex {
            Some(s) => parse_hex(s).unwrap_or(fallback),
            None => fallback,
        }
    }
}

/// Parse "#RRGGBB" into a `ratatui::style::Color`.
fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Map an action string to a `Dir` for split commands.
pub fn split_dir(action: &str) -> Option<Dir> {
    match action {
        "split-h" | "split-horizontal" | "split-right" | "split-r" => Some(Dir::Horizontal),
        "split-v" | "split-vertical" | "split-down" | "split-d" => Some(Dir::Vertical),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let c = Config::default();
        assert!(c.mouse.unwrap_or(false));
        assert_eq!(c.leader.as_deref(), Some("ctrl-space"));
    }

    #[test]
    fn parse_hex_works() {
        assert_eq!(parse_hex("#1e282c"), Some(Color::Rgb(0x1e, 0x28, 0x2c)));
        assert_eq!(parse_hex("#ffffff"), Some(Color::Rgb(0xff, 0xff, 0xff)));
        assert!(parse_hex("not-a-color").is_none());
        assert!(parse_hex("#123").is_none());
    }

    #[test]
    fn split_dir_mapping() {
        assert_eq!(split_dir("split-h"), Some(Dir::Horizontal));
        assert_eq!(split_dir("split-v"), Some(Dir::Vertical));
        assert_eq!(split_dir("bogus"), None);
    }
}
