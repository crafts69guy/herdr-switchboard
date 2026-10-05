//! Picker adapter for command search and selection actions.

use std::collections::HashMap;
use std::env;
use std::path::Path;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use super::action::{confirm_multiline, send_to_pane, shell_quote};
use super::catalog::{ago, fingerprint, stamp, CommandCatalog, CommandRecord, SelectionAction};
use crate::clipboard::copy_text;
use crate::config::Config;
use crate::data::Theme;
use crate::keymap::parse_chord;
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::picker::{
    self, ActionOutcome, ActionSpec, PickerItem, PickerMarker, PickerMode, PickerTab,
};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::runner::{CommandRunner, SystemRunner};

pub(super) fn run(cfg: Config, theme: Theme) -> Result<()> {
    let mode = CommandMode::new(&cfg)?;
    picker::run(mode, theme, cfg)
}

/// The star chord's cap, spelled once. The action bar prints it and the empty
/// Starred tab names it; a second literal is how the two drift apart.
const STAR_CAP: &str = "^s";

struct CommandMode {
    catalog: CommandCatalog,
    tab: CommandTab,
    origin_pane: String,
    origin_cwd: Option<String>,
    notifier: Notifier,
    /// The effect edge: herdr delivery, the clipboard, and the multiline
    /// prompt. Production wires the real ones; tests swap in harmless doubles.
    runner: Box<dyn CommandRunner>,
    copy: fn(&str) -> Result<()>,
    confirm: fn(&str) -> Result<()>,
    bindings: HashMap<String, String>,
    /// The empty-Starred sentence, rebuilt whenever the bindings are, because it
    /// names a key and every other cap on this surface follows a remap.
    starred_empty: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandTab {
    History,
    Starred,
}

impl CommandMode {
    fn new(cfg: &Config) -> Result<Self> {
        Ok(Self::with_catalog(cfg, CommandCatalog::load(cfg)?))
    }

    /// The mode over an already-loaded catalogue, with the origin read from the
    /// environment Herdr gives the pane.
    fn with_catalog(cfg: &Config, catalog: CommandCatalog) -> Self {
        let bindings = cfg.keys.get("commands").cloned().unwrap_or_default();
        let starred_empty = starred_empty(&bindings);
        Self {
            catalog,
            tab: CommandTab::History,
            origin_pane: env::var("SWITCHBOARD_ORIGIN_PANE_ID").unwrap_or_default(),
            origin_cwd: env::var("SWITCHBOARD_ORIGIN_CWD")
                .ok()
                .filter(|cwd| !cwd.is_empty()),
            notifier: Notifier::new(cfg),
            runner: Box::new(SystemRunner),
            copy: copy_text,
            confirm: confirm_multiline,
            bindings,
            starred_empty,
        }
    }

