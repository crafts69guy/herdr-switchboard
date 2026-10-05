//! Central Switchboard menu. It delegates to public plugin actions so direct
//! bindings and menu navigation share one launch contract.

use std::collections::HashMap;
use std::env;
use std::os::unix::process::CommandExt;
use std::process::{self, Command, Stdio};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};

use crate::config::Config;
use crate::data::Theme;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema};

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    main_in(&mut crate::surface::TerminalHost, cfg, theme)
}

fn main_in(host: &mut impl crate::surface::Host, cfg: Config, theme: Theme) -> Result<()> {
    let mode = MenuMode::new(&cfg);
    picker::run_in(host, mode, theme, cfg)
}

struct MenuMode {
    bindings: HashMap<String, String>,
    /// Starts the detached handoff; a test swaps in one that starts nothing.
    launch: fn(&mut Command) -> std::io::Result<()>,
}

impl MenuMode {
    fn new(cfg: &Config) -> Self {
        Self {
            bindings: cfg.keys.get("menu").cloned().unwrap_or_default(),
            launch: |command| command.spawn().map(drop),
        }
    }
}

fn handoff_command(root: &str, route_id: &str, origin_pane: &str, parent_pid: u32) -> Command {
    let mut command = Command::new("bash");
    command
        .arg(format!("{root}/bin/action.sh"))
        .env("HERDR_PLUGIN_ACTION_ID", route_id)
        .env("SWITCHBOARD_ORIGIN_PANE_ID", origin_pane)
        .env("SWITCHBOARD_HANDOFF_PARENT_PID", parent_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

#[derive(Clone, Copy)]
struct Route {
    id: &'static str,
    group: &'static str,
    title: &'static str,
    detail: &'static str,
    color: &'static str,
    mnemonic: char,
    key_label: &'static str,
}

const ROUTES: &[Route] = &[
    Route {
        id: "projects",
        group: "Pickers",
        title: "Projects",
        detail: "repos, worktrees, agents and workspaces",
        color: "peach",
        mnemonic: 'p',
        key_label: "⌥p",
    },
    Route {
        id: "agents",
        group: "Pickers",
        title: "AI Agents",
        detail: "start an installed AI integration",
        color: "mauve",
        mnemonic: 'a',
        key_label: "⌥a",
    },
    Route {
        id: "commands",
        group: "Pickers",
        title: "Commands",
        detail: "shell history and command presets",
        color: "blue",
        mnemonic: 'c',
        key_label: "⌥c",
    },
    Route {
        id: "ports",
        group: "Pickers",
        title: "Ports",
        detail: "live TCP listeners and owner processes",
        color: "teal",
        mnemonic: 'o',
        key_label: "⌥o",
    },
    Route {
        id: "fnm",
        group: "Utilities",
        title: "Node Versions",
        detail: "use, install and remove versions with fnm",
        color: "green",
        mnemonic: 'n',
        key_label: "⌥n",
    },
    Route {
        id: "zen",
        group: "Pickers",
        title: "Zen",
        detail: "give one pane the screen, centred and flanked",
        color: "mauve",
        mnemonic: 'z',
        key_label: "⌥z",
    },
    Route {
        id: "usage",
        group: "Utilities",
        title: "Usage",
        detail: "subscription quota for your AI agents",
        color: "teal",
        mnemonic: 'q',
        key_label: "⌥q",
    },
    Route {
        id: "git",
        group: "Utilities",
        title: "Git",
        detail: "review or stage the current repository",
        color: "green",
        mnemonic: 'g',
        key_label: "⌥g",
    },
    Route {
        id: "clone",
        group: "Utilities",
        title: "Clone",
        detail: "get a repository and open it",
        color: "mauve",
        mnemonic: 'l',
        key_label: "⌥l",
    },
    Route {
        id: "settings",
        group: "Utilities",
        title: "Settings",
        detail: "configure every Switchboard picker",
        color: "yellow",
        mnemonic: 's',
        key_label: "⌥s",
    },
    Route {
        id: "changelog",
        group: "Utilities",
        title: "Changelog",
        detail: "read installed release notes",
        color: "blue",
        mnemonic: 'h',
        key_label: "⌥h",
    },
    Route {
        id: "update",
        group: "Utilities",
        title: "Update",
        detail: "install the newest tagged release",
        color: "green",
        mnemonic: 'u',
        key_label: "⌥u",
    },
];

impl PickerMode for MenuMode {
    fn title(&self) -> &str {
        "Switchboard"
    }
    fn accent_slot(&self) -> &'static str {
        "mauve"
    }
    fn action_bar_rows(&self) -> u16 {
        2
    }
    fn schema(&self) -> FieldSchema {
        FieldSchema::default()
    }
    fn key_bindings(&self) -> HashMap<String, String> {
        self.bindings.clone()
    }
    fn reload_config(&mut self, cfg: &Config) -> Result<()> {
        self.bindings = cfg.keys.get("menu").cloned().unwrap_or_default();
        Ok(())
    }
    fn actions(&self) -> Vec<ActionSpec> {
        std::iter::once(ActionSpec {
            id: "open",
            key: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            key_label: "↵".into(),
            label: "open",
            color_slot: "mauve",
        })
        .chain(ROUTES.iter().map(|route| ActionSpec {
            id: route.id,
            key: KeyCode::Char(route.mnemonic),
            modifiers: KeyModifiers::ALT,
            key_label: route.key_label.into(),
            label: route.title,
            color_slot: route.color,
        }))
        .collect()
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        Ok(ROUTES
            .iter()
            .map(|route| PickerItem {
                id: route.id.into(),
                primary: route.title.into(),
                secondary: format!("{} · {}", route.group, route.detail),
                trailing: None,
                trailing_marker: None,
                document: Document::fuzzy(format!(
                    "{} {} {}",
                    route.group, route.title, route.detail
                )),
                preview: vec![
                    route.group.into(),
                    String::new(),
                    route.title.into(),
                    route.detail.into(),
                    String::new(),
                    format!("accent: {}", route.color),
                ],
                accent_slot: Some(route.color.into()),
            })
            .collect())
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        let route_id = if action == "open" { item_id } else { action };
        anyhow::ensure!(
            ROUTES.iter().any(|route| route.id == route_id),
            "unknown route {route_id}"
        );
        let root = env::var("HERDR_PLUGIN_ROOT").unwrap_or_else(|_| ".".into());
        let mut command = handoff_command(
            &root,
            route_id,
            &env::var("HERDR_PANE_ID").unwrap_or_default(),
            process::id(),
        );
        (self.launch)(&mut command)
            .with_context(|| format!("could not schedule {route_id} handoff"))?;
        Ok(ActionOutcome::Close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn central_routes_have_matching_direct_actions_and_bash_routes() {
        let manifest = include_str!("../herdr-plugin.toml");
        let action = include_str!("../bin/action.sh");
        for route in ROUTES {
            assert!(
                manifest.contains(&format!("id = \"{}\"", route.id)),
                "missing manifest action {}",
                route.id
            );
            assert!(
                action.contains(&format!("{}) entrypoint=\"{}\"", route.id, route.id)),
                "missing bash route {}",
                route.id
            );
        }
        let mnemonics = ROUTES
            .iter()
            .map(|route| route.mnemonic)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            mnemonics.len(),
            ROUTES.len(),
            "route mnemonics must be unique"
        );
    }

    /// The Menu was one of two pickers with no `[keys.*]` table at all, so its
    /// routes were the only actions in the plugin nobody could rebind.
    #[test]
    fn a_menu_route_can_be_rebound_like_any_other_action() {
        let mut cfg = Config::default();
        cfg.keys
            .entry("menu".into())
            .or_default()
            .insert("git".into(), "alt-b".into());
        let mode = MenuMode::new(&cfg);
        assert_eq!(
            mode.key_bindings().get("git").map(String::as_str),
            Some("alt-b")
        );
    }

    #[test]
    fn the_full_action_bar_fits_the_menu_popup() {
        let actions = MenuMode::new(&Config::default()).actions();
        let mut pills = actions
            .iter()
            .map(|action| crate::tui::Pill::new(&action.key_label, action.label, Color::Reset))
            .collect::<Vec<_>>();
        pills.push(crate::tui::Pill::new("⌥,", "settings", Color::Reset));
        pills.push(crate::tui::Pill::new("esc", "mode/close", Color::Reset));
        let (spans, _) = crate::tui::pill_row(&pills, Color::Reset, 0);
        let action_bar_width = spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum::<usize>();

        // Herdr's two border columns sit outside the TUI's drawable area. The
        // two balanced rows share the total capacity of a 112-column popup.
        assert!(
            action_bar_width <= 2 * 110,
            "{action_bar_width}-column action bar exceeds two 110-column rows"
        );
        assert_eq!(MenuMode::new(&Config::default()).action_bar_rows(), 2);
        crate::picker::assert_follows_prefix_concept("menu", &actions);
    }

    #[test]
    fn menu_handoff_runs_detached_after_the_menu_process_exits() {
        let command = handoff_command("/plugin", "agents", "w1:p1", 42);
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let env = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(command.get_program(), "bash");
        assert_eq!(args, ["/plugin/bin/action.sh"]);
        assert_eq!(env["HERDR_PLUGIN_ACTION_ID"], "agents");
        assert_eq!(env["SWITCHBOARD_ORIGIN_PANE_ID"], "w1:p1");
        assert_eq!(env["SWITCHBOARD_HANDOFF_PARENT_PID"], "42");
    }

    fn quiet_menu() -> MenuMode {
        let mut cfg = Config::default();
        cfg.keys.insert(
            "menu".into(),
            HashMap::from([("zen".into(), "alt-y".into())]),
        );
        let mut mode = MenuMode::new(&cfg);
        mode.launch = |_| Ok(());
        mode
    }

    /// The menu lists every route with an open action and a direct chord for
    /// each, follows its `[keys.menu]` table, and reloads it.
    #[test]
    fn the_menu_lists_every_route_with_a_chord_and_follows_its_bindings() {
        let mut mode = quiet_menu();
        assert_eq!(mode.title(), "Switchboard");
        assert_eq!(mode.accent_slot(), "mauve");
        assert_eq!(mode.action_bar_rows(), 2);
        let _ = mode.schema();
        assert_eq!(mode.actions().len(), ROUTES.len() + 1);
        assert_eq!(
            mode.key_bindings().get("zen").map(String::as_str),
            Some("alt-y")
        );
        let items = mode.initial().unwrap();
        assert_eq!(items.len(), ROUTES.len());
        assert!(items[0].secondary.contains(" · "));

        mode.reload_config(&Config::default()).unwrap();
        assert!(mode.key_bindings().is_empty());
    }

    /// Enter opens the selected route and a chord opens its own; an unknown
    /// route is refused before anything is scheduled, and a failed launch says so.
    #[test]
    fn executing_schedules_a_known_route_and_refuses_the_rest() {
        let mut mode = quiet_menu();
        assert!(matches!(
            mode.execute("projects", "open").unwrap(),
            ActionOutcome::Close
        ));
        assert!(matches!(
            mode.execute("ignored", "usage").unwrap(),
            ActionOutcome::Close
        ));
        assert!(mode.execute("teleport", "open").is_err());

        mode.launch = |_| Err(std::io::Error::other("no fork"));
        let error = mode.execute("projects", "open").unwrap_err();
        assert!(error.to_string().contains("projects"), "{error}");
    }

    /// The menu as `--menu` hosts it closes on `esc` without scheduling anything.
    #[test]
    fn the_menu_closes_on_esc() {
        use crate::surface::ScriptedHost;
        main_in(
            &mut ScriptedHost::new([ScriptedHost::key(KeyCode::Esc, KeyModifiers::NONE)]),
            Config::default(),
            Theme::default(),
        )
        .expect("closes");
    }
}
