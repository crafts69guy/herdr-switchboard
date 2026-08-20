//! Zen mode: give one pane the screen, centred between two dark gutters.
//!
//! # Why it is built this way
//!
//! The obvious implementation is `herdr pane zoom`, and it does not work.
//! Measured against herdr 0.8.0: **splitting a zoomed tab silently cancels the
//! zoom** (`zoomed` flips back to `false` the moment a gutter is created), and
//! `pane layout` on a zoomed tab reports the *underlying* rects rather than the
//! rendered ones, so the gutters would be sized off the wrong geometry anyway.
//! Zoom and gutters cannot coexist.
//!
//! So zen moves the target pane into a tab of its own instead. `pane move
//! --new-tab` preserves the pane's `terminal_id`, which means the running
//! process survives the move, and — the reason this shape was chosen over
//! stashing the *other* panes — **no pane but the target is ever touched**. The
//! neighbours keep running, undisturbed, in the original tab.
//!
//! # The centring recipe
//!
//! `pane split` can only place a new pane to the right or below, and `--ratio R`
//! gives the ratio to the **existing** pane, not the new one. Both facts are
//! counter-intuitive and both were measured. Centring therefore takes three
//! calls, for a target width `w` and gutter `g = (1 - w) / 2`:
//!
//! ```text
//! 1. split target right, ratio 1-g      [ target 1-g | gutter g ]
//! 2. split target right, ratio g/(1-g)  [ target g | gutter w | gutter g ]
//! 3. swap target right                  [ gutter g | target w | gutter g ]
//! ```
//!
//! Step 2 leaves the target in the *left gutter slot* and a blank pane in the
//! middle; the swap is what puts the target where the eye expects it.
//!
//! # The scrim
//!
//! Gutters are dimmed with [`crate::socket`], not with any herdr styling: herdr
//! has no pane opacity, no dim-inactive, and no per-pane background. Painting a
//! scrim over the *gutters* rather than over live neighbours is deliberate — a
//! parked gutter never redraws, so the scrim cannot be punched through by a log
//! tail. herdr composites it opaque regardless of the alpha byte, which over a
//! blank pane reads as void, which is the intended look.

use anyhow::{bail, Context, Result};

use crate::chrome;
use crate::config::Config;
use crate::data::Theme;
use crate::notify::Notifier;
use crate::runner::CommandRunner;
use crate::state;

mod engine;
mod geometry;
mod picker;
mod session;

use engine::{enter, leave, toggle, ZenConfig};
use session::SessionStore;

#[cfg(test)]
use geometry::*;
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use session::*;
#[cfg(test)]
use std::fs;

/// Where the toggle remembers an active session. Written last on the way in and
/// deleted first on the way out, so a crash mid-enter leaves no file claiming a
/// zen that is not there.
pub fn cli<R: CommandRunner>(runner: &R, args: &[String], cfg: &Config) -> Result<()> {
    let verb = args.first().map(String::as_str).unwrap_or("toggle");
    let pane = args
        .windows(2)
        .find(|pair| pair[0] == "--pane")
        .map(|pair| pair[1].clone())
        .or_else(|| std::env::var("SWITCHBOARD_ORIGIN_PANE_ID").ok())
        .filter(|pane| !pane.is_empty());
    let zen = ZenConfig::from(cfg);
    let notifier = Notifier::new(cfg);
    let store = SessionStore::new();

    match verb {
        "off" => match store.load() {
            Some(session) => leave(runner, &session, &notifier, &store),
            None => Ok(()),
        },
        "on" | "toggle" => {
            let pane =
                pane.context("zen needs a pane id (--pane or SWITCHBOARD_ORIGIN_PANE_ID)")?;
            if verb == "on" {
                if let Some(session) = store.load() {
                    leave(runner, &session, &notifier, &store)?;
                }
                enter(runner, &pane, &zen, &notifier, &store).map(|_| ())
            } else {
                toggle(runner, &pane, &zen, &notifier, &store)
            }
        }
        // The escape hatch for a zen that never got to clean up: herdr killed
        // mid-session, or a restore herdr refused. Needs no pane and no session.
        "chrome-restore" => {
            let overrides = store.load_chrome();
            if overrides.is_empty() {
                return Ok(());
            }
            anyhow::ensure!(
                chrome::disengage(runner, &overrides),
                "could not rewrite {} — the original is saved as {}",
                chrome::config_path().display(),
                state::state_file(chrome::BACKUP)
                    .unwrap_or_default()
                    .display()
            );
            store.clear_chrome();
            Ok(())
        }
        other => bail!("unknown zen verb '{other}' (expected toggle, on, off, or chrome-restore)"),
    }
}

