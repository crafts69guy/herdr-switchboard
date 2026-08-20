//! Command catalogue merge, privacy policy, selection state, and persistence.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::history::{read_login_shell_history, resolve_preset_cwd};
use crate::config::{Config, Preset};
use crate::state::{now, state_file};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SelectionAction {
    Fill,
    Run,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CommandRecord {
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
pub(super) struct Import {
    pub command: String,
    pub timestamp: u64,
}

pub(super) struct CommandCatalog {
    records: Vec<CommandRecord>,
    diagnostics: Vec<String>,
    denied: HashSet<String>,
    history_path: Option<PathBuf>,
    deny_path: Option<PathBuf>,
    pub(super) sort: CommandSort,
}

#[derive(Clone, Copy)]
pub(super) enum CommandSort {
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

    pub(super) fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.sort_records();
    }

    pub(super) fn sort_records(&mut self) {
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

pub(super) fn empty_record(command: String, label: String) -> CommandRecord {
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

/// A unix timestamp as the gutter tag the list is actually sorted by: `2h`, `3d`.
/// Coarse on purpose — this column exists to show the recency gradient down the
/// list, not to report a duration.
pub(super) fn ago(at: u64) -> String {
    if at == 0 {
        return "—".into();
    }
    let secs = crate::state::now().saturating_sub(at);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=2_591_999 => format!("{}d", secs / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    }
}

/// The same instant as a sortable absolute date, for the preview card.
pub(super) fn stamp(at: u64) -> String {
    if at == 0 {
        return "never".into();
    }
    // Civil-from-days (Howard Hinnant's algorithm), so the card can print a date
    // without pulling in a date crate for one line.
    let days = (at / 86_400) as i64;
    let secs_of_day = at % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60
    )
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

pub(super) fn fingerprint(command: &str) -> String {
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

pub(super) fn read_denylist(path: &Path) -> Result<HashSet<String>> {
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
