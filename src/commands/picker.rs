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
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::picker::{
    self, ActionOutcome, ActionSpec, PickerItem, PickerMarker, PickerMode, PickerTab,
};
use crate::query::{Document, FieldSchema, MatchKind};

pub(super) fn run(cfg: Config, theme: Theme) -> Result<()> {
    let mode = CommandMode::new(&cfg)?;
    picker::run(mode, theme, cfg)
}

struct CommandMode {
    catalog: CommandCatalog,
    tab: CommandTab,
    origin_pane: String,
    origin_cwd: Option<String>,
    notifier: Notifier,
    bindings: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandTab {
    History,
    Starred,
}

impl CommandMode {
    fn new(cfg: &Config) -> Result<Self> {
        Ok(Self {
            catalog: CommandCatalog::load(cfg)?,
            tab: CommandTab::History,
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
            CommandTab::Starred => "No stars — ctrl-s in History",
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
                key_label: "^s".into(),
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
    use crate::commands::catalog::{empty_record, CommandCatalog, SelectionAction};

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
            bindings: HashMap::new(),
        }
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
            mode.empty_message().contains("ctrl-s"),
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
}
