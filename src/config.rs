//! Typed, namespaced Switchboard configuration.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct Config {
    pub common: Common,
    pub projects: Projects,
    pub commands: Commands,
    pub ports: Ports,
    pub clone: CloneFlow,
    pub git: Git,
    pub zen: Zen,
    pub usage: Usage,
    #[serde(default)]
    pub keys: HashMap<String, HashMap<String, String>>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    Insert,
    #[default]
    Normal,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Common {
    pub keymode: KeyMode,
    pub notifications: bool,
    pub notification_position: String,
    pub notification_sound: String,
    pub title_color: String,
    pub transparency: String,
    pub update_check: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Projects {
    pub default_target: String,
    pub split_direction: String,
    pub split_ratio: String,
    pub label: String,
    pub include_agents: bool,
    pub include_workspaces: bool,
    pub include_worktrees: bool,
    pub default_tab: String,
    pub sort: String,
    pub preview: String,
    pub preview_position: String,
    pub preview_size: String,
    pub preview_readme: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Commands {
    pub history_limit: usize,
    pub history_exclude: Vec<String>,
    pub sort: String,
    pub presets: Vec<Preset>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Ports {
    pub refresh_interval_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CloneFlow {
    pub source: String,
    pub open_after: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Git {
    pub base_branch: String,
    /// Ask before an all-files review of a tree with more tracked files than
    /// this; `0` never asks. `tuicr -A` reads every one of them, so a large
    /// checkout is minutes of a screen that cannot say it is working.
    pub all_files_warn: usize,
}

impl Default for Git {
    fn default() -> Self {
        Git {
            base_branch: String::new(),
            all_files_warn: 1500,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Zen {
    /// The zen'd pane's share of the tab, as a percentage. The rest is split
    /// evenly between the two gutters.
    pub width: u16,
    pub scrim: bool,
    /// `#rrggbb`. herdr composites the scrim opaquely, so this is the colour the
    /// gutters actually become, not a tint over them.
    pub scrim_color: String,
    /// How much of herdr's own chrome a zen session suppresses: `off`, `panes`
    /// (borders, gaps, scrollbars) or `full` (also the tab row and the sidebar).
    /// Anything but `off` rewrites keys in herdr's *own* `config.toml` for the
    /// length of the session, which is why the default is `off`.
    pub chrome: String,
}

impl Default for Zen {
    fn default() -> Self {
        Self {
            width: 70,
            scrim: true,
            scrim_color: "#11111b".into(),
            chrome: "off".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Usage {
    /// Which quota providers the popup shows, in display order. Dropping a name
    /// turns it off; an unknown name is ignored rather than fatal, so a config
    /// written for a later version still opens.
    pub providers: Vec<String>,
    /// How long the one networked provider may take before its card gives up.
    pub timeout_ms: u64,
    /// The percentages an ungraded window bar turns yellow and then red at.
    pub warn_percent: u16,
    pub alert_percent: u16,
}

impl Default for Usage {
    fn default() -> Self {
        Self {
            providers: vec!["codex".into(), "claude".into()],
            timeout_ms: 3000,
            warn_percent: 60,
            alert_percent: 85,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Preset {
    pub label: String,
    pub command: String,
    #[serde(default = "origin")]
    pub cwd: String,
}

fn origin() -> String {
    "origin".into()
}

impl Default for Common {
    fn default() -> Self {
        Self {
            keymode: KeyMode::Normal,
            notifications: true,
            notification_position: "top-right".into(),
            notification_sound: "auto".into(),
            title_color: "peach".into(),
            transparency: "auto".into(),
            update_check: true,
        }
    }
}

impl Default for Projects {
    fn default() -> Self {
        Self {
            default_target: "workspace".into(),
            split_direction: "right".into(),
            split_ratio: "0.5".into(),
            label: "repo".into(),
            include_agents: true,
            include_workspaces: true,
            include_worktrees: true,
            default_tab: "all".into(),
            sort: "recent".into(),
            preview: "enabled".into(),
            preview_position: "right".into(),
            preview_size: "60%".into(),
            preview_readme: true,
        }
    }
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            history_limit: 5_000,
            history_exclude: Vec::new(),
            sort: "frecency".into(),
            presets: Vec::new(),
        }
    }
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            refresh_interval_ms: 2_000,
        }
    }
}

impl Default for CloneFlow {
    fn default() -> Self {
        Self {
            source: "clipboard".into(),
            open_after: true,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("invalid switchboard config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load() -> Self {
        Self::try_load().unwrap_or_else(|error| {
            eprintln!("herdr-switchboard: config load failed: {error}");
            Self::default()
        })
    }

    pub fn try_load() -> Result<Self> {
        let path = config_path();
        Ok(if path.exists() {
            Self::parse(
                &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
            )?
        } else {
            Self::default()
        })
    }

    /// Serialize the small compatibility vocabulary consumed by the Bash
    /// entrypoints and `config get`. Rust callers should use the typed fields.
    pub fn value_for_cli(&self, key: &str) -> Option<String> {
        Some(match key {
            "keymode" => match self.common.keymode {
                KeyMode::Insert => "insert".into(),
                KeyMode::Normal => "normal".into(),
            },
            "notifications" => self.common.notifications.to_string(),
            "notification_position" => self.common.notification_position.clone(),
            "notification_sound" => self.common.notification_sound.clone(),
            "title_color" => self.common.title_color.clone(),
            "transparency" => self.common.transparency.clone(),
            "update_check" => self.common.update_check.to_string(),
            "default_target" => self.projects.default_target.clone(),
            "split_direction" => self.projects.split_direction.clone(),
            "split_ratio" => self.projects.split_ratio.clone(),
            "label" => self.projects.label.clone(),
            "include_agents" => self.projects.include_agents.to_string(),
            "include_workspaces" => self.projects.include_workspaces.to_string(),
            "include_worktrees" => self.projects.include_worktrees.to_string(),
            "default_tab" => self.projects.default_tab.clone(),
            "sort" => self.projects.sort.clone(),
            "preview" => self.projects.preview.clone(),
            "preview_position" => self.projects.preview_position.clone(),
            "preview_size" => self.projects.preview_size.clone(),
            "preview_readme" => self.projects.preview_readme.to_string(),
            "clone_source" => self.clone.source.clone(),
            "open_after_clone" => self.clone.open_after.to_string(),
            "base_branch" => self.git.base_branch.clone(),
            "all_files_warn" => self.git.all_files_warn.to_string(),
            "history_limit" => self.commands.history_limit.to_string(),
            "command_sort" => self.commands.sort.clone(),
            "refresh_interval_ms" => self.ports.refresh_interval_ms.to_string(),
            "usage_warn_percent" => self.usage.warn_percent.to_string(),
            "usage_alert_percent" => self.usage.alert_percent.to_string(),
            "zen_width" => self.zen.width.to_string(),
            "zen_scrim" => self.zen.scrim.to_string(),
            "zen_scrim_color" => self.zen.scrim_color.clone(),
            "zen_chrome" => self.zen.chrome.clone(),
            _ => return None,
        })
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.commands.history_limit > 0,
            "commands.history_limit must be positive"
        );
        anyhow::ensure!(
            self.ports.refresh_interval_ms >= 250,
            "ports.refresh_interval_ms must be at least 250"
        );
        let ratio = self
            .projects
            .split_ratio
            .parse::<f32>()
            .context("projects.split_ratio must be a number")?;
        anyhow::ensure!(
            (0.1..=0.9).contains(&ratio),
            "projects.split_ratio must be between 0.1 and 0.9"
        );
        anyhow::ensure!(
            (20..=95).contains(&self.zen.width),
            "zen.width must be between 20 and 95"
        );
        anyhow::ensure!(
            matches!(self.zen.chrome.as_str(), "off" | "panes" | "full"),
            "zen.chrome must be off, panes or full"
        );
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    env::var("HERDR_PLUGIN_CONFIG_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var("HERDR_PLUGIN_ROOT").unwrap_or_else(|_| ".".into()))
                .join(".config")
        })
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_config_parses_multiline_presets_and_defaults() {
        let cfg = Config::parse(
            r#"
[commands]
history_limit = 42

[[commands.presets]]
label = "quality"
command = """cargo test
cargo clippy"""
"#,
        )
        .expect("valid config");

        assert_eq!(cfg.commands.history_limit, 42);
        assert_eq!(cfg.commands.presets[0].command, "cargo test\ncargo clippy");
        assert_eq!(cfg.commands.presets[0].cwd, "origin");
        assert_eq!(cfg.ports.refresh_interval_ms, 2_000);
    }

    #[test]
    fn flat_config_is_rejected() {
        let result = Config::parse(
            r#"
notifications = "false"
include_agents = "false"
default_target = "tab"
keys.down = "ctrl-j,ctrl-n"
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn normal_mode_is_the_typed_default() {
        let cfg = Config::default();
        assert_eq!(cfg.common.keymode, KeyMode::Normal);
        assert_eq!(cfg.value_for_cli("keymode").as_deref(), Some("normal"));
    }

    #[test]
    fn cli_compatibility_is_finite_and_does_not_expose_arbitrary_paths() {
        let cfg = Config::parse("[common]\nnotifications = false\n").unwrap();
        assert_eq!(cfg.value_for_cli("notifications").as_deref(), Some("false"));
        assert_eq!(cfg.value_for_cli("common.notifications"), None);
        assert_eq!(cfg.value_for_cli("keys.projects.open"), None);
    }
}
