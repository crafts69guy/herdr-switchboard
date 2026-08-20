//! Picker adapter for command search and selection actions.

use std::collections::HashMap;
use std::env;
use std::path::Path;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use super::action::{confirm_multiline, copy_text, send_to_pane, shell_quote};
use super::catalog::{ago, fingerprint, stamp, CommandCatalog, CommandRecord, SelectionAction};
use crate::config::Config;
use crate::data::Theme;
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};

pub(super) fn run(cfg: Config, theme: Theme) -> Result<()> {
    let mode = CommandMode::new(&cfg)?;
    picker::run(mode, theme, cfg)
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
                trailing: None,
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
    fn emphasize_head(&self) -> bool {
        true
    }
    /// Rows here are whole shell commands and the card beside them is a short
    /// metadata block, so the split that suits Ports leaves this list truncating
    /// against a mostly empty preview.
    fn list_pct(&self) -> u16 {
        58
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
        trailing: Some(ago(record.last_selected_at)),
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
