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
use crate::state::{now, state_file, write_private};

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
    #[serde(default)]
    pub starred: bool,
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
        // `history_limit` bounds the ordinary imported catalogue, not the
        // commands the user explicitly chose to keep. Retain every star and up
        // to `limit` unstarred records while preserving the resting order.
        let mut ordinary_left = limit;
        records.retain(|record| {
            if record.starred {
                true
            } else if ordinary_left > 0 {
                ordinary_left -= 1;
                true
            } else {
                false
            }
        });
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

    pub fn toggle_star(&mut self, command: &str) -> Result<bool> {
        let Some(index) = self
            .records
            .iter()
            .position(|record| record.command == command)
        else {
            anyhow::bail!("command is no longer in the catalog")
        };
        let previous = self.records[index].starred;
        self.records[index].starred = !previous;
        if let Err(error) = self.persist() {
            self.records[index].starred = previous;
            return Err(error);
        }
        Ok(!previous)
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
        starred: false,
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

pub(super) fn read_records(path: &Path) -> Result<Vec<CommandRecord>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(pairs: &[(&str, u64)]) -> Vec<Import> {
        pairs
            .iter()
            .map(|(command, timestamp)| Import {
                command: (*command).to_string(),
                timestamp: *timestamp,
            })
            .collect()
    }

    /// A catalogue with no paths, so every mutation below persists nowhere.
    fn catalog(imports: Vec<Import>, presets: &[Preset], limit: usize) -> CommandCatalog {
        CommandCatalog::from_sources(
            imports,
            presets,
            Vec::new(),
            HashSet::new(),
            limit,
            &[],
            None,
            None,
        )
        .expect("a catalogue with no paths always builds")
    }

    fn preset(label: &str, command: &str) -> Preset {
        Preset {
            label: label.into(),
            command: command.into(),
            cwd: "origin".into(),
        }
    }

    fn commands(catalog: &CommandCatalog) -> Vec<&str> {
        catalog
            .records()
            .iter()
            .map(|r| r.command.as_str())
            .collect()
    }

    /// A command typed a hundred times is one row, carrying the newest
    /// timestamp — the history file is full of repeats and a list that showed
    /// each one would be unusable.
    #[test]
    fn repeated_history_entries_become_one_record() {
        let catalog = catalog(
            imports(&[("cargo test", 100), ("ls", 150), ("cargo test", 200)]),
            &[],
            50,
        );
        assert_eq!(catalog.records().len(), 2);
        let repeated = catalog
            .records()
            .iter()
            .find(|r| r.command == "cargo test")
            .expect("the repeated command is present");
        assert_eq!(repeated.last_selected_at, 200, "the newest use wins");
    }

    /// `history_limit` bounds what is imported, keeping the newest.
    #[test]
    fn the_history_limit_keeps_the_newest_commands() {
        let raw: Vec<(String, u64)> = (0..20)
            .map(|i| (format!("echo {i}"), 100 + i as u64))
            .collect();
        let as_refs: Vec<(&str, u64)> = raw.iter().map(|(c, t)| (c.as_str(), *t)).collect();
        let catalog = catalog(imports(&as_refs), &[], 5);

        assert_eq!(catalog.records().len(), 5);
        assert!(
            commands(&catalog).contains(&"echo 19"),
            "the newest survived: {:?}",
            commands(&catalog)
        );
        assert!(
            !commands(&catalog).contains(&"echo 0"),
            "the oldest was dropped"
        );
    }

    /// A preset reaches the catalogue carrying the label its config gave it and
    /// marked as a preset, so the row can say where it came from.
    #[test]
    fn a_preset_reaches_the_catalogue_with_its_label_and_source() {
        let catalog = catalog(
            imports(&[("echo one", 100)]),
            &[preset("Deploy", "make deploy")],
            50,
        );
        let deploy = catalog
            .records()
            .iter()
            .find(|r| r.command == "make deploy")
            .expect("the preset is present");
        assert_eq!(deploy.label, "Deploy");
        assert!(
            deploy.sources.iter().any(|s| s == "preset"),
            "{:?}",
            deploy.sources
        );
    }

    /// The limit bounds the ordinary catalogue but never drops a star: a command
    /// the user explicitly kept must survive an import that would otherwise
    /// push it out.
    #[test]
    fn the_limit_never_drops_a_starred_command() {
        let stored = vec![CommandRecord {
            command: "make release".into(),
            label: String::new(),
            sources: vec!["shell".into()],
            starred: true,
            selected_count: 0,
            last_selected_at: 1,
            last_action: None,
            recent_cwds: Vec::new(),
            diagnostics: Vec::new(),
        }];
        let catalog = CommandCatalog::from_sources(
            imports(&[("echo new", 500), ("echo newer", 600)]),
            &[],
            stored,
            HashSet::new(),
            1,
            &[],
            None,
            None,
        )
        .unwrap();

        assert!(
            commands(&catalog).contains(&"make release"),
            "a star survived the limit: {:?}",
            commands(&catalog)
        );
    }

    /// A preset carrying what looks like a literal secret is refused, and the
    /// diagnostic that explains why quotes the label — so that label is stripped
    /// of control characters and bounded before it is shown.
    #[test]
    fn a_preset_that_looks_like_a_secret_is_refused_with_a_sanitised_diagnostic() {
        let hostile = format!("Deploy\u{1b}[31m\r\n{}", "x".repeat(80));
        let catalog = catalog(
            Vec::new(),
            &[preset(
                &hostile,
                "curl -H 'Authorization: Bearer sk-live-abcdef123456'",
            )],
            50,
        );

        assert!(catalog.records().is_empty(), "the preset was not offered");
        let diagnostic = catalog.diagnostics().join("\n");
        assert!(diagnostic.contains("literal secret"), "{diagnostic}");
        assert!(
            !diagnostic.chars().any(|c| c == '\u{1b}' || c == '\r'),
            "the quoted label still carries control characters: {diagnostic:?}"
        );
        // And the quoted label is bounded rather than echoing the whole string.
        assert!(!diagnostic.contains(&"x".repeat(80)), "{diagnostic}");
    }

    /// Sorting cycles through every order and back, and each one actually
    /// reorders — a cycle that silently kept one order would look like sorting
    /// was broken only for some lists.
    #[test]
    fn cycling_the_sort_visits_every_order_and_reorders_the_list() {
        let mut catalog = catalog(
            imports(&[("zebra", 100), ("alpha", 300), ("middle", 200)]),
            &[],
            50,
        );
        for record in &mut catalog.records {
            record.selected_count = match record.command.as_str() {
                "zebra" => 9,
                "middle" => 5,
                _ => 1,
            };
        }

        catalog.sort = CommandSort::Alphabetical;
        catalog.sort_records();
        assert_eq!(commands(&catalog), ["alpha", "middle", "zebra"]);

        catalog.sort = CommandSort::Recent;
        catalog.sort_records();
        assert_eq!(commands(&catalog), ["alpha", "middle", "zebra"]);

        catalog.sort = CommandSort::Frequency;
        catalog.sort_records();
        assert_eq!(commands(&catalog), ["zebra", "middle", "alpha"]);

        // And the cycle returns to where it began after four steps.
        catalog.sort = CommandSort::Frecency;
        let start = std::mem::discriminant(&catalog.sort);
        for step in 1..4 {
            catalog.cycle_sort();
            assert_ne!(
                std::mem::discriminant(&catalog.sort),
                start,
                "step {step} came back to the start early"
            );
        }
        catalog.cycle_sort();
        assert_eq!(std::mem::discriminant(&catalog.sort), start);
    }

    /// Selecting a command is what makes it rise in the list, and it remembers
    /// where it was run so `run here` has somewhere to go.
    #[test]
    fn selecting_a_command_counts_it_and_remembers_its_directory() {
        let mut catalog = catalog(imports(&[("cargo test", 100)]), &[], 50);

        catalog
            .record_selection("cargo test", SelectionAction::Run, Some("/work/api"))
            .unwrap();
        catalog
            .record_selection("cargo test", SelectionAction::Fill, Some("/work/web"))
            .unwrap();

        let record = &catalog.records()[0];
        assert_eq!(record.selected_count, 2);
        assert!(record.last_selected_at > 100, "the clock moved forward");
        assert_eq!(record.last_action, Some(SelectionAction::Fill));
        assert_eq!(
            record.recent_cwds,
            ["/work/web", "/work/api"],
            "the newest directory is first"
        );

        // The same directory again moves it to the front rather than repeating.
        catalog
            .record_selection("cargo test", SelectionAction::Run, Some("/work/api"))
            .unwrap();
        assert_eq!(catalog.records()[0].recent_cwds, ["/work/api", "/work/web"]);
    }

    /// Only five directories are kept, so a command run all over the disk does
    /// not grow without bound.
    #[test]
    fn only_the_five_newest_directories_are_remembered() {
        let mut catalog = catalog(imports(&[("ls", 100)]), &[], 50);
        for index in 0..8 {
            catalog
                .record_selection("ls", SelectionAction::Run, Some(&format!("/d{index}")))
                .unwrap();
        }
        assert_eq!(catalog.records()[0].recent_cwds.len(), 5);
        assert_eq!(catalog.records()[0].recent_cwds[0], "/d7");
    }

    /// Acting on a command that is gone must be refused rather than silently
    /// recording against nothing.
    #[test]
    fn acting_on_a_command_that_is_gone_is_refused() {
        let mut catalog = catalog(imports(&[("ls", 100)]), &[], 50);
        let error = catalog
            .record_selection("not here", SelectionAction::Run, None)
            .unwrap_err();
        assert!(
            error.to_string().contains("no longer in the catalog"),
            "{error}"
        );

        let error = catalog.toggle_star("not here").unwrap_err();
        assert!(
            error.to_string().contains("no longer in the catalog"),
            "{error}"
        );
    }

    /// Starring toggles and reports the new state.
    #[test]
    fn starring_toggles_and_reports_the_state_it_reached() {
        let mut catalog = catalog(imports(&[("cargo test", 100)]), &[], 50);
        assert!(!catalog.records()[0].starred);

        assert!(catalog.toggle_star("cargo test").unwrap(), "now starred");
        assert!(catalog.records()[0].starred);
        assert!(!catalog.toggle_star("cargo test").unwrap(), "now unstarred");
        assert!(!catalog.records()[0].starred);
    }

    /// Forgetting removes the row *and* denies it, so the next import does not
    /// bring it straight back from the shell history it still lives in.
    #[test]
    fn forgetting_a_command_also_denies_it_from_returning() {
        let mut catalog = catalog(imports(&[("rm -rf build", 100), ("ls", 100)]), &[], 50);
        catalog.forget("rm -rf build").unwrap();
        assert_eq!(commands(&catalog), ["ls"]);

        // A fresh import carrying the same command respects the denylist.
        let reimported = CommandCatalog::from_sources(
            imports(&[("rm -rf build", 200), ("ls", 200)]),
            &[],
            Vec::new(),
            catalog.denied.clone(),
            50,
            &[],
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            commands(&reimported),
            ["ls"],
            "a forgotten command stays gone"
        );
    }

    /// An exclude pattern keeps matching commands out of the catalogue
    /// entirely — it is how a user keeps secrets out of a list they will show
    /// on screen.
    #[test]
    fn an_exclude_pattern_keeps_matching_commands_out() {
        let catalog = CommandCatalog::from_sources(
            imports(&[("export TOKEN=abc", 100), ("cargo test", 100)]),
            &[],
            Vec::new(),
            HashSet::new(),
            50,
            &["TOKEN".to_string()],
            None,
            None,
        )
        .unwrap();
        assert_eq!(commands(&catalog), ["cargo test"]);
    }

    /// A multi-line command's first line stands in for it in the list, marked so
    /// it is clear there is more.
    #[test]
    fn a_multiline_command_is_summarised_by_its_first_line() {
        let multi = catalog(imports(&[("git commit -m x\n\nbody", 100)]), &[], 50);
        assert_eq!(multi.records()[0].first_line(), "git commit -m x …");

        let single = catalog(imports(&[("ls -la", 100)]), &[], 50);
        assert_eq!(single.records()[0].first_line(), "ls -la");
    }

    /// The fingerprint is the row id, so two different commands must never
    /// share one and the same command must always produce the same one.
    #[test]
    fn a_fingerprint_is_stable_and_distinguishes_commands() {
        assert_eq!(fingerprint("cargo test"), fingerprint("cargo test"));
        assert_ne!(fingerprint("cargo test"), fingerprint("cargo test "));
        assert_ne!(fingerprint("cargo test"), fingerprint("cargo build"));
        assert!(!fingerprint("cargo test").is_empty());
    }

    /// Presets: a denied one is dropped, a fixed cwd is remembered once, an
    /// unusable cwd is a diagnostic, and missing state files read as empty.
    #[test]
    fn presets_are_filtered_resolved_and_diagnosed() {
        let dir = std::env::temp_dir();
        let dir = dir.to_string_lossy().into_owned();
        let presets = [
            Preset {
                label: "Denied".into(),
                command: "rm -rf build".into(),
                cwd: "origin".into(),
            },
            Preset {
                label: "Here".into(),
                command: "make".into(),
                cwd: dir.clone(),
            },
            Preset {
                label: "Shell".into(),
                command: "make check".into(),
                cwd: "$(pwd)".into(),
            },
        ];
        let denied = HashSet::from([fingerprint("rm -rf build")]);
        let catalog = CommandCatalog::from_sources(
            Vec::new(),
            &presets,
            Vec::new(),
            denied,
            5_000,
            &[],
            None,
            None,
        )
        .unwrap();
        let names = commands(&catalog);
        assert!(!names.contains(&"rm -rf build"), "{names:?}");
        let here = catalog
            .records()
            .iter()
            .find(|record| record.command == "make")
            .unwrap();
        assert_eq!(here.recent_cwds, vec![dir]);
        let shell = catalog
            .records()
            .iter()
            .find(|record| record.command == "make check")
            .unwrap();
        assert!(!shell.diagnostics.is_empty());

        let missing = std::path::Path::new("/definitely/not/a/state/file.json");
        assert!(read_records(missing).unwrap().is_empty());
        assert!(read_denylist(missing).unwrap().is_empty());
    }
}
