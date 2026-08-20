//! Picker adapter for selecting the pane that enters or exits zen.

use std::collections::HashMap;

use anyhow::{bail, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use super::engine::{enter, leave, list_panes, PaneInfo, ZenConfig};
use super::session::{Session, SessionStore};
use crate::config::Config;
use crate::data::Theme;
use crate::notify::Notifier;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::runner::SystemRunner;

pub(super) fn run(cfg: Config, theme: Theme) -> Result<()> {
    let mode = ZenMode::new(cfg.clone());
    picker::run(mode, theme, cfg)
}

struct ZenMode {
    cfg: ZenConfig,
    notifier: Notifier,
    store: SessionStore,
    bindings: HashMap<String, String>,
    panes: Vec<PaneInfo>,
    session: Option<Session>,
}

impl ZenMode {
    fn new(cfg: Config) -> Self {
        Self {
            cfg: ZenConfig::from(&cfg),
            notifier: Notifier::new(&cfg),
            store: SessionStore::new(),
            bindings: cfg.keys.get("zen").cloned().unwrap_or_default(),
            panes: Vec::new(),
            session: None,
        }
    }

    /// Every pane the user could sensibly zen. The gutters of a live session are
    /// filtered out: they are this plugin's scaffolding, not somewhere to work,
    /// and zenning one would nest zen inside itself.
    fn reload(&mut self) -> Vec<PickerItem> {
        self.session = self.store.load();
        let hidden: Vec<&str> = self
            .session
            .iter()
            .flat_map(|session| session.gutters.iter().map(String::as_str))
            .collect();
        self.panes = list_panes(&SystemRunner)
            .into_iter()
            .filter(|pane| !hidden.contains(&pane.pane_id.as_str()))
            .collect();
        let zenned = self.session.as_ref().map(|s| s.target.clone());
        self.panes
            .iter()
            .map(|pane| pane_item(pane, zenned.as_deref() == Some(&pane.pane_id)))
            .collect()
    }
}

impl PickerMode for ZenMode {
    fn title(&self) -> &str {
        "Zen"
    }
    fn accent_slot(&self) -> &'static str {
        "mauve"
    }
    fn schema(&self) -> FieldSchema {
        FieldSchema::new(
            &[
                ("pane", MatchKind::Exact),
                ("title", MatchKind::Contains),
                ("cwd", MatchKind::Contains),
                ("repo", MatchKind::Contains),
                ("agent", MatchKind::Contains),
                ("tab", MatchKind::Exact),
            ],
            &[("dir", "cwd")],
        )
    }
    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "zen",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵".into(),
                label: "zen",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "exit",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x".into(),
                label: "exit zen",
                color_slot: "peach",
            },
        ]
    }
    fn key_bindings(&self) -> HashMap<String, String> {
        self.bindings.clone()
    }
    fn action_disabled_reason(&self, item_id: &str, action: &str) -> Option<String> {
        match action {
            "exit" if self.session.is_none() => {
                Some("exit is unavailable because no pane is in zen".into())
            }
            "zen" if self.session.as_ref().is_some_and(|s| s.target == item_id) => {
                Some("this pane is already in zen — use exit to bring it back".into())
            }
            _ => None,
        }
    }
    fn reload_config(&mut self, config: &Config) -> Result<()> {
        self.cfg = ZenConfig::from(config);
        self.notifier = Notifier::new(config);
        self.bindings = config.keys.get("zen").cloned().unwrap_or_default();
        Ok(())
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        Ok(self.reload())
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        match action {
            "zen" => {
                // Only one pane can hold the screen; entering while another is
                // zenned would strand the first in its tab.
                if let Some(session) = self.store.load() {
                    leave(&SystemRunner, &session, &self.notifier, &self.store)?;
                }
                enter(
                    &SystemRunner,
                    item_id,
                    &self.cfg,
                    &self.notifier,
                    &self.store,
                )?;
            }
            "exit" => {
                if let Some(session) = self.store.load() {
                    leave(&SystemRunner, &session, &self.notifier, &self.store)?;
                }
            }
            other => bail!("unknown zen action '{other}'"),
        }
        Ok(ActionOutcome::Close)
    }
}

fn pane_item(pane: &PaneInfo, zenned: bool) -> PickerItem {
    let repo = pane
        .cwd
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .to_string();
    let title = if pane.title.is_empty() {
        pane.pane_id.clone()
    } else {
        pane.title.clone()
    };
    let agent = pane.agent.clone().unwrap_or_default();
    let mut preview = vec![
        format!("pane      {}", pane.pane_id),
        format!("tab       {}", pane.tab_id),
        format!("workspace {}", pane.workspace_id),
        format!("title     {title}"),
        format!("cwd       {}", pane.cwd),
    ];
    if !agent.is_empty() {
        preview.push(format!("agent     {agent}"));
    }
    if zenned {
        preview.push("state     in zen".into());
    }

    PickerItem {
        id: pane.pane_id.clone(),
        primary: title.clone(),
        secondary: format!("{} · {}", pane.pane_id, pane.cwd),
        trailing: zenned
            .then(|| "zen".to_string())
            .or_else(|| (!agent.is_empty()).then(|| agent.clone())),
        document: Document {
            fuzzy: format!("{} {title} {} {repo} {agent}", pane.pane_id, pane.cwd),
            fields: picker::fields(&[
                ("pane", pane.pane_id.clone()),
                ("title", title),
                ("cwd", pane.cwd.clone()),
                ("repo", repo),
                ("agent", agent),
                ("tab", pane.tab_id.clone()),
            ]),
        },
        preview,
        accent_slot: Some(if zenned { "mauve" } else { "blue" }.into()),
    }
}
