//! App registry — what the launcher menu can run, and from where.
//!
//! Apps are defined in `~/.config/termland/apps.toml`. Each app has a name,
//! the command to run, an optional upstream repo, and a category.
//!
//! There is **no update mechanism yet**: `github` is recorded so a future
//! manager knows where to look, but nothing in this crate fetches it. Updates
//! today are whatever the distro image ships.

use serde::Deserialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// One installable/runnable app.
#[derive(Clone, Debug, Deserialize)]
pub struct AppDef {
    /// Human-readable name.
    pub name: String,
    /// Command to run (shell-ish, split on whitespace).
    pub command: String,
    /// GitHub "owner/repo" for updates (optional).
    pub github: Option<String>,
    /// Category for the menu.
    pub category: String,
    /// Short description shown in the menu.
    pub description: Option<String>,
}

/// The full apps config file.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct AppsConfig {
    pub apps: Vec<AppDef>,
}

/// The app registry, loaded from disk (or the built-in defaults).
pub struct AppRegistry {
    pub apps: HashMap<String, AppDef>,
    pub config_path: PathBuf,
}

impl AppRegistry {
    /// Load from `~/.config/termland/apps.toml`, discarding any error report.
    pub fn load() -> Self {
        Self::load_reporting().0
    }

    /// Load, and report a bad or unreadable file so the UI can show it.
    pub fn load_reporting() -> (Self, Option<String>) {
        let config_path = dirs::home_dir()
            .map(|h| h.join(".config/termland/apps.toml"))
            .unwrap_or_else(|| PathBuf::from("apps.toml"));

        let mut error = None;
        let apps = match fs::read_to_string(&config_path) {
            Ok(text) => match toml::from_str::<AppsConfig>(&text) {
                Ok(c) => c.apps.into_iter().map(|a| (a.name.clone(), a)).collect(),
                Err(e) => {
                    let msg = format!(
                        "bad apps config at {}: {e}; using defaults",
                        config_path.display()
                    );
                    crate::log::write(&msg);
                    error = Some(msg);
                    Self::default_apps()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default_apps(),
            Err(e) => {
                let msg = format!("cannot read {}: {e}; using defaults", config_path.display());
                crate::log::write(&msg);
                error = Some(msg);
                Self::default_apps()
            }
        };

        (Self { apps, config_path }, error)
    }

    /// Built-in default apps (no GitHub updates — those come from your distro
    /// image or a later `apps.toml` you drop in).
    fn default_apps() -> HashMap<String, AppDef> {
        let mut m = HashMap::new();
        m.insert(
            "terminal".to_string(),
            AppDef {
                name: "terminal".to_string(),
                command: "$SHELL".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("Shell terminal".to_string()),
            },
        );
        m.insert(
            "shell".to_string(),
            AppDef {
                name: "shell".to_string(),
                command: "$SHELL".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("Shell terminal".to_string()),
            },
        );
        m.insert(
            "nmtui".to_string(),
            AppDef {
                name: "nmtui".to_string(),
                command: "nmtui".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("Network manager (Wi-Fi)".to_string()),
            },
        );
        m.insert(
            "bluetui".to_string(),
            AppDef {
                name: "bluetui".to_string(),
                command: "bluetui".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("Bluetooth manager".to_string()),
            },
        );
        m.insert(
            "pulsemixer".to_string(),
            AppDef {
                name: "pulsemixer".to_string(),
                command: "pulsemixer".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("Audio mixer (terminal)".to_string()),
            },
        );
        m.insert(
            "w3m".to_string(),
            AppDef {
                name: "w3m".to_string(),
                command: "w3m".to_string(),
                github: None,
                category: "Web".to_string(),
                description: Some("Text web browser".to_string()),
            },
        );
        m.insert(
            "btop".to_string(),
            AppDef {
                name: "btop".to_string(),
                command: "btop".to_string(),
                github: None,
                category: "System".to_string(),
                description: Some("System monitor".to_string()),
            },
        );
        m.insert(
            "hx".to_string(),
            AppDef {
                name: "hx".to_string(),
                command: "hx".to_string(),
                github: None,
                category: "Editor".to_string(),
                description: Some("Helix text editor".to_string()),
            },
        );
        m
    }

    /// Look up an app by name.
    pub fn get(&self, name: &str) -> Option<&AppDef> {
        self.apps.get(name)
    }

    /// List all app names.
    pub fn names(&self) -> Vec<&String> {
        self.apps.keys().collect()
    }

    /// List apps by category.
    pub fn by_category(&self, category: &str) -> Vec<&AppDef> {
        self.apps
            .values()
            .filter(|a| a.category == category)
            .collect()
    }

    /// All categories.
    pub fn categories(&self) -> Vec<&str> {
        let mut cats: Vec<&str> = self.apps.values().map(|a| a.category.as_str()).collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// Expand `$SHELL` and other env vars in a command.
    pub fn expand_command(cmd: &str) -> Vec<String> {
        let expanded = shellexpand::env(cmd).unwrap_or_else(|_| Cow::from(cmd.to_string()));
        expanded.split_whitespace().map(|s| s.to_string()).collect()
    }

    /// Turn one `apps = [...]` entry from `config.toml` into an argv.
    ///
    /// An entry may be a registry name (`shell`, `btop`) or a raw command
    /// (`hx README.md`). The registry wins when the first word matches, so the
    /// sample config's `apps = ["shell"]` becomes `$SHELL` instead of a doomed
    /// exec of a binary called `shell` — which is what it did before, leaving
    /// every configured workspace to the unrelated "empty workspace gets a
    /// shell" fallback.
    ///
    /// Extra words after a registry name are appended, so `w3m example.org`
    /// means "the w3m app, on that page".
    pub fn argv_for(&self, app_cmd: &str) -> Vec<String> {
        let mut words = app_cmd.split_whitespace();
        let Some(first) = words.next() else {
            return Vec::new();
        };
        match self.get(first) {
            Some(def) => {
                let mut argv = Self::expand_command(&def.command);
                argv.extend(words.map(|w| w.to_string()));
                argv
            }
            None => std::iter::once(first.to_string())
                .chain(words.map(|w| w.to_string()))
                .collect(),
        }
    }
}

impl Default for AppRegistry {
    fn default() -> Self {
        Self {
            apps: Self::default_apps(),
            config_path: PathBuf::from("apps.toml"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_apps_include_terminal() {
        let r = AppRegistry::load();
        assert!(r.get("terminal").is_some());
    }

    #[test]
    fn categories_are_unique() {
        let r = AppRegistry::load();
        let cats = r.categories();
        assert_eq!(
            cats.len(),
            cats.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }

    #[test]
    fn argv_for_resolves_registry_names() {
        let r = AppRegistry::default();
        assert_eq!(
            r.argv_for("shell"),
            vec![std::env::var("SHELL").unwrap_or("/bin/sh".into())]
        );
        assert_eq!(r.argv_for("btop"), vec!["btop"]);
        assert!(r.argv_for("").is_empty());
    }

    #[test]
    fn argv_for_passes_unknown_commands_through() {
        let r = AppRegistry::default();
        assert_eq!(r.argv_for("hx README.md"), vec!["hx", "README.md"]);
        assert_eq!(r.argv_for("w3m"), vec!["w3m"]);
    }

    #[test]
    fn expand_command_replaces_shell() {
        let args = AppRegistry::expand_command("$SHELL -c hello");
        assert!(!args.is_empty());
    }
}
