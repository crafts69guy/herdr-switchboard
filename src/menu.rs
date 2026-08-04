//! Central Switchboard menu. It delegates to public plugin actions so direct
//! bindings and menu navigation share one launch contract.

use std::env;
use std::os::unix::process::CommandExt;
use std::process::Command;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::config::Config;
use crate::data::Theme;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema};

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let normal = cfg.common.keymode == "normal";
    picker::run(MenuMode, theme, normal)
}

struct MenuMode;

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
    fn schema(&self) -> FieldSchema {
        FieldSchema::default()
    }
    fn actions(&self) -> Vec<ActionSpec> {
        std::iter::once(ActionSpec {
            id: "open",
            key: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            key_label: "↵",
            label: "open",
            color_slot: "mauve",
        })
        .chain(ROUTES.iter().map(|route| ActionSpec {
            id: route.id,
            key: KeyCode::Char(route.mnemonic),
            modifiers: KeyModifiers::ALT,
            key_label: route.key_label,
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
                document: Document {
                    fuzzy: format!("{} {} {}", route.group, route.title, route.detail),
                    fields: Default::default(),
                },
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
        let error = Command::new("bash")
            .arg(format!("{root}/bin/action.sh"))
            .env("HERDR_PLUGIN_ACTION_ID", route_id)
            .env(
                "SWITCHBOARD_HANDOFF_PANE_ID",
                env::var("HERDR_PANE_ID").unwrap_or_default(),
            )
            .exec();
        Err(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