    fn items(&self) -> Vec<PickerItem> {
        let mut items = self
            .catalog
            .records()
            .iter()
            .filter(|record| self.tab == CommandTab::History || record.starred)
            .map(command_item)
            .collect::<Vec<_>>();
        if self.tab == CommandTab::History {
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
                    trailing: None,
                    trailing_marker: None,
                    document: Document::default(),
                    preview: self.catalog.diagnostics().to_vec(),
                    accent_slot: Some("red".into()),
                });
            }
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
    fn emphasize_head(&self) -> bool {
        true
    }
    /// Rows here are whole shell commands and the card beside them is a short
    /// metadata block, so the split that suits Ports leaves this list truncating
    /// against a mostly empty preview.
    fn list_pct(&self) -> u16 {
        58
    }
    fn action_bar_rows(&self) -> u16 {
        2
    }
    fn tabs(&self) -> Vec<PickerTab> {
        vec![
            PickerTab {
                id: "history",
                label: "History",
                active: self.tab == CommandTab::History,
            },
            PickerTab {
                id: "starred",
                label: "★ Starred",
                active: self.tab == CommandTab::Starred,
            },
        ]
    }
    fn activate_tab(&mut self, id: &str) -> Option<Vec<PickerItem>> {
        self.tab = match id {
            "history" => CommandTab::History,
            "starred" => CommandTab::Starred,
            _ => return None,
        };
        Some(self.items())
    }
    fn empty_message(&self) -> &str {
        match self.tab {
            CommandTab::History => "No safe commands",
            CommandTab::Starred => &self.starred_empty,
        }
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
                key_label: "⌥s".into(),
                label: "sort",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "fill",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵".into(),
                label: "fill",
                color_slot: "blue",
            },
            ActionSpec {
                id: "run",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::CONTROL,
                key_label: "^↵".into(),
                label: "run",
                color_slot: "green",
            },
            ActionSpec {
                id: "run_cwd",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                key_label: "⌥↵".into(),
                label: "run cwd",
                color_slot: "teal",
            },
            ActionSpec {
                id: "copy",
                key: KeyCode::Char('y'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^y".into(),
                label: "copy",
                color_slot: "peach",
            },
            ActionSpec {
                id: "star",
                key: KeyCode::Char('s'),
                modifiers: KeyModifiers::CONTROL,
                key_label: STAR_CAP.into(),
                label: "star/unstar",
                color_slot: "yellow",
            },
            ActionSpec {
                id: "forget",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x".into(),
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
        self.starred_empty = starred_empty(&self.bindings);
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
        if action == "star" {
            self.catalog.toggle_star(&record.command)?;
            return Ok(ActionOutcome::StayOpen);
        }
        match action {
            "fill" => {
                if let Err(error) = send_to_pane(
                    self.runner.as_ref(),
                    &self.origin_pane,
                    &record.command,
                    false,
                ) {
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
                (self.confirm)(&record.command)?;
                if let Err(error) = send_to_pane(
                    self.runner.as_ref(),
                    &self.origin_pane,
                    &record.command,
                    true,
                ) {
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
                (self.confirm)(&record.command)?;
                let command = format!("cd -- {} && {}", shell_quote(cwd), record.command);
                if let Err(error) =
                    send_to_pane(self.runner.as_ref(), &self.origin_pane, &command, true)
                {
                    self.notifier.send(NotifyEvent::CommandDeliveryFailed, None);
                    return Err(error);
                }
                self.catalog
                    .record_selection(&record.command, SelectionAction::Run, Some(cwd))?;
            }
            "copy" => (self.copy)(&record.command)?,
            "forget" => self.catalog.forget(&record.command)?,
            _ => anyhow::bail!("unknown command action {action}"),
        }
        Ok(ActionOutcome::Close)
    }
}

/// The empty-Starred sentence, naming the star chord as it is actually bound.
fn starred_empty(bindings: &HashMap<String, String>) -> String {
    let cap = bindings
        .get("star")
        .and_then(|spec| spec.split(',').next())
        .and_then(parse_chord)
        .map(|chord| chord.label())
        .unwrap_or_else(|| STAR_CAP.to_string());
    format!("No stars — {cap} in History")
}

pub(super) fn command_item(record: &CommandRecord) -> PickerItem {
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
        format!("starred  {}", if record.starred { "yes" } else { "no" }),
        format!("selected  {} ×", record.selected_count),
        // A unix timestamp is not a fact anyone can read off a card. Both forms
        // are here because the relative one answers "is this stale?" at a glance
        // and the absolute one is what you quote when something looks wrong.
        format!(
            "last used  {} ({})",
            ago(record.last_selected_at),
            stamp(record.last_selected_at)
        ),
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
        // `shell` is where all but a handful of these come from, so printing it on
        // every row was forty identical words down a ragged column. The badge now
        // says only what is worth noticing — that an entry is a preset, or ours.
        secondary: match source.as_str() {
            "shell" => String::new(),
            other => other.to_string(),
        },
        trailing: Some(format!("{:>4}", ago(record.last_selected_at))),
        trailing_marker: record.starred.then(|| PickerMarker::new("★", "peach")),
        document: Document::new(
            format!("{} {} {} {}", record.command, record.label, cwd, source),
            &[
                ("command", record.command.clone()),
                ("label", record.label.clone()),
                ("cwd", cwd),
                ("source", source),
            ],
        ),
        preview,
        accent_slot: Some("blue".into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::commands::catalog::{empty_record, CommandCatalog, Import, SelectionAction};
    use crate::config::Preset;
    use crate::runner::MockRunner;

    fn command_mode() -> CommandMode {
        let mut starred = empty_record("cargo test".into(), String::new());
        starred.starred = true;
        let ordinary = empty_record("git status".into(), String::new());
        CommandMode {
            catalog: CommandCatalog::from_sources(
                Vec::new(),
                &[],
                vec![starred, ordinary],
                HashSet::new(),
                5_000,
                &[],
                None,
                None,
            )
            .unwrap(),
            tab: CommandTab::History,
            origin_pane: String::new(),
            origin_cwd: None,
            notifier: Notifier::silent(),
            runner: Box::new(MockRunner::new()),
            copy: |_| Ok(()),
            confirm: |_| Ok(()),
            bindings: HashMap::new(),
            starred_empty: starred_empty(&HashMap::new()),
        }
    }

    /// A catalogue with nothing safe to offer but something to say puts the
    /// diagnostic in the list itself — an empty pane with the reason hidden in a
    /// card nobody can select reads as a bug.
    #[test]
    fn diagnostics_reach_the_list_even_when_there_is_nothing_to_run() {
        let mut empty = CommandMode {
            catalog: CommandCatalog::from_sources(
                Vec::new(),
                &[Preset {
                    label: "Deploy".into(),
                    command: "curl -H 'Authorization: Bearer sk-live-abcdef123456'".into(),
                    cwd: "origin".into(),
                }],
                Vec::new(),
                HashSet::new(),
                5_000,
                &[],
                None,
                None,
            )
            .unwrap(),
            tab: CommandTab::History,
            origin_pane: String::new(),
            origin_cwd: None,
            notifier: Notifier::silent(),
            runner: Box::new(MockRunner::new()),
            copy: |_| Ok(()),
            confirm: |_| Ok(()),
            bindings: HashMap::new(),
            starred_empty: starred_empty(&HashMap::new()),
        };

        let items = empty.items();
        assert_eq!(items.len(), 1, "the diagnostic is the only row");
        assert_eq!(items[0].id, "__diagnostic");
        assert!(
            items[0].preview.join("\n").contains("literal secret"),
            "{:?}",
            items[0].preview
        );

        // Starred is a different view and does not carry the placeholder.
        assert!(empty.activate_tab("starred").unwrap().is_empty());
    }

    /// When there *are* commands, the diagnostics ride along on the first card
    /// rather than taking a row of their own.
    #[test]
    fn diagnostics_ride_the_first_card_when_there_are_commands_to_show() {
        let mut mode = CommandMode {
            catalog: CommandCatalog::from_sources(
                vec![Import {
                    command: "cargo test".into(),
                    timestamp: 100,
                }],
                &[Preset {
                    label: "Deploy".into(),
                    command: "curl -H 'Authorization: Bearer sk-live-abcdef123456'".into(),
                    cwd: "origin".into(),
                }],
                Vec::new(),
                HashSet::new(),
                5_000,
                &[],
                None,
                None,
            )
            .unwrap(),
            tab: CommandTab::History,
            origin_pane: String::new(),
            origin_cwd: None,
            notifier: Notifier::silent(),
            runner: Box::new(MockRunner::new()),
            copy: |_| Ok(()),
            confirm: |_| Ok(()),
            bindings: HashMap::new(),
            starred_empty: starred_empty(&HashMap::new()),
        };

        let items = mode.items();
        assert!(
            items.iter().all(|item| item.id != "__diagnostic"),
            "no placeholder row when there is something to run"
        );
        assert!(
            items[0].preview.join("\n").contains("warning"),
            "the warning rides the first card: {:?}",
            items[0].preview
        );
        assert_eq!(mode.initial().unwrap().len(), items.len());
    }

    /// A row leads with the label a preset gave it, or the command itself, and
    /// carries the facts that decide whether it is worth running again.
    #[test]
    fn a_command_row_leads_with_its_label_and_states_its_history() {
        let mut record = empty_record("cargo test --workspace".into(), "Full suite".into());
        record.selected_count = 7;
        record.starred = true;
        record.recent_cwds = vec!["/work/api".into()];
        record.sources = vec!["preset".into()];

        let item = command_item(&record);
        assert_eq!(item.primary, "Full suite", "a labelled row shows its label");
        assert_eq!(item.secondary, "preset");
        assert!(item.trailing_marker.is_some(), "a star is marked");
        let card = item.preview.join("\n");
        assert!(card.contains("cargo test --workspace"), "{card}");
        assert!(card.contains("selected  7 ×"), "{card}");
        assert!(card.contains("starred  yes"), "{card}");
        assert!(card.contains("cwd  /work/api"), "{card}");

        // An unlabelled shell command shows itself, and `shell` is not a badge:
        // it is where all but a handful come from.
        let plain = command_item(&empty_record("ls -la".into(), String::new()));
        assert_eq!(plain.primary, "ls -la");
        assert_eq!(plain.secondary, "", "the common source is not repeated");
        assert!(plain.trailing_marker.is_none());
    }

    /// A record carrying a diagnostic says so on its card rather than silently
    /// offering an action that will fail.
    #[test]
    fn a_records_diagnostic_reaches_its_card() {
        let mut record = empty_record("make deploy".into(), "Deploy".into());
        record.diagnostics = vec!["preset cwd is unavailable".into()];
        let card = command_item(&record).preview.join("\n");
        assert!(
            card.contains("warning  preset cwd is unavailable"),
            "{card}"
        );
    }

    /// The two tabs are the mode's own view over one catalogue, and only the
    /// active one is marked — the shared picker draws from that.
    #[test]
    fn exactly_one_tab_is_active_and_an_unknown_tab_is_refused() {
        let mut mode = command_mode();

        let tabs = mode.tabs();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.iter().filter(|tab| tab.active).count(), 1);
        assert!(tabs[0].active, "History is where it opens");

        mode.activate_tab("starred").unwrap();
        assert!(mode.tabs()[1].active);
        assert!(
            mode.activate_tab("not-a-tab").is_none(),
            "an unknown tab must not silently switch the view"
        );
        assert!(mode.tabs()[1].active, "the view did not change");
    }

    /// Each tab says something different when it is empty: History means there
    /// is nothing safe to show, Starred means the user has not starred anything.
    #[test]
    fn each_empty_tab_explains_itself_differently() {
        let mut mode = command_mode();
        assert_eq!(mode.empty_message(), "No safe commands");
        mode.activate_tab("starred").unwrap();
        assert!(
            mode.empty_message().contains(STAR_CAP),
            "{}",
            mode.empty_message()
        );
    }

    /// The empty tab names a key, so it has to follow a remap the way the pill
    /// caps do — a frozen literal would send the user to a key that no longer
    /// stars anything.
    #[test]
    fn the_empty_starred_tab_names_the_remapped_star_key() {
        let mut cfg = Config::default();
        cfg.keys
            .entry("commands".into())
            .or_default()
            .insert("star".into(), "alt-f".into());
        let mut mode = command_mode();
        mode.reload_config(&cfg).unwrap();
        mode.activate_tab("starred").unwrap();
        assert!(
            mode.empty_message().contains("⌥f"),
            "{}",
            mode.empty_message()
        );
    }

    /// Every documented filter field has to resolve against a real row.
    #[test]
    fn every_advertised_filter_field_matches_the_row_it_describes() {
        let mut record = empty_record("cargo test --workspace".into(), "Full suite".into());
        record.recent_cwds = vec!["/work/api".into()];
        record.sources = vec!["preset".into()];
        let item = command_item(&record);
        let mode = command_mode();
        let schema = mode.schema();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);

        for query in [
            "command:workspace",
            "cmd:cargo",
            "label:suite",
            "cwd:/work/api",
            "source:preset",
        ] {
            let compiled = crate::query::CompiledQuery::compile(query, &schema)
                .unwrap_or_else(|error| panic!("`{query}` did not compile: {error:?}"));
            assert!(
                compiled.score(&item.document, &mut matcher).is_some(),
                "`{query}` matched no command"
            );
        }
    }

    /// Sorting is a mode action rather than a row action: it keeps the picker
    /// open and reorders in place.
    #[test]
    fn cycling_the_sort_keeps_the_picker_open() {
        let mut mode = command_mode();
        let id = fingerprint("cargo test");
        assert!(matches!(
            mode.execute(&id, "sort").unwrap(),
            ActionOutcome::StayOpen
        ));
    }

    /// Forgetting drops the row and closes, because the thing that was selected
    /// no longer exists to act on.
    #[test]
    fn forgetting_a_command_removes_it_and_closes() {
        let mut mode = command_mode();
        let id = fingerprint("git status");

        assert!(matches!(
            mode.execute(&id, "forget").unwrap(),
            ActionOutcome::Close
        ));
        assert!(
            !mode.items().iter().any(|item| item.id == id),
            "the forgotten row is gone"
        );
    }

    /// `run here` needs a directory the command was actually used in, and that
    /// directory has to still exist — running somewhere else is worse than
    /// refusing.
    #[test]
    fn running_in_a_historical_directory_needs_one_that_still_exists() {
        let mut mode = command_mode();
        let id = fingerprint("git status");

        let error = mode.execute(&id, "run_cwd").unwrap_err();
        assert!(error.to_string().contains("no historical cwd"), "{error}");

        // A directory that was recorded but has since gone is refused by name.
        mode.catalog
            .record_selection("git status", SelectionAction::Run, Some("/definitely/gone"))
            .unwrap();
        let error = mode.execute(&id, "run_cwd").unwrap_err();
        assert!(error.to_string().contains("no longer exists"), "{error}");
    }

    fn mode_with(runner: MockRunner) -> (CommandMode, &'static MockRunner) {
        let runner = runner.leak();
        let mut mode = command_mode();
        mode.runner = Box::new(runner);
        mode.origin_pane = "w1:p1".into();
        mode.origin_cwd = Some("/repo".into());
        (mode, runner)
    }

    /// Fill types the command into the origin pane; run also presses Enter.
    /// Either one is recorded as a selection in the cwd it came from.
    #[test]
    fn fill_and_run_deliver_the_exact_command_to_the_origin_pane() {
        let (mut mode, runner) = mode_with(MockRunner::new());
        let id = fingerprint("git status");

        assert!(matches!(
            mode.execute(&id, "fill").unwrap(),
            ActionOutcome::Close
        ));
        assert!(matches!(
            mode.execute(&id, "run").unwrap(),
            ActionOutcome::Close
        ));
        assert_eq!(
            runner.calls(),
            vec![
                vec!["herdr", "pane", "send-text", "w1:p1", "git status"],
                vec!["herdr", "pane", "run", "w1:p1", "git status"],
            ]
        );
        let record = mode
            .catalog
            .records()
            .iter()
            .find(|record| record.command == "git status")
            .expect("still catalogued");
        assert_eq!(record.selected_count, 2);
        assert_eq!(record.recent_cwds, vec!["/repo".to_string()]);
    }

    /// Run-here prefixes a quoted `cd` into the remembered directory.
    #[test]
    fn running_in_a_historical_directory_changes_into_it_first() {
        let (mut mode, runner) = mode_with(MockRunner::new());
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        mode.catalog
            .record_selection("git status", SelectionAction::Run, Some(&dir))
            .unwrap();

        mode.execute(&fingerprint("git status"), "run_cwd").unwrap();
        let sent = runner.calls().last().cloned().expect("delivered");
        assert_eq!(sent[..4], ["herdr", "pane", "run", "w1:p1"]);
        assert_eq!(
            sent[4],
            format!("cd -- {} && git status", shell_quote(&dir))
        );
    }

    /// A failed delivery is an error for every verb, and nothing is recorded as
    /// selected that never reached the pane.
    #[test]
    fn a_failed_delivery_is_reported_and_not_recorded() {
        let (mut mode, _) = mode_with(MockRunner::new().failing("herdr"));
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        mode.catalog
            .record_selection("git status", SelectionAction::Run, Some(&dir))
            .unwrap();
        let id = fingerprint("git status");
        for action in ["fill", "run", "run_cwd"] {
            assert!(mode.execute(&id, action).is_err(), "{action} must fail");
        }
        let record = mode
            .catalog
            .records()
            .iter()
            .find(|record| record.command == "git status")
            .expect("still catalogued");
        assert_eq!(record.selected_count, 1, "only the seeded selection");
    }

    /// Copy goes to the clipboard and touches no pane; a refused multiline
    /// confirmation stops the run before anything is sent.
    #[test]
    fn copy_uses_the_clipboard_and_a_refused_confirmation_sends_nothing() {
        let (mut mode, runner) = mode_with(MockRunner::new());
        mode.copy = |text| {
            anyhow::ensure!(text == "git status", "copied {text}");
            Ok(())
        };
        mode.execute(&fingerprint("git status"), "copy").unwrap();

        mode.confirm = |_| anyhow::bail!("multiline run cancelled");
        assert!(mode.execute(&fingerprint("git status"), "run").is_err());
        assert!(runner.calls().is_empty(), "{:?}", runner.calls());
    }

    /// Acting on a row that is gone, or with an action nothing answers to, must
    /// be refused by name rather than falling through to another command.
    #[test]
    fn a_vanished_row_or_an_unknown_action_is_refused() {
        let mut mode = command_mode();
        let error = mode.execute("not-a-fingerprint", "copy").unwrap_err();
        assert!(error.to_string().contains("disappeared"), "{error}");

        let id = fingerprint("git status");
        let error = mode.execute(&id, "teleport").unwrap_err();
        assert!(
            error.to_string().contains("unknown command action"),
            "{error}"
        );
    }

    /// The diagnostic placeholder is a message, not a command, so every action
    /// on it is disabled — otherwise Enter would try to run the warning.
    #[test]
    fn the_diagnostic_placeholder_is_not_actionable() {
        let mode = command_mode();
        for action in ["run", "fill", "copy", "forget"] {
            let reason = mode
                .action_disabled_reason("__diagnostic", action)
                .unwrap_or_else(|| panic!("{action} must be disabled on the placeholder"));
            assert!(reason.contains("no safe command"), "{reason}");
        }
        // A real row is untouched by that rule.
        assert!(mode
            .action_disabled_reason(&fingerprint("git status"), "run")
            .is_none());
    }

    /// The chrome the shared picker renders comes from these.
    #[test]
    fn the_mode_declares_its_title_accent_and_layout() {
        let mode = command_mode();
        assert_eq!(mode.title(), "Commands");
        assert_eq!(mode.accent_slot(), "mauve");
        assert!(
            mode.emphasize_head(),
            "the leading word is what the eye hunts for"
        );
        assert_eq!(mode.list_pct(), 58);
        assert!(!mode.is_polling(), "the catalogue is loaded up front");
        assert!(mode.actions().iter().all(|a| !a.key_label.is_empty()));
        crate::picker::assert_follows_prefix_concept("commands", &mode.actions());
    }

    #[test]
    fn starred_is_a_full_action_subset_of_history() {
        let mut mode = command_mode();
        assert_eq!(mode.items().len(), 2);
        assert_eq!(mode.action_bar_rows(), 2);
        assert!(mode.actions().iter().any(|action| action.id == "star"));

        let starred = mode.activate_tab("starred").unwrap();

        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].primary, "cargo test");
        assert_eq!(mode.actions().len(), 7);
    }

    #[test]
    fn unstar_removes_the_row_from_starred_but_not_history() {
        let mut mode = command_mode();
        mode.activate_tab("starred").unwrap();
        let id = fingerprint("cargo test");

        assert!(matches!(
            mode.execute(&id, "star").unwrap(),
            ActionOutcome::StayOpen
        ));
        assert!(mode.items().is_empty());
        assert_eq!(mode.activate_tab("history").unwrap().len(), 2);
    }

    /// The mode as `--commands` builds it, over a catalogue that touched no
    /// state, follows its key table and reloads cleanly.
    #[test]
    fn the_mode_is_built_over_a_loaded_catalogue() {
        let mut cfg = Config::default();
        cfg.keys.insert(
            "commands".into(),
            HashMap::from([("star".into(), "alt-y".into())]),
        );
        let catalog = CommandCatalog::from_sources(
            Vec::new(),
            &[],
            vec![empty_record("ls".into(), String::new())],
            HashSet::new(),
            5_000,
            &[],
            None,
            None,
        )
        .unwrap();
        let mode = CommandMode::with_catalog(&cfg, catalog);
        assert_eq!(mode.items().len(), 1);
        assert!(mode.starred_empty.contains("⌥y"), "{}", mode.starred_empty);
    }
}
