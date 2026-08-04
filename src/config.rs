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
    #[serde(default)]
    pub keys: HashMap<String, HashMap<String, String>>,
    #[serde(skip)]
    values: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Common {
    pub keymode: String,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Git {
    pub base_branch: String,
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
            keymode: "insert".into(),
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
        let mut cfg: Config = toml::from_str(text).context("invalid switchboard config")?;
        cfg.values = flatten_values(&toml::from_str(text)?);
        cfg.seed_scalar_values();
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

    pub fn get(&self, key: &str, default: &str) -> String {
        self.values
            .get(key)
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.get(key, if default { "true" } else { "false" }) == "true"
    }

    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            values: pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..Self::default()
        }
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
            matches!(self.common.keymode.as_str(), "insert" | "normal"),
            "common.keymode must be insert or normal"
        );
        Ok(())
    }

    fn seed_scalar_values(&mut self) {
        let pairs = [
            ("keymode", self.common.keymode.clone()),
            ("notifications", self.common.notifications.to_string()),
            (
                "notification_position",
                self.common.notification_position.clone(),
            ),
            ("notification_sound", self.common.notification_sound.clone()),
            ("title_color", self.common.title_color.clone()),
            ("transparency", self.common.transparency.clone()),
            ("update_check", self.common.update_check.to_string()),
            ("default_target", self.projects.default_target.clone()),
            ("split_direction", self.projects.split_direction.clone()),
            ("split_ratio", self.projects.split_ratio.clone()),
            ("label", self.projects.label.clone()),
            ("include_agents", self.projects.include_agents.to_string()),
            (
                "include_workspaces",
                self.projects.include_workspaces.to_string(),
            ),
            (
                "include_worktrees",
                self.projects.include_worktrees.to_string(),
            ),
            ("default_tab", self.projects.default_tab.clone()),
            ("sort", self.projects.sort.clone()),
            ("preview", self.projects.preview.clone()),
            ("preview_position", self.projects.preview_position.clone()),
            ("preview_size", self.projects.preview_size.clone()),
            ("preview_readme", self.projects.preview_readme.to_string()),
            ("clone_source", self.clone.source.clone()),
            ("open_after_clone", self.clone.open_after.to_string()),
            ("base_branch", self.git.base_branch.clone()),
            ("history_limit", self.commands.history_limit.to_string()),
            ("command_sort", self.commands.sort.clone()),
            (
                "refresh_interval_ms",
                self.ports.refresh_interval_ms.to_string(),
            ),
        ];
        self.values.extend(
            pairs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        );
        for (mode, bindings) in &self.keys {
            for (action, chord) in bindings {
                if mode == "projects" {
                    self.values.insert(format!("keys.{action}"), chord.clone());
                }
                self.values
                    .insert(format!("keys.{mode}.{action}"), chord.clone());
            }
        }
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

fn flatten_values(value: &toml::Value) -> HashMap<String, String> {
    fn visit(prefix: &str, value: &toml::Value, out: &mut HashMap<String, String>) {
        match value {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    let next = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    visit(&next, value, out);
                }
            }
            toml::Value::String(value) => {
                out.insert(prefix.to_string(), value.clone());
            }
            toml::Value::Boolean(value) => {
                out.insert(prefix.to_string(), value.to_string());
            }
            toml::Value::Integer(value) => {
                out.insert(prefix.to_string(), value.to_string());
            }
            _ => {}
        }
    }
    let mut out = HashMap::new();
    visit("", value, &mut out);
    out
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
}
