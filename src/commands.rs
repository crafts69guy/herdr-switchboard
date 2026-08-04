//! Command Catalog: shell import, Presets, privacy policy and selection history.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Config, Preset};
use crate::data::Theme;
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::state::{now, state_file};
use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionAction {
    Fill,
    Run,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CommandRecord {
    pub command: String,
    pub label: String,
    pub sources: Vec<String>,
    pub selected_count: u64,
    pub last_selected_at: u64,
    pub last_action: Option<SelectionAction>,
    pub recent_cwds: Vec<String>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

impl CommandRecord {
    pub fn first_line(&self) -> String {
        let first = self.command.lines().next().unwrap_or_default();
        if self.command.contains('\n') {
            format!("{first} …")
        } else {
            first.to_string()
        }
    }

    pub fn source_text(&self) -> String {
        self.sources.join(",")
    }
}

#[derive(Clone, Debug)]
pub struct Import {
    pub command: String,
    pub timestamp: u64,
}

pub struct CommandCatalog {
    records: Vec<CommandRecord>,
    diagnostics: Vec<String>,
    denied: HashSet<String>,
    history_path: Option<PathBuf>,
    deny_path: Option<PathBuf>,
    sort: CommandSort,
}

#[derive(Clone, Copy)]
enum CommandSort {
    Frecency,
    Recent,
    Frequency,
    Alphabetical,
}

impl CommandSort {
    fn parse(value: &str) -> Self {
        match value {
            "recent" => Self::Recent,
            "frequency" => Self::Frequency,
            "alphabetical" => Self::Alphabetical,
            _ => Self::Frecency,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Frecency => Self::Recent,
            Self::Recent => Self::Frequency,
            Self::Frequency => Self::Alphabetical,
            Self::Alphabetical => Self::Frecency,
        }
    }
}

impl CommandCatalog {
    pub fn load(cfg: &Config) -> Result<Self> {
        let history_path = state_file("commands.json");
        let deny_path = state_file("command-deny.txt");
        let stored = history_path
            .as_deref()
            .map(read_records)
            .transpose()?
            .unwrap_or_default();
        let denied = deny_path
            .as_deref()
            .map(read_denylist)
            .transpose()?
            .unwrap_or_default();
        let imports = read_login_shell_history().unwrap_or_default();
        let mut catalog = Self::from_sources(
            imports,
            &cfg.commands.presets,
            stored,
            denied,
            cfg.commands.history_limit,
            &cfg.commands.history_exclude,
            history_path,
            deny_path,
        )?;
        catalog.sort = CommandSort::parse(&cfg.commands.sort);
        catalog.sort_records();
        Ok(catalog)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_sources(
        imports: Vec<Import>,
        presets: &[Preset],
        stored: Vec<CommandRecord>,
        denied: HashSet<String>,
        limit: usize,
        exclude_patterns: &[String],
        history_path: Option<PathBuf>,
        deny_path: Option<PathBuf>,
    ) -> Result<Self> {
        let excludes = exclude_patterns
            .iter()
            .map(|pattern| {
                Regex::new(pattern).with_context(|| format!("invalid history_exclude {pattern:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut by_command: HashMap<String, CommandRecord> = stored
            .into_iter()
            .map(|record| (record.command.clone(), record))
            .collect();
        for (order, import) in imports.into_iter().enumerate() {
            if !allowed(&import.command, &denied, &excludes) {
                continue;
            }
            let record = by_command
                .entry(import.command.clone())
                .or_insert_with(|| empty_record(import.command.clone(), String::new()));
            add_source(record, "shell");
            let recency = if import.timestamp == 0 {
                order as u64 + 1
            } else {
                import.timestamp
            };
            record.last_selected_at = record.last_selected_at.max(recency);
        }
        let mut diagnostics = Vec::new();
        for preset in presets {
            if looks_sensitive(&preset.command) {
                diagnostics.push(format!(
                    "preset {:?} was excluded because it appears to contain a literal secret; use an environment variable",
                    safe_label(&preset.label)
                ));
                continue;
            }
            if !allowed(&preset.command, &denied, &excludes) {
                continue;
            }
            let record = by_command
                .entry(preset.command.clone())
                .or_insert_with(|| empty_record(preset.command.clone(), preset.label.clone()));
            if record.label.is_empty() {
                record.label = preset.label.clone();
            }
            add_source(record, "preset");
            match resolve_preset_cwd(&preset.cwd) {
                Ok(Some(cwd)) if !record.recent_cwds.contains(&cwd) => {
                    record.recent_cwds.insert(0, cwd);
                }
                Ok(_) => {}
                Err(_) => record
                    .diagnostics
                    .push("preset cwd is unavailable; historical-cwd run is disabled".into()),
            }
        }
        let mut records: Vec<_> = by_command
            .into_values()
            .filter(|record| allowed(&record.command, &denied, &excludes))
            .collect();
        records.sort_by_key(|record| std::cmp::Reverse(frecency(record)));
        records.truncate(limit);
        Ok(Self {
            records,
            diagnostics,
            denied,
            history_path,
            deny_path,
            sort: CommandSort::Frecency,
        })
    }

    pub fn records(&self) -> &[CommandRecord] {
        &self.records
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.sort_records();
    }

    fn sort_records(&mut self) {
        match self.sort {
            CommandSort::Frecency => self
                .records
                .sort_by_key(|record| std::cmp::Reverse(frecency(record))),
            CommandSort::Recent => self
                .records
                .sort_by_key(|record| std::cmp::Reverse(record.last_selected_at)),
            CommandSort::Frequency => self
                .records
                .sort_by_key(|record| std::cmp::Reverse(record.selected_count)),
            CommandSort::Alphabetical => self
                .records
                .sort_by_key(|record| record.first_line().to_lowercase()),
        }
    }

    pub fn record_selection(
        &mut self,
        command: &str,
        action: SelectionAction,
        cwd: Option<&str>,
    ) -> Result<()> {
        let Some(record) = self
            .records
            .iter_mut()
            .find(|record| record.command == command)
        else {
            anyhow::bail!("command is no longer in the catalog")
        };
        record.selected_count = record.selected_count.saturating_add(1);
        record.last_selected_at = now();
        record.last_action = Some(action);
        add_source(record, "switchboard");
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            record.recent_cwds.retain(|known| known != cwd);
            record.recent_cwds.insert(0, cwd.to_string());
            record.recent_cwds.truncate(5);
        }
        self.persist()
    }

    pub fn forget(&mut self, command: &str) -> Result<()> {
        self.records.retain(|record| record.command != command);
        self.denied.insert(fingerprint(command));
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        if let Some(path) = &self.deny_path {
            let mut hashes: Vec<_> = self.denied.iter().cloned().collect();
            hashes.sort();
            write_private(path, hashes.join("\n").as_bytes())?;
        }
        if let Some(path) = &self.history_path {
            write_private(path, &serde_json::to_vec_pretty(&self.records)?)?;
        }
        Ok(())
    }
}

fn empty_record(command: String, label: String) -> CommandRecord {
    CommandRecord {
        command,
        label,
        sources: Vec::new(),
        selected_count: 0,
        last_selected_at: 0,
        last_action: None,
        recent_cwds: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn add_source(record: &mut CommandRecord, source: &str) {
    if !record.sources.iter().any(|known| known == source) {
        record.sources.push(source.to_string());
    }
}

fn frecency(record: &CommandRecord) -> u64 {
    record
        .last_selected_at
        .saturating_add(record.selected_count.saturating_mul(86_400))
}

fn allowed(command: &str, denied: &HashSet<String>, excludes: &[Regex]) -> bool {
    !command.trim().is_empty()
        && !denied.contains(&fingerprint(command))
        && !looks_sensitive(command)
        && !excludes.iter().any(|pattern| pattern.is_match(command))
}

fn looks_sensitive(command: &str) -> bool {
    static ASSIGNMENT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static CREDENTIAL_URL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static SECRET_FLAG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static AUTH_HEADER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(r#"(?i)(password|passwd|token|secret|api[_-]?key)\s*=\s*["']?[^\s$]+"#)
            .expect("constant regex")
    });
    let credential_url = CREDENTIAL_URL
        .get_or_init(|| Regex::new(r#"[a-z]+://[^/\s:@]+:[^@\s]+@"#).expect("constant regex"));
    let secret_flag = SECRET_FLAG.get_or_init(|| {
        Regex::new(r#"(?i)(--password|--token|--secret|--api[_-]?key)(=|\s+)[^\s$]+"#)
            .expect("constant regex")
    });
    let auth_header = AUTH_HEADER.get_or_init(|| {
        Regex::new(r#"(?i)authorization\s*:\s*(bearer|basic)\s+[^\s$]+"#).expect("constant regex")
    });
    command.contains("-----BEGIN")
        || assignment.is_match(command)
        || credential_url.is_match(command)
        || secret_flag.is_match(command)
        || auth_header.is_match(command)
}

fn fingerprint(command: &str) -> String {
    let digest = Sha256::digest(command.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn safe_label(label: &str) -> String {
    label
        .chars()
        .filter(|ch| !ch.is_control())
        .take(48)
        .collect()
}

fn read_records(path: &Path) -> Result<Vec<CommandRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    anyhow::ensure!(
        fs::metadata(path)?.len() <= 16 * 1024 * 1024,
        "command history exceeds the 16 MiB safety limit"
    );
    serde_json::from_slice(&fs::read(path)?).context("parse command history")
}

fn read_denylist(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
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

fn read_login_shell_history() -> Result<Vec<Import>> {
    let shell = env::var("SHELL").unwrap_or_default();
    let home = env::var("HOME").unwrap_or_default();
    let (kind, path) = if shell.ends_with("/fish") {
        (
            "fish",
            env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".local/share"))
                .join("fish/fish_history"),
        )
    } else if shell.ends_with("/bash") {
        (
            "bash",
            env::var("HISTFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".bash_history")),
        )
    } else {
        (
            "zsh",
            env::var("HISTFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(&home).join(".zsh_history")),
        )
    };
    let (text, truncated) = read_tail(&path, 8 * 1024 * 1024)?;
    let text = if truncated {
        trim_to_record_boundary(kind, &text)
    } else {
        text
    };
    Ok(parse_shell_history(kind, &text))
}

fn read_tail(path: &Path, max_bytes: u64) -> Result<(String, bool)> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let truncated = len > max_bytes;
    if truncated {
        file.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut bytes = Vec::with_capacity(len.min(max_bytes) as usize);
    file.take(max_bytes).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    if truncated {
        Ok((
            text.split_once('\n')
                .map(|(_, tail)| tail)
                .unwrap_or_default()
                .to_string(),
            true,
        ))
    } else {
        Ok((text.into_owned(), false))
    }
}

fn trim_to_record_boundary(kind: &str, text: &str) -> String {
    let is_boundary = |line: &str| match kind {
        "zsh" => line.starts_with(": ") && line.contains(';'),
        "fish" => line.starts_with("- cmd: "),
        "bash" => line
            .strip_prefix('#')
            .is_some_and(|value| value.parse::<u64>().is_ok()),
        _ => false,
    };
    if kind == "bash" && !text.lines().any(is_boundary) {
        return text.to_string();
    }
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if is_boundary(line.trim_end_matches('\n')) {
            return text[offset..].to_string();
        }
        offset += line.len();
    }
    String::new()
}

fn resolve_preset_cwd(raw: &str) -> Result<Option<String>> {
    if raw == "origin" || raw.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        !raw.contains("$(") && !raw.contains('`'),
        "preset cwd cannot execute shell syntax"
    );
    let mut expanded = raw.to_string();
    if expanded == "~" || expanded.starts_with("~/") {
        let home = env::var("HOME").context("HOME is unavailable for preset cwd")?;
        expanded = format!("{home}{}", &expanded[1..]);
    }
    static VARIABLE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let variable = VARIABLE.get_or_init(|| {
        Regex::new(r#"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)"#)
            .expect("constant regex")
    });
    expanded = variable
        .replace_all(&expanded, |captures: &regex::Captures<'_>| {
            let name = captures
                .get(1)
                .or_else(|| captures.get(2))
                .map(|value| value.as_str())
                .unwrap_or_default();
            env::var(name).unwrap_or_default()
        })
        .into_owned();
    let path = Path::new(&expanded);
    anyhow::ensure!(
        path.is_absolute(),
        "preset cwd must resolve to an absolute path"
    );
    anyhow::ensure!(path.is_dir(), "preset cwd does not exist");
    Ok(Some(expanded))
}

pub fn parse_shell_history(kind: &str, text: &str) -> Vec<Import> {
    match kind {
        "fish" => parse_fish(text),
        "bash" => parse_bash(text),
        _ => parse_zsh(text),
    }
}

fn parse_zsh(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut timestamp = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(": ") {
            if !current.is_empty() {
                out.push(Import {
                    command: current,
                    timestamp,
                });
                current = String::new();
            }
            if let Some((meta, command)) = rest.split_once(';') {
                timestamp = meta
                    .split(':')
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                current.push_str(command.trim_end_matches('\\'));
                if command.ends_with('\\') {
                    current.push('\n');
                }
            }
        } else if !current.is_empty() {
            current.push_str(line.trim_end_matches('\\'));
            if line.ends_with('\\') {
                current.push('\n');
            }
        } else if !line.trim().is_empty() {
            out.push(Import {
                command: line.to_string(),
                timestamp: 0,
            });
        }
    }
    if !current.is_empty() {
        out.push(Import {
            command: current,
            timestamp,
        });
    }
    out
}

fn parse_bash(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut timestamp = 0;
    let mut current = Vec::new();
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix('#')
            .and_then(|value| value.parse::<u64>().ok())
        {
            if !current.is_empty() {
                out.push(Import {
                    command: current.join("\n"),
                    timestamp,
                });
                current.clear();
            }
            timestamp = value;
        } else if timestamp == 0 {
            if !line.trim().is_empty() {
                out.push(Import {
                    command: line.to_string(),
                    timestamp: 0,
                });
            }
        } else {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        out.push(Import {
            command: current.join("\n"),
            timestamp,
        });
    }
    out
}

fn parse_fish(text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    let mut command: Option<String> = None;
    let mut timestamp = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("- cmd: ") {
            if let Some(command) = command.take() {
                out.push(Import { command, timestamp });
            }
            command = Some(value.replace("\\n", "\n").replace("\\\\", "\\"));
            timestamp = 0;
        } else if let Some(value) = line.trim().strip_prefix("when: ") {
            timestamp = value.parse().unwrap_or(0);
        }
    }
    if let Some(command) = command {
        out.push(Import { command, timestamp });
    }
    out
}

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let normal = cfg.common.keymode == "normal";
    picker::run(CommandMode::new(&cfg)?, theme, normal)
}

struct CommandMode {
    catalog: CommandCatalog,
    origin_pane: String,
    origin_cwd: Option<String>,
    notifier: Notifier,
    bindings: HashMap<String, String>,
}

impl CommandMode {
    fn new(cfg: &Config) -> Result<Self> {
        Ok(Self {
            catalog: CommandCatalog::load(cfg)?,
            origin_pane: env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default(),
            origin_cwd: env::var("SWITCHBOARD_ORIGIN_CWD")
                .ok()
                .filter(|cwd| !cwd.is_empty()),
            notifier: Notifier::new(cfg),
            bindings: cfg.keys.get("commands").cloned().unwrap_or_default(),
        })
    }

    fn items(&self) -> Vec<PickerItem> {
        let mut items = self
            .catalog
            .records()
            .iter()
            .map(command_item)
            .collect::<Vec<_>>();
        if let Some(first) = items.first_mut() {
            first.preview.extend(
                self.catalog
                    .diagnostics()
                    .iter()
                    .map(|diagnostic| format!("warning  {diagnostic}")),
            );
        } else if !self.catalog.diagnostics().is_empty() {
            items.push(PickerItem {
                id: "__diagnostic".into(),
                primary: "No safe commands".into(),
                secondary: "review preset diagnostics".into(),
                document: Document::default(),
                preview: self.catalog.diagnostics().to_vec(),
                accent_slot: Some("red".into()),
            });
        }
        items
    }
}

impl PickerMode for CommandMode {
    fn title(&self) -> &str {
        "Commands"
    }
    fn accent_slot(&self) -> &'static str {
        "mauve"
    }
    fn schema(&self) -> FieldSchema {
        FieldSchema::new(
            &[
                ("command", MatchKind::Contains),
                ("label", MatchKind::Contains),
                ("cwd", MatchKind::Contains),
                ("source", MatchKind::Exact),
            ],
            &[("cmd", "command")],
        )
    }
    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "sort",
                key: KeyCode::Char('s'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥s",
                label: "sort",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "fill",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵",
                label: "fill",
                color_slot: "blue",
            },
            ActionSpec {
                id: "run",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::CONTROL,
                key_label: "^↵",
                label: "run",
                color_slot: "green",
            },
            ActionSpec {
                id: "run_cwd",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                key_label: "⌥↵",
                label: "run cwd",
                color_slot: "teal",
            },
            ActionSpec {
                id: "copy",
                key: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^y",
                label: "copy",
                color_slot: "peach",
            },
            ActionSpec {
                id: "forget",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x",
                label: "forget",
                color_slot: "red",
            },
        ]
    }
    fn key_bindings(&self) -> HashMap<String, String> {
        Config::try_load()
            .ok()
            .and_then(|cfg| cfg.keys.get("commands").cloned())
            .unwrap_or_else(|| self.bindings.clone())
    }
    fn action_disabled_reason(&self, item_id: &str, _action: &str) -> Option<String> {
        (item_id == "__diagnostic").then(|| "there is no safe command to act on".into())
    }
    fn reload_config(&mut self, config: &Config) -> Result<()> {
        self.catalog = CommandCatalog::load(config)?;
        self.notifier = Notifier::new(config);
        self.bindings = config.keys.get("commands").cloned().unwrap_or_default();
        Ok(())
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        Ok(self.items())
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        let record = self
            .catalog
            .records()
            .iter()
            .find(|record| fingerprint(&record.command) == item_id)
            .cloned()
            .context("command disappeared")?;
        if action == "sort" {
            self.catalog.cycle_sort();
            return Ok(ActionOutcome::StayOpen);
        }
        match action {
            "fill" => {
                if let Err(error) = send_to_pane(&self.origin_pane, &record.command, false) {
                    self.notifier.send(NotifyEvent::CommandDeliveryFailed, None);
                    return Err(error);
                }
                self.catalog.record_selection(
                    &record.command,
                    SelectionAction::Fill,
                    self.origin_cwd.as_deref(),
                )?;
            }
            "run" => {
                confirm_multiline(&record.command)?;
                if let Err(error) = send_to_pane(&self.origin_pane, &record.command, true) {
                    self.notifier.send(NotifyEvent::CommandDeliveryFailed, None);
                    return Err(error);
                }
                self.catalog.record_selection(
                    &record.command,
                    SelectionAction::Run,
                    self.origin_cwd.as_deref(),
                )?;
            }
            "run_cwd" => {
                let cwd = record
                    .recent_cwds
                    .first()
                    .context("command has no historical cwd")?;
                anyhow::ensure!(
                    Path::new(cwd).is_dir(),
                    "historical cwd no longer exists: {cwd}"
                );
                confirm_multiline(&record.command)?;
                let command = format!("cd -- {} && {}", shell_quote(cwd), record.command);
                if let Err(error) = send_to_pane(&self.origin_pane, &command, true) {
                    self.notifier.send(NotifyEvent::CommandDeliveryFailed, None);
                    return Err(error);
                }
                self.catalog
                    .record_selection(&record.command, SelectionAction::Run, Some(cwd))?;
            }
            "copy" => copy_text(&record.command)?,
            "forget" => self.catalog.forget(&record.command)?,
            _ => anyhow::bail!("unknown command action {action}"),
        }
        Ok(ActionOutcome::Close)
    }
}

fn command_item(record: &CommandRecord) -> PickerItem {
    let cwd = record.recent_cwds.join(" ");
    let source = record.source_text();
    let label = if record.label.is_empty() {
        record.first_line()
    } else {
        record.label.clone()
    };
    let mut preview = record
        .command
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    preview.extend([
        String::new(),
        format!("source  {source}"),
        format!("selected  {}", record.selected_count),
        format!("last used  {}", record.last_selected_at),
    ]);
    if !cwd.is_empty() {
        preview.push(format!("cwd  {cwd}"));
    }
    preview.extend(
        record
            .diagnostics
            .iter()
            .map(|diagnostic| format!("warning  {diagnostic}")),
    );
    PickerItem {
        id: fingerprint(&record.command),
        primary: label,
        secondary: source.clone(),
        document: Document {
            fuzzy: format!("{} {} {} {}", record.command, record.label, cwd, source),
            fields: picker::fields(&[
                ("command", record.command.clone()),
                ("label", record.label.clone()),
                ("cwd", cwd),
                ("source", source),
            ]),
        },
        preview,
        accent_slot: Some("blue".into()),
    }
}

fn send_to_pane(pane: &str, text: &str, run: bool) -> Result<()> {
    anyhow::ensure!(!pane.is_empty(), "origin pane is unavailable");
    let verb = if run { "run" } else { "send-text" };
    let status = Command::new("herdr")
        .args(["pane", verb, pane, text])
        .status()?;
    anyhow::ensure!(status.success(), "herdr pane {verb} failed");
    Ok(())
}

fn confirm_multiline(command: &str) -> Result<()> {
    if !command.contains('\n') {
        return Ok(());
    }
    println!("\x1b[1mRun multiline command?\x1b[0m\n\n{command}\n");
    print!("Type run to confirm: ");
    std::io::stdout().flush()?;
    let mut reply = String::new();
    std::io::stdin().read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == "run", "multiline run cancelled");
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn copy_text(text: &str) -> Result<()> {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if program_exists("wl-copy") {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {program}"))?;
    child
        .stdin
        .as_mut()
        .context("clipboard stdin unavailable")?
        .write_all(text.as_bytes())?;
    anyhow::ensure!(child.wait()?.success(), "clipboard command failed");
    Ok(())
}

fn program_exists(program: &str) -> bool {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .any(|directory| directory.join(program).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_parsers_preserve_multiline_commands() {
        let zsh = parse_shell_history("zsh", ": 100:0;cargo test\\\ncargo clippy\n");
        let bash =
            parse_shell_history("bash", "#100\ncargo test\ncargo clippy\n#200\ngit status\n");
        let fish = parse_shell_history("fish", "- cmd: cargo test\\ncargo clippy\n  when: 100\n");
        assert_eq!(zsh[0].command, "cargo test\ncargo clippy");
        assert_eq!(bash[0].command, "cargo test\ncargo clippy");
        assert_eq!(fish[0].command, "cargo test\ncargo clippy");
    }

    #[test]
    fn catalog_deduplicates_sources_and_rejects_literal_secrets() {
        let imports = vec![
            Import {
                command: "cargo test".into(),
                timestamp: 10,
            },
            Import {
                command: "TOKEN=literal deploy".into(),
                timestamp: 20,
            },
            Import {
                command: "mysql --password hunter2".into(),
                timestamp: 30,
            },
            Import {
                command: "curl -H 'Authorization: Bearer sk-secret' example.test".into(),
                timestamp: 40,
            },
        ];
        let presets = vec![Preset {
            label: "tests".into(),
            command: "cargo test".into(),
            cwd: "origin".into(),
        }];
        let catalog = CommandCatalog::from_sources(
            imports,
            &presets,
            Vec::new(),
            HashSet::new(),
            5_000,
            &[],
            None,
            None,
        )
        .unwrap();
        assert_eq!(catalog.records().len(), 1);
        assert_eq!(catalog.records()[0].sources, ["shell", "preset"]);
        assert_eq!(catalog.records()[0].label, "tests");
    }

    #[test]
    fn forget_denylist_prevents_reimport() {
        let root = env::temp_dir().join(format!("switchboard-command-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let history = root.join("commands.json");
        let deny = root.join("deny.txt");
        let import = Import {
            command: "cargo test".into(),
            timestamp: 10,
        };
        let mut catalog = CommandCatalog::from_sources(
            vec![import.clone()],
            &[],
            Vec::new(),
            HashSet::new(),
            5_000,
            &[],
            Some(history.clone()),
            Some(deny.clone()),
        )
        .unwrap();
        catalog.forget("cargo test").unwrap();
        let denied = read_denylist(&deny).unwrap();
        let next = CommandCatalog::from_sources(
            vec![import],
            &[],
            Vec::new(),
            denied,
            5_000,
            &[],
            None,
            None,
        )
        .unwrap();
        assert!(next.records().is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn truncated_structured_history_starts_at_next_logical_record() {
        let zsh = "continuation\n: 200:0;git status\n";
        let bash = "continuation\n#200\ngit status\n";
        assert_eq!(trim_to_record_boundary("zsh", zsh), ": 200:0;git status\n");
        assert_eq!(trim_to_record_boundary("bash", bash), "#200\ngit status\n");
    }

    #[test]
    fn alternate_sorts_are_deterministic() {
        let mut catalog = CommandCatalog::from_sources(
            vec![
                Import {
                    command: "z-last".into(),
                    timestamp: 20,
                },
                Import {
                    command: "a-first".into(),
                    timestamp: 10,
                },
            ],
            &[],
            Vec::new(),
            HashSet::new(),
            5_000,
            &[],
            None,
            None,
        )
        .unwrap();
        catalog.sort = CommandSort::Alphabetical;
        catalog.sort_records();
        assert_eq!(catalog.records()[0].command, "a-first");
    }
}
