//! Command Catalog: shell import, Presets, privacy policy and selection history.

use anyhow::Result;

use crate::config::Config;
use crate::data::Theme;

mod action;
mod catalog;
mod history;
mod picker;

#[cfg(test)]
use crate::{config::Preset, state::now};
#[cfg(test)]
use catalog::*;
#[cfg(test)]
use history::*;
#[cfg(test)]
use picker::*;
#[cfg(test)]
use std::{collections::HashSet, env, fs};

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    picker::run(cfg, theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gutter tag reads as a recency gradient, so the unit has to change with
    /// the magnitude rather than run to four digits of minutes.
    #[test]
    fn ago_picks_a_unit_that_keeps_the_gutter_narrow() {
        let t = now();
        assert_eq!(ago(0), "—");
        assert_eq!(ago(t), "now");
        assert_eq!(ago(t - 90), "1m");
        assert_eq!(ago(t - 7_200), "2h");
        assert_eq!(ago(t - 3 * 86_400), "3d");
        assert_eq!(ago(t - 60 * 86_400), "2mo");
        assert_eq!(ago(t - 800 * 86_400), "2y");
        // Whatever the age, the tag stays inside the gutter the list reserves.
        assert!(ago(t - 999 * 86_400).chars().count() <= 4);
    }

    /// The preview used to print `last used  1785811497`, which is not a fact
    /// anyone can read. Verified against the same instants in UTC.
    #[test]
    fn stamp_renders_a_readable_utc_date() {
        assert_eq!(stamp(1_785_811_497), "2026-08-04 02:44");
        assert_eq!(stamp(1_785_203_321), "2026-07-28 01:48");
        assert_eq!(stamp(1), "1970-01-01 00:00");
        assert_eq!(stamp(0), "never");
    }

    /// `shell` is the source of all but a handful of entries, so it earns no
    /// column; a preset is worth pointing at.
    #[test]
    fn only_a_notable_source_earns_a_badge() {
        let item = |sources: &[&str]| {
            let mut record = empty_record("vim".into(), String::new());
            record.sources = sources.iter().map(|s| s.to_string()).collect();
            command_item(&record)
        };
        assert_eq!(item(&["shell"]).secondary, "");
        assert_eq!(item(&["preset"]).secondary, "preset");
        assert_eq!(item(&["switchboard"]).secondary, "switchboard");
    }

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

    #[test]
    fn old_command_state_defaults_to_unstarred() {
        let root = env::temp_dir().join(format!(
            "switchboard-command-old-state-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let history = root.join("commands.json");
        fs::write(
            &history,
            r#"[{
                "command":"cargo test",
                "label":"",
                "sources":["shell"],
                "selected_count":0,
                "last_selected_at":10,
                "last_action":null,
                "recent_cwds":[]
            }]"#,
        )
        .unwrap();

        let records = read_records(&history).unwrap();

        assert_eq!(records.len(), 1);
        assert!(!records[0].starred);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn stars_persist_and_survive_the_ordinary_history_limit() {
        let root =
            env::temp_dir().join(format!("switchboard-command-stars-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let history = root.join("commands.json");
        let mut kept = empty_record(
            "kept even after shell history drops it".into(),
            String::new(),
        );
        kept.starred = true;
        let mut catalog = CommandCatalog::from_sources(
            vec![
                Import {
                    command: "newest ordinary".into(),
                    timestamp: 30,
                },
                Import {
                    command: "older ordinary".into(),
                    timestamp: 20,
                },
            ],
            &[],
            vec![kept],
            HashSet::new(),
            1,
            &[],
            Some(history.clone()),
            None,
        )
        .unwrap();

        assert_eq!(catalog.records().len(), 2);
        assert!(catalog.records().iter().any(|record| record.starred));
        assert_eq!(
            catalog
                .records()
                .iter()
                .filter(|record| !record.starred)
                .count(),
            1
        );

        assert!(catalog.toggle_star("newest ordinary").unwrap());
        let stored = read_records(&history).unwrap();
        assert!(
            stored
                .iter()
                .find(|record| record.command == "newest ordinary")
                .unwrap()
                .starred
        );
        assert!(!catalog.toggle_star("newest ordinary").unwrap());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_failed_star_write_rolls_back_the_catalog() {
        let root = env::temp_dir().join(format!(
            "switchboard-command-star-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let unwritable_history = root.join("commands.json");
        fs::create_dir_all(&unwritable_history).unwrap();
        let mut catalog = CommandCatalog::from_sources(
            vec![Import {
                command: "cargo test".into(),
                timestamp: 10,
            }],
            &[],
            Vec::new(),
            HashSet::new(),
            5_000,
            &[],
            Some(unwritable_history),
            None,
        )
        .unwrap();

        assert!(catalog.toggle_star("cargo test").is_err());
        assert!(!catalog.records()[0].starred);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_star_uses_a_fixed_gutter_slot_and_preview_fact() {
        let mut plain = empty_record("cargo test".into(), String::new());
        let plain_item = command_item(&plain);
        plain.starred = true;
        let starred_item = command_item(&plain);

        assert_eq!(
            plain_item.trailing.as_ref().unwrap().chars().count(),
            starred_item.trailing.as_ref().unwrap().chars().count()
        );
        assert!(plain_item.trailing_marker.is_none());
        assert!(starred_item.trailing_marker.is_some());
        assert!(starred_item
            .preview
            .iter()
            .any(|line| line == "starred  yes"));
    }
}