// ---------------------------------------------------------------------------
// The `--zen` picker mode
// ---------------------------------------------------------------------------

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    picker::run(cfg, theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    /// A store pointed at a unique temp path. Never the real state dir: these
    /// tests write and delete the session file for real.
    fn temp_store() -> SessionStore {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "switchboard-zen-test-{}-{}.tsv",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        SessionStore::at(path)
    }

    const PANES: &str = r#"{"result":{"panes":[
        {"pane_id":"w1:p1","tab_id":"w1:t1","workspace_id":"w1","terminal_title":"nvim","cwd":"/repo","focused":true},
        {"pane_id":"w1:p2","tab_id":"w1:t1","workspace_id":"w1","terminal_title":"tests","cwd":"/repo"}
    ]}}"#;

    const LAYOUT: &str = r#"{"result":{"layout":{"area":{"x":0,"y":0,"width":200,"height":50},
        "panes":[{"pane_id":"w1:p1","rect":{"x":0,"y":0,"width":100,"height":50}},
                 {"pane_id":"w1:p2","rect":{"x":100,"y":0,"width":100,"height":50}}],
        "zoomed":false}}}"#;

    // --- geometry ---------------------------------------------------------

    #[test]
    fn gutter_ratios_centre_the_target() {
        let (right, left) = gutter_ratios(70);
        // Target keeps 85% of the tab, then 15/85 of that, leaving 70% centred.
        assert!((right - 0.85).abs() < 0.001, "{right}");
        assert!((left - 0.17647).abs() < 0.001, "{left}");
        // The composition is what matters: 0.85 * (1 - 0.17647) == 0.70.
        assert!((right * (1.0 - left) - 0.70).abs() < 0.001);
    }

    #[test]
    fn gutter_ratios_compose_to_the_requested_width() {
        for width in [30u16, 50, 70, 80, 95] {
            let (right, left) = gutter_ratios(width);
            let actual = right * (1.0 - left);
            let expected = width as f32 / 100.0;
            assert!((actual - expected).abs() < 0.001, "{width}: {actual}");
        }
    }

    #[test]
    fn an_absurd_width_is_clamped_rather_than_refused() {
        // Zen should look wrong, not fail to open, on a nonsense config.
        let (right, _) = gutter_ratios(0);
        assert!(right < 1.0 && right > 0.0);
        assert_eq!(gutter_ratios(500), gutter_ratios(95));
    }

    /// The tab the e2e run used: `p7 | (pA over pB)`. Exported verbatim from
    /// herdr 0.8.0's `layout.export`.
    fn nested_tree() -> Value {
        serde_json::json!({
            "type": "split", "direction": "right", "ratio": 0.5,
            "first": { "type": "pane", "pane_id": "w1:p7" },
            "second": {
                "type": "split", "direction": "down", "ratio": 0.5,
                "first": { "type": "pane", "pane_id": "w1:pA" },
                "second": { "type": "pane", "pane_id": "w1:pB" }
            }
        })
    }

    #[test]
    fn a_two_pane_tab_restores_exactly() {
        let tree = serde_json::json!({
            "type": "split", "direction": "right", "ratio": 0.4,
            "first": { "type": "pane", "pane_id": "w1:p1" },
            "second": { "type": "pane", "pane_id": "w1:p2" }
        });
        let anchor = sibling_anchor(&tree, "w1:p1").unwrap();
        assert_eq!(anchor.pane, "w1:p2");
        assert_eq!(anchor.split, "right");
        assert!(anchor.target_first);
        assert!((anchor.ratio - 0.4).abs() < 0.001);
        assert!(anchor.exact, "a leaf sibling can be rebuilt exactly");
    }

    #[test]
    fn a_target_sitting_second_owns_the_remaining_ratio() {
        let tree = serde_json::json!({
            "type": "split", "direction": "down", "ratio": 0.3,
            "first": { "type": "pane", "pane_id": "w1:p1" },
            "second": { "type": "pane", "pane_id": "w1:p2" }
        });
        // The exported ratio always describes `first`, so p2's share is 0.7.
        let anchor = sibling_anchor(&tree, "w1:p2").unwrap();
        assert!(!anchor.target_first);
        assert_eq!(anchor.split, "down");
        assert!((anchor.ratio - 0.7).abs() < 0.001, "{}", anchor.ratio);
        assert!(anchor.exact);
    }

    #[test]
    fn a_subtree_sibling_is_reported_inexact_rather_than_pretended_exact() {
        // This is the case the e2e run caught: p7's sibling is a whole subtree,
        // and `pane move --target-pane` can only rejoin it next to one pane,
        // nesting p7 a level too deep. Zen still restores every pane — it just
        // must not claim the arrangement is identical.
        let anchor = sibling_anchor(&nested_tree(), "w1:p7").unwrap();
        assert_eq!(anchor.pane, "w1:pA", "the nearest leaf of the sibling");
        assert!(anchor.target_first);
        assert!(!anchor.exact, "a subtree sibling cannot be rebuilt exactly");
    }

    #[test]
    fn a_pane_nested_inside_a_subtree_finds_its_own_sibling() {
        // pA and pB are each other's siblings deeper in the tree; zenning one
        // must anchor on the other, not on the unrelated p7.
        let anchor = sibling_anchor(&nested_tree(), "w1:pA").unwrap();
        assert_eq!(anchor.pane, "w1:pB");
        assert_eq!(anchor.split, "down");
        assert!(anchor.target_first);
        assert!(anchor.exact, "both are leaves of the same split");
    }

    #[test]
    fn a_lone_pane_has_no_sibling_to_anchor_on() {
        let tree = serde_json::json!({ "type": "pane", "pane_id": "w1:p1" });
        assert_eq!(sibling_anchor(&tree, "w1:p1"), None);
        // A pane that is not in the tree at all must not match something else.
        assert_eq!(sibling_anchor(&nested_tree(), "w1:pZZ"), None);
    }

    #[test]
    fn a_side_by_side_neighbour_restores_as_a_right_split() {
        let target = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 50,
        };
        let other = Rect {
            x: 60,
            y: 0,
            width: 140,
            height: 50,
        };
        let anchor = anchor_between(target, other, "w1:p2");
        assert_eq!(anchor.split, "right");
        assert!(anchor.target_first, "target sat left of the anchor");
        assert!((anchor.ratio - 0.3).abs() < 0.001, "{}", anchor.ratio);
    }

    #[test]
    fn a_stacked_neighbour_restores_as_a_down_split() {
        let target = Rect {
            x: 0,
            y: 30,
            width: 200,
            height: 20,
        };
        let other = Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 30,
        };
        let anchor = anchor_between(target, other, "w1:p2");
        assert_eq!(anchor.split, "down");
        assert!(!anchor.target_first, "target sat below the anchor");
    }

    // --- state file -------------------------------------------------------

    #[test]
    fn a_session_survives_a_round_trip() {
        let session = Session {
            target: "w1:p1".into(),
            zen_tab: "w1:t9".into(),
            origin_tab: "w1:t1".into(),
            gutters: vec!["w1:p5".into(), "w1:p6".into()],
            anchor: Some(Anchor {
                pane: "w1:p2".into(),
                split: "right".into(),
                ratio: 0.5,
                target_first: true,
                exact: true,
            }),
        };
        assert_eq!(decode(&encode(&session)), Some(session));
    }

    #[test]
    fn a_session_without_an_anchor_round_trips() {
        let session = Session {
            target: "w1:p1".into(),
            zen_tab: "w1:t9".into(),
            origin_tab: "w1:t1".into(),
            gutters: vec![],
            anchor: None,
        };
        assert_eq!(decode(&encode(&session)), Some(session));
    }

    #[test]
    fn a_store_round_trips_a_session_through_a_real_file() {
        let store = temp_store();
        assert_eq!(store.load(), None, "a fresh store holds nothing");
        let session = session_of(&["w1:p5"]);
        store.save(&session).unwrap();
        assert_eq!(store.load(), Some(session));
        store.clear();
        assert_eq!(store.load(), None, "clear must leave no session behind");
    }

    #[test]
    fn every_store_is_its_own_file() {
        // The reason `SessionStore` is passed in rather than resolved from the
        // state dir: these tests really do delete the file, and against the
        // default path they would delete the user's live session.
        let (a, b) = (temp_store(), temp_store());
        a.save(&session_of(&[])).unwrap();
        assert!(a.load().is_some());
        assert!(b.load().is_none(), "stores must not share a path");
    }

    #[test]
    fn a_truncated_state_file_is_rejected_rather_than_half_applied() {
        // A half-written file must read as "no session", so the toggle enters
        // cleanly instead of trying to undo a zen it cannot describe.
        assert_eq!(decode("target\tw1:p1\n"), None);
        assert_eq!(decode(""), None);
        assert_eq!(decode("garbage"), None);
    }

    // --- the chrome snapshot -----------------------------------------------

    #[test]
    fn a_chrome_snapshot_round_trips_including_the_absent_marker() {
        let overrides = vec![
            chrome::Override {
                key: "pane_borders".into(),
                want: "false".into(),
                prior: Some("true".into()),
            },
            chrome::Override {
                key: "pane_gaps".into(),
                want: "false".into(),
                // Absent from herdr's config: restoring must *remove* the key.
                prior: None,
            },
            chrome::Override {
                key: "sidebar_collapsed_mode".into(),
                want: "\"hidden\"".into(),
                prior: Some("\"compact\"".into()),
            },
        ];
        assert_eq!(decode_chrome(&encode_chrome(&overrides)), overrides);
    }

    #[test]
    fn the_chrome_snapshot_outlives_the_session_it_came_from() {
        // The two records have different lifetimes: clearing the session must
        // not drop the only copy of the user's `[ui]` values.
        let store = temp_store();
        assert!(
            store.load_chrome().is_empty(),
            "a fresh store holds nothing"
        );
        let overrides = vec![chrome::Override {
            key: "pane_borders".into(),
            want: "false".into(),
            prior: Some("true".into()),
        }];
        store.save_chrome(&overrides).unwrap();
        store.save(&session_of(&[])).unwrap();
        store.clear();
        assert_eq!(store.load_chrome(), overrides);
        store.clear_chrome();
        assert!(store.load_chrome().is_empty());
    }

    // --- enter ------------------------------------------------------------

    fn entering() -> MockRunner {
        MockRunner::new()
            .on("pane list", PANES)
            .on("pane layout", LAYOUT)
            .on(
                "pane move w1:p1 --new-tab",
                r#"{"result":{"move_result":{"created_tab":{"tab_id":"w1:t9"}}}}"#,
            )
            .on(
                "--ratio 0.8500",
                r#"{"result":{"pane":{"pane_id":"w1:p5"}}}"#,
            )
            .on(
                "--ratio 0.1765",
                r#"{"result":{"pane":{"pane_id":"w1:p6"}}}"#,
            )
    }

    fn cfg(scrim: bool) -> ZenConfig {
        ZenConfig {
            width: 70,
            scrim,
            color: [0, 0, 0, 255],
            // Never `Full` here: a test must not rewrite the developer's own
            // herdr config, the way it must not delete their live session.
            chrome: chrome::Level::Off,
        }
    }

    #[test]
    fn entering_moves_the_target_out_then_builds_two_gutters_and_swaps() {
        let runner = entering();
        let session = enter(
            &runner,
            "w1:p1",
            &cfg(false),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();

        assert_eq!(session.zen_tab, "w1:t9");
        assert_eq!(session.origin_tab, "w1:t1");
        assert_eq!(session.gutters, vec!["w1:p5", "w1:p6"]);

        let calls: Vec<String> = runner.calls().iter().map(|c| c.join(" ")).collect();
        let move_at = calls.iter().position(|c| c.contains("--new-tab")).unwrap();
        let split_at = calls.iter().position(|c| c.contains("pane split")).unwrap();
        let swap_at = calls.iter().position(|c| c.contains("pane swap")).unwrap();
        // Order is the whole design: the target must be alone before it is
        // flanked, and the swap must come after both gutters exist.
        assert!(move_at < split_at, "{calls:#?}");
        assert!(split_at < swap_at, "{calls:#?}");
        assert!(calls[swap_at].contains("--direction right"));
    }

    #[test]
    fn zen_chrome_off_never_touches_herdrs_config() {
        // The default. Nothing is written and herdr is never asked to reload,
        // which is what keeps a plain zen a pane operation and nothing more.
        let runner = entering();
        let store = temp_store();
        enter(&runner, "w1:p1", &cfg(false), &Notifier::silent(), &store).unwrap();
        let calls: Vec<String> = runner.calls().iter().map(|c| c.join(" ")).collect();
        assert!(
            !calls.iter().any(|c| c.contains("reload-config")),
            "{calls:#?}"
        );
        assert!(store.load_chrome().is_empty());
    }

    #[test]
    fn entering_records_the_way_home_before_the_pane_moves() {
        let runner = entering();
        let session = enter(
            &runner,
            "w1:p1",
            &cfg(false),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();
        let anchor = session.anchor.expect("neighbour should have been recorded");
        assert_eq!(anchor.pane, "w1:p2");
        assert_eq!(anchor.split, "right");
        assert!(anchor.target_first);
    }

    #[test]
    fn entering_parks_each_gutter_so_no_prompt_shows_through() {
        let runner = entering();
        enter(
            &runner,
            "w1:p1",
            &cfg(false),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();
        let parked = runner
            .calls()
            .iter()
            .filter(|c| c.join(" ").contains("pane run") && c.join(" ").contains("sleep"))
            .count();
        assert_eq!(parked, 2, "both gutters should be parked");
    }

    #[test]
    fn entering_an_unknown_pane_fails_before_moving_anything() {
        let runner = entering();
        assert!(enter(
            &runner,
            "w1:pZZ",
            &cfg(false),
            &Notifier::silent(),
            &temp_store()
        )
        .is_err());
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.join(" ").contains("pane move")),
            "nothing may move when the target does not exist"
        );
    }

    #[test]
    fn a_failed_split_leaves_a_usable_session_rather_than_erroring() {
        // The target is already alone on screen at this point, which is most of
        // the value; a missing gutter must not fail the whole enter.
        let runner = MockRunner::new()
            .on("pane list", PANES)
            .on("pane layout", LAYOUT)
            .on(
                "pane move w1:p1 --new-tab",
                r#"{"result":{"move_result":{"created_tab":{"tab_id":"w1:t9"}}}}"#,
            )
            .failing("pane split");
        let session = enter(
            &runner,
            "w1:p1",
            &cfg(false),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();
        assert!(session.gutters.is_empty());
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.join(" ").contains("pane swap")),
            "no swap without a pair of gutters"
        );
    }

    // --- leave ------------------------------------------------------------

    fn session_of(gutters: &[&str]) -> Session {
        Session {
            target: "w1:p1".into(),
            zen_tab: "w1:t9".into(),
            origin_tab: "w1:t1".into(),
            gutters: gutters.iter().map(|g| g.to_string()).collect(),
            anchor: Some(Anchor {
                pane: "w1:p2".into(),
                split: "right".into(),
                ratio: 0.5,
                target_first: true,
                exact: true,
            }),
        }
    }

    #[test]
    fn leaving_closes_the_gutters_and_moves_the_target_home() {
        let runner = MockRunner::new()
            .on("pane list", PANES)
            .on("tab get", r#"{"result":{"tab":{"pane_count":0}}}"#);
        leave(
            &runner,
            &session_of(&[]),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();

        let calls: Vec<String> = runner.calls().iter().map(|c| c.join(" ")).collect();
        let home = calls
            .iter()
            .find(|c| c.contains("pane move w1:p1"))
            .expect("target should move home");
        assert!(home.contains("--tab w1:t1"), "{home}");
        assert!(home.contains("--target-pane w1:p2"), "{home}");
        assert!(home.contains("--split right"), "{home}");
        // It was originally the left of the pair, and `pane move` inserts after.
        assert!(calls
            .iter()
            .any(|c| c.contains("pane swap") && c.contains("--direction left")));
    }

    #[test]
    fn leaving_skips_gutters_the_user_already_closed() {
        // w1:p5 is not in the live pane list, so closing it would be an error
        // herdr would reject; the exit must simply not try.
        let runner = MockRunner::new()
            .on("pane list", PANES)
            .on("tab get", r#"{"result":{"tab":{"pane_count":0}}}"#);
        leave(
            &runner,
            &session_of(&["w1:p5"]),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.join(" ").contains("pane close")),
            "a dead gutter must not be closed again"
        );
    }

    #[test]
    fn leaving_keeps_a_zen_tab_the_user_put_something_in() {
        let runner = MockRunner::new()
            .on("pane list", PANES)
            .on("tab get", r#"{"result":{"tab":{"pane_count":2}}}"#);
        leave(
            &runner,
            &session_of(&[]),
            &Notifier::silent(),
            &temp_store(),
        )
        .unwrap();
        assert!(
            !runner
                .calls()
                .iter()
                .any(|c| c.join(" ").contains("tab close")),
            "a non-empty zen tab is the user's, not ours to close"
        );
    }

    #[test]
    fn leaving_a_vanished_target_is_a_no_op_not_an_error() {
        let runner = MockRunner::new().on("pane list", r#"{"result":{"panes":[]}}"#);
        let mut session = session_of(&[]);
        session.target = "w1:pGONE".into();
        assert!(leave(&runner, &session, &Notifier::silent(), &temp_store()).is_ok());
        assert!(!runner
            .calls()
            .iter()
            .any(|c| c.join(" ").contains("pane move")));
    }

    // --- toggle -----------------------------------------------------------

    #[test]
    fn an_unknown_verb_is_rejected() {
        let runner = MockRunner::new();
        let args = vec!["sideways".to_string()];
        assert!(cli(&runner, &args, &Config::default()).is_err());
    }
}
