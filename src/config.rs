//! Typed, namespaced Switchboard configuration and the one-time legacy migration.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
        let migrated = if has_sections(text) {
            text.to_string()
        } else {
            migrate_flat_text(text)
        };
        let mut cfg: Config = toml::from_str(&migrated).context("invalid switchboard config")?;
        cfg.values = flatten_values(&toml::from_str(&migrated)?);
        cfg.seed_compat_values();
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
        let migrated = match migrate_if_needed() {
            Ok(migrated) => migrated,
            Err(error) => {
                crate::notify::Notifier::new(&Self::default())
                    .send(crate::notify::Event::MigrationFailed, None);
                return Err(error);
            }
        };
        let path = config_path();
        let cfg = if path.exists() {
            Self::parse(
                &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
            )?
        } else {
            Self::default()
        };
        if migrated {
            crate::notify::Notifier::new(&cfg).send(crate::notify::Event::MigrationSucceeded, None);
        }
        Ok(cfg)
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

    fn seed_compat_values(&mut self) {
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

fn has_sections(text: &str) -> bool {
    text.lines().any(|line| line.trim_start().starts_with('['))
}

fn migrate_flat_text(text: &str) -> String {
    let mut sections: HashMap<&str, Vec<String>> = HashMap::new();
    let comments = text
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .collect::<Vec<_>>();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let value = normalize_legacy_value(key, raw_value.trim());
        let (section, new_key) = legacy_location(key);
        sections
            .entry(section)
            .or_default()
            .push(format!("{new_key} = {value}"));
    }
    let mut out = String::new();
    for comment in comments {
        out.push_str(comment);
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    for section in ["common", "projects", "clone", "git", "keys.projects"] {
        let Some(lines) = sections.remove(section) else {
            continue;
        };
        out.push_str(&format!("[{section}]\n"));
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn normalize_legacy_value(key: &str, value: &str) -> String {
    let is_bool = matches!(
        key,
        "notifications"
            | "update_check"
            | "include_agents"
            | "include_workspaces"
            | "include_worktrees"
            | "preview_readme"
            | "open_after_clone"
    );
    if is_bool {
        let unquoted = value.trim_matches('"');
        if matches!(unquoted, "true" | "false") {
            return unquoted.to_string();
        }
    }
    value.to_string()
}

fn legacy_location(key: &str) -> (&str, &str) {
    if let Some(action) = key.strip_prefix("keys.") {
        return ("keys.projects", action);
    }
    match key {
        "keymode"
        | "notifications"
        | "notification_position"
        | "notification_sound"
        | "title_color"
        | "transparency"
        | "update_check" => ("common", key),
        "clone_source" => ("clone", "source"),
        "open_after_clone" => ("clone", "open_after"),
        "base_branch" => ("git", key),
        _ => ("projects", key),
    }
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

const MIGRATION_MARKER: &str = ".migrated-from-ghq";
const STATE_MIGRATION_MARKER: &str = ".switchboard-migration-in-progress";

pub fn migrate_if_needed() -> Result<bool> {
    let Some(new_config_dir) = env::var("HERDR_PLUGIN_CONFIG_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(false);
    };
    if new_config_dir.join(MIGRATION_MARKER).exists() || new_config_dir.join("config.toml").exists()
    {
        return Ok(false);
    }
    let Some(config_parent) = new_config_dir.parent() else {
        return Ok(false);
    };
    let old_config_dir = config_parent.join("ghq");
    if !old_config_dir.join("config.toml").exists() {
        return Ok(false);
    }
    let state_base = env::var("XDG_STATE_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".local/state"))
        });
    let (old_state, new_state) = match state_base {
        Some(base) => (
            Some(base.join("herdr-ghq")),
            Some(base.join("herdr-switchboard")),
        ),
        None => (None, None),
    };
    if let (Some(old_state), Some(new_state)) = (&old_state, &new_state) {
        recover_state_migration(old_state, new_state)?;
    }
    migrate_legacy(
        &old_config_dir,
        &new_config_dir,
        old_state.as_deref(),
        new_state.as_deref(),
    )?;
    Ok(true)
}

fn recover_state_migration(old_state: &Path, new_state: &Path) -> Result<()> {
    if old_state.exists() && new_state.join(STATE_MIGRATION_MARKER).exists() {
        fs::remove_dir_all(new_state).context("recover interrupted switchboard state migration")?;
    }
    Ok(())
}

pub fn migrate_legacy(
    old_config_dir: &Path,
    new_config_dir: &Path,
    old_state_dir: Option<&Path>,
    new_state_dir: Option<&Path>,
) -> Result<()> {
    let old_text =
        fs::read_to_string(old_config_dir.join("config.toml")).context("read legacy ghq config")?;
    let migrated_text = if has_sections(&old_text) {
        old_text.clone()
    } else {
        migrate_flat_text(&old_text)
    };
    Config::parse(&migrated_text)?;
    ensure_empty_destination(new_config_dir)?;
    if let Some(new_state) = new_state_dir {
        ensure_empty_destination(new_state)?;
    }
    let config_stage = stage_path(new_config_dir, "config")?;
    let state_stage = new_state_dir
        .filter(|_| old_state_dir.is_some_and(Path::exists))
        .map(|path| stage_path(path, "state"))
        .transpose()?;
    let result = (|| -> Result<()> {
        fs::create_dir_all(&config_stage)?;
        write_private(&config_stage.join("config.toml"), migrated_text.as_bytes())?;
        copy_optional(
            old_config_dir.join("menu.conf"),
            config_stage.join("menu.conf"),
        )?;

        if let (Some(old_state), Some(staged_state)) = (old_state_dir, state_stage.as_deref()) {
            fs::create_dir_all(staged_state)?;
            for name in ["recent.tsv", "update.tsv"] {
                copy_optional(old_state.join(name), staged_state.join(name))?;
            }
            write_private(&staged_state.join(STATE_MIGRATION_MARKER), b"in-progress\n")?;
        }

        let written = fs::read_to_string(config_stage.join("config.toml"))?;
        Config::parse(&written).context("validate migrated switchboard config")?;
        write_private(&config_stage.join(MIGRATION_MARKER), b"0.11\n")?;

        remove_empty_destination(new_config_dir)?;
        if let (Some(staged_state), Some(new_state)) = (&state_stage, new_state_dir) {
            remove_empty_destination(new_state)?;
            fs::rename(staged_state, new_state).context("commit migrated switchboard state")?;
        }
        if let Err(error) =
            fs::rename(&config_stage, new_config_dir).context("commit migrated switchboard config")
        {
            if let (Some(staged_state), Some(new_state)) = (&state_stage, new_state_dir) {
                let _ = fs::rename(new_state, staged_state);
            }
            return Err(error);
        }
        if let Some(new_state) = new_state_dir {
            let marker = new_state.join(STATE_MIGRATION_MARKER);
            if marker.exists() {
                fs::remove_file(marker).context("finish switchboard state migration")?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&config_stage);
        if let Some(staged_state) = &state_stage {
            let _ = fs::remove_dir_all(staged_state);
        }
        return Err(error);
    }

    if let Some(old_state) = old_state_dir.filter(|path| path.exists()) {
        fs::remove_dir_all(old_state).context("remove migrated ghq state")?;
    }
    fs::remove_dir_all(old_config_dir).context("remove migrated ghq config")?;
    Ok(())
}

fn ensure_empty_destination(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    anyhow::ensure!(
        fs::read_dir(path)?.next().is_none(),
        "migration destination already contains data: {}",
        path.display()
    );
    Ok(())
}

fn remove_empty_destination(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir(path)
            .with_context(|| format!("remove empty destination {}", path.display()))?;
    }
    Ok(())
}

fn stage_path(destination: &Path, kind: &str) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .context("migration destination has no parent")?;
    fs::create_dir_all(parent)?;
    let path = parent.join(format!(".switchboard-{kind}-{}.tmp", std::process::id()));
    anyhow::ensure!(!path.exists(), "migration staging path already exists");
    Ok(path)
}

fn copy_optional(from: PathBuf, to: PathBuf) -> Result<()> {
    if from.exists() {
        let bytes = fs::read(from)?;
        write_private(&to, &bytes)?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(tmp, path)?;
    Ok(())
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
    fn flat_config_migrates_string_booleans_and_project_keys() {
        let cfg = Config::parse(
            r#"
notifications = "false"
include_agents = "false"
default_target = "tab"
keys.down = "ctrl-j,ctrl-n"
"#,
        )
        .expect("legacy config migrates");

        assert!(!cfg.common.notifications);
        assert!(!cfg.projects.include_agents);
        assert_eq!(cfg.projects.default_target, "tab");
        assert_eq!(cfg.get("keys.down", ""), "ctrl-j,ctrl-n");
    }

    #[test]
    fn hard_migration_validates_destination_then_removes_legacy_tree() {
        let root = env::temp_dir().join(format!("switchboard-migrate-{}", std::process::id()));
        let old_config = root.join("config/ghq");
        let new_config = root.join("config/switchboard");
        let old_state = root.join("state/herdr-ghq");
        let new_state = root.join("state/herdr-switchboard");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&old_config).unwrap();
        fs::create_dir_all(&old_state).unwrap();
        fs::write(
            old_config.join("config.toml"),
            "# keep this comment\ndefault_target = \"tab\"\nnotifications = \"false\"\nfuture_option = \"keep\"\n",
        )
        .unwrap();
        fs::write(old_config.join("menu.conf"), "p||push|git push\n").unwrap();
        fs::write(old_state.join("recent.tsv"), "10\trepo\n").unwrap();

        migrate_legacy(&old_config, &new_config, Some(&old_state), Some(&new_state)).unwrap();

        assert!(!old_config.exists());
        assert!(!old_state.exists());
        assert!(new_config.join(MIGRATION_MARKER).exists());
        let cfg =
            Config::parse(&fs::read_to_string(new_config.join("config.toml")).unwrap()).unwrap();
        assert_eq!(cfg.projects.default_target, "tab");
        assert!(!cfg.common.notifications);
        let migrated_text = fs::read_to_string(new_config.join("config.toml")).unwrap();
        assert!(migrated_text.contains("# keep this comment"));
        assert!(migrated_text.contains("future_option = \"keep\""));
        assert_eq!(
            fs::read_to_string(new_state.join("recent.tsv")).unwrap(),
            "10\trepo\n"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hard_migration_never_overwrites_existing_destination() {
        let root = env::temp_dir().join(format!("switchboard-existing-{}", std::process::id()));
        let old_config = root.join("config/ghq");
        let new_config = root.join("config/switchboard");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&old_config).unwrap();
        fs::create_dir_all(&new_config).unwrap();
        fs::write(old_config.join("config.toml"), "default_target = \"tab\"\n").unwrap();
        fs::write(
            new_config.join("config.toml"),
            "[projects]\ndefault_target = \"pane\"\n",
        )
        .unwrap();

        assert!(migrate_legacy(&old_config, &new_config, None, None).is_err());
        assert!(old_config.join("config.toml").exists());
        assert!(fs::read_to_string(new_config.join("config.toml"))
            .unwrap()
            .contains("pane"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn interrupted_state_commit_is_recognized_and_recovered() {
        let root = env::temp_dir().join(format!("switchboard-recovery-{}", std::process::id()));
        let old_state = root.join("old");
        let new_state = root.join("new");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&old_state).unwrap();
        fs::create_dir_all(&new_state).unwrap();
        fs::write(new_state.join(STATE_MIGRATION_MARKER), "in-progress\n").unwrap();
        fs::write(new_state.join("recent.tsv"), "copy\n").unwrap();

        recover_state_migration(&old_state, &new_state).unwrap();
        assert!(!new_state.exists());
        assert!(old_state.exists());
        fs::remove_dir_all(root).ok();
    }
}
