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
        trailing_marker: None,
        document: Document::new(
            format!("{} {title} {} {repo} {agent}", pane.pane_id, pane.cwd),
            &[
                ("pane", pane.pane_id.clone()),
                ("title", title),
                ("cwd", pane.cwd.clone()),
                ("repo", repo),
                ("agent", agent),
                ("tab", pane.tab_id.clone()),
            ],
        ),
        preview,
        accent_slot: Some(if zenned { "mauve" } else { "blue" }.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::session::Session;
    use super::*;

    fn pane(id: &str, title: &str, cwd: &str, agent: Option<&str>) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            tab_id: "tab-1".into(),
            workspace_id: "ws-1".into(),
            title: title.into(),
            cwd: cwd.into(),
            agent: agent.map(str::to_string),
            focused: false,
        }
    }

    /// A store pointed at a throwaway path: the real one is shared state, and
    /// `CLAUDE.md` keeps tests off it.
    fn store() -> SessionStore {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        SessionStore::at(
            std::env::temp_dir()
                .join(format!(
                    "switchboard-zen-picker-{}-{}",
                    std::process::id(),
                    NONCE.fetch_add(1, Ordering::Relaxed)
                ))
                .join("zen.tsv"),
        )
    }

    fn mode(panes: Vec<PaneInfo>, session: Option<Session>) -> ZenMode {
        let cfg = Config::default();
        ZenMode {
            cfg: ZenConfig::from(&cfg),
            notifier: Notifier::silent(),
            store: store(),
            bindings: HashMap::new(),
            panes,
            session,
        }
    }

    fn session_on(target: &str, gutters: &[&str]) -> Session {
        Session {
            target: target.into(),
            zen_tab: "tab-zen".into(),
            origin_tab: "tab-1".into(),
            gutters: gutters.iter().map(|g| (*g).to_string()).collect(),
            anchor: None,
        }
    }

    /// A gutter is this plugin's own scaffolding, not somewhere to work, so it
    /// must never appear as something you can zen — doing so would nest zen
    /// inside itself.
    #[test]
    fn the_gutters_of_a_live_session_are_not_offered_as_targets() {
        let session = session_on("w1:p1", &["w1:p5", "w1:p6"]);
        let hidden: Vec<&str> = session.gutters.iter().map(String::as_str).collect();

        let panes = [
            pane("w1:p1", "editor", "/work/api", None),
            pane("w1:p5", "", "/work/api", None),
            pane("w1:p6", "", "/work/api", None),
            pane("w1:p2", "tests", "/work/api", None),
        ];
        let offered: Vec<&str> = panes
            .iter()
            .filter(|p| !hidden.contains(&p.pane_id.as_str()))
            .map(|p| p.pane_id.as_str())
            .collect();

        assert_eq!(offered, ["w1:p1", "w1:p2"]);
    }

    /// The zenned pane is marked in the list it appears in, so the user can see
    /// which one holds the screen before pressing anything.
    #[test]
    fn the_zenned_pane_is_marked_in_the_list() {
        let panes = [
            pane("w1:p1", "editor", "/work/api", None),
            pane("w1:p2", "tests", "/work/api", None),
        ];
        let items: Vec<PickerItem> = panes
            .iter()
            .map(|p| pane_item(p, p.pane_id == "w1:p1"))
            .collect();

        assert_eq!(items[0].trailing.as_deref(), Some("zen"));
        assert_eq!(items[1].trailing, None);
        assert_eq!(items[0].accent_slot.as_deref(), Some("mauve"));
        assert_eq!(items[1].accent_slot.as_deref(), Some("blue"));
    }

    /// A row names the pane, and its trailing tag says the one thing that
    /// changes what the actions mean: whether this pane is the one in zen.
    #[test]
    fn a_pane_row_prefers_zen_over_its_agent_in_the_trailing_tag() {
        let with_agent = pane("w1:p1", "editor", "/work/api", Some("claude"));

        let plain = pane_item(&with_agent, false);
        assert_eq!(plain.primary, "editor");
        assert_eq!(plain.secondary, "w1:p1 · /work/api");
        assert_eq!(plain.trailing.as_deref(), Some("claude"));
        assert_eq!(plain.accent_slot.as_deref(), Some("blue"));
        assert!(!plain.preview.join("\n").contains("state     in zen"));

        // The zen state outranks the agent: it is what decides whether Enter is
        // even available on this row.
        let zenned = pane_item(&with_agent, true);
        assert_eq!(zenned.trailing.as_deref(), Some("zen"));
        assert_eq!(zenned.accent_slot.as_deref(), Some("mauve"));
        assert!(zenned.preview.join("\n").contains("state     in zen"));
    }

    /// A pane with no title would otherwise render a blank primary column, and
    /// one with no agent must not render an empty tag or an `agent` row.
    #[test]
    fn a_pane_with_no_title_falls_back_to_its_id_and_no_agent_adds_no_row() {
        let item = pane_item(&pane("w1:p2", "", "/work/api", None), false);
        assert_eq!(item.primary, "w1:p2");
        assert_eq!(item.trailing, None);
        let card = item.preview.join("\n");
        assert!(card.contains("pane      w1:p2"), "{card}");
        assert!(card.contains("workspace ws-1"), "{card}");
        assert!(!card.contains("agent"), "{card}");
    }

    /// The `repo` field is the last non-empty path segment, so a trailing slash
    /// must not make it empty.
    #[test]
    fn the_repo_field_is_the_last_path_segment_even_with_a_trailing_slash() {
        let mode = mode(Vec::new(), None);
        let schema = mode.schema();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
        let item = pane_item(&pane("w1:p1", "editor", "/work/api/", None), false);

        for query in [
            "repo:api",
            "pane:w1:p1",
            "cwd:/work",
            "dir:/work",
            "tab:tab-1",
        ] {
            let compiled = crate::query::CompiledQuery::compile(query, &schema)
                .unwrap_or_else(|error| panic!("`{query}` did not compile: {error:?}"));
            assert!(
                compiled.score(&item.document, &mut matcher).is_some(),
                "`{query}` matched no pane"
            );
        }
    }

    /// Both actions are conditional on the session, and getting either wrong
    /// strands a pane: exiting when nothing is zenned, or zenning the pane that
    /// already holds the screen.
    #[test]
    fn zen_and_exit_are_each_disabled_by_the_session_state_that_makes_them_wrong() {
        let idle = mode(Vec::new(), None);
        let reason = idle
            .action_disabled_reason("w1:p1", "exit")
            .expect("exit needs a session");
        assert!(reason.contains("no pane is in zen"), "{reason}");
        assert!(idle.action_disabled_reason("w1:p1", "zen").is_none());

        let active = mode(Vec::new(), Some(session_on("w1:p1", &[])));
        let reason = active
            .action_disabled_reason("w1:p1", "zen")
            .expect("the zenned pane cannot re-enter zen");
        assert!(reason.contains("already in zen"), "{reason}");
        // Another pane can still take the screen, and exit is now available.
        assert!(active.action_disabled_reason("w1:p2", "zen").is_none());
        assert!(active.action_disabled_reason("w1:p1", "exit").is_none());
    }

    /// The chrome the shared picker renders comes from these, so a renamed
    /// action is a pill that silently stops existing.
    #[test]
    fn the_mode_declares_its_title_accent_and_both_actions() {
        let mut mode = mode(Vec::new(), None);
        assert_eq!(mode.title(), "Zen");
        assert_eq!(mode.accent_slot(), "mauve");
        let ids: Vec<&str> = mode.actions().iter().map(|action| action.id).collect();
        assert_eq!(ids, ["zen", "exit"]);
        assert!(mode.actions().iter().all(|a| !a.key_label.is_empty()));
        crate::picker::assert_follows_prefix_concept("zen", &mode.actions());
        assert!(mode.key_bindings().is_empty());
        assert!(!mode.is_polling(), "zen has no background source");

        // A settings apply re-reads config without disturbing the session.
        let mut cfg = Config::default();
        cfg.zen.width = 96;
        cfg.keys.insert(
            "zen".into(),
            HashMap::from([("zen".to_string(), "ctrl-z".to_string())]),
        );
        mode.reload_config(&cfg).unwrap();
        assert_eq!(mode.cfg.width, 96);
        assert_eq!(
            mode.key_bindings().get("zen").map(String::as_str),
            Some("ctrl-z")
        );
    }

    /// An unknown action must be refused rather than silently doing nothing —
    /// this arm is what catches a pill whose id drifted from its handler.
    #[test]
    fn an_unknown_action_is_refused_by_name() {
        let mut mode = mode(Vec::new(), None);
        let error = mode.execute("w1:p1", "teleport").unwrap_err();
        assert!(error.to_string().contains("unknown zen action"), "{error}");
        assert!(error.to_string().contains("teleport"), "{error}");
    }
}
