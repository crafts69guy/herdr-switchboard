//! Zen lifecycle and its typed boundary around herdr effects.

use anyhow::{Context, Result};
use serde_json::Value;

use super::geometry::{anchor_between, gutter_ratios, sibling_anchor, Rect};
use super::session::{Session, SessionStore};
use crate::chrome;
use crate::config::Config;
use crate::notify::Notifier;
use crate::runner::CommandRunner;
use crate::socket;

const ZEN_LABEL: &str = "zen";
fn herdr_do<R: CommandRunner>(runner: &R, args: &[&str]) -> bool {
    runner.capture("herdr", args).is_some()
}

fn herdr_json<R: CommandRunner>(runner: &R, args: &[&str]) -> Option<Value> {
    serde_json::from_str(&runner.capture("herdr", args)?).ok()
}

/// Every pane herdr knows about, as `(pane_id, tab_id)` plus its title and cwd.
pub fn list_panes<R: CommandRunner>(runner: &R) -> Vec<PaneInfo> {
    let Some(value) = herdr_json(runner, &["pane", "list"]) else {
        return Vec::new();
    };
    value["result"]["panes"]
        .as_array()
        .map(|panes| panes.iter().filter_map(PaneInfo::from_json).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneInfo {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub title: String,
    pub cwd: String,
    pub agent: Option<String>,
    pub focused: bool,
}

impl PaneInfo {
    pub(super) fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
        let pane_id = text("pane_id");
        if pane_id.is_empty() {
            return None;
        }
        Some(Self {
            pane_id: pane_id.to_string(),
            tab_id: text("tab_id").to_string(),
            workspace_id: text("workspace_id").to_string(),
            title: {
                let title = text("terminal_title_stripped");
                let title = if title.is_empty() {
                    text("terminal_title")
                } else {
                    title
                };
                title.to_string()
            },
            cwd: {
                let cwd = text("foreground_cwd");
                if cwd.is_empty() { text("cwd") } else { cwd }.to_string()
            },
            agent: Some(text("agent"))
                .filter(|a| !a.is_empty())
                .map(str::to_string),
            focused: value
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// The tab's total area and each pane's rect, from `herdr pane layout`.
fn layout<R: CommandRunner>(runner: &R, pane: &str) -> Option<(Rect, Vec<(String, Rect)>)> {
    let value = herdr_json(runner, &["pane", "layout", "--pane", pane])?;
    let layout = &value["result"]["layout"];
    let area = Rect::from_json(&layout["area"]);
    let panes = layout["panes"]
        .as_array()?
        .iter()
        .filter_map(|pane| {
            Some((
                pane.get("pane_id")?.as_str()?.to_string(),
                Rect::from_json(&pane["rect"]),
            ))
        })
        .collect();
    Some((area, panes))
}

// ---------------------------------------------------------------------------
// Enter / leave
// ---------------------------------------------------------------------------

/// Settings zen reads, resolved once so the CLI and the picker agree.
pub struct ZenConfig {
    pub width: u16,
    pub scrim: bool,
    pub color: [u8; 4],
    pub chrome: chrome::Level,
}

impl ZenConfig {
    pub fn from(cfg: &Config) -> Self {
        Self {
            width: cfg.zen.width,
            scrim: cfg.zen.scrim,
            color: socket::parse_hex(&cfg.zen.scrim_color).unwrap_or([0x11, 0x11, 0x1b, 0xff]),
            chrome: chrome::Level::parse(&cfg.zen.chrome),
        }
    }
}

/// Move `target` into a tab of its own, flank it with gutters, and dim them.
pub fn enter<R: CommandRunner>(
    runner: &R,
    target: &str,
    cfg: &ZenConfig,
    notifier: &Notifier,
    store: &SessionStore,
) -> Result<Session> {
    let panes = list_panes(runner);
    let info = panes
        .iter()
        .find(|pane| pane.pane_id == target)
        .with_context(|| format!("pane {target} is not open"))?;
    let origin_tab = info.tab_id.clone();

    // Record the way home before anything moves, while the original tab is
    // intact. The exported tree is authoritative; geometry is the fallback for a
    // herdr that will not hand one over.
    let anchor = socket::export_layout(&origin_tab)
        .and_then(|root| sibling_anchor(&root, target))
        .or_else(|| {
            let (_, rects) = layout(runner, target)?;
            let target_rect = rects.iter().find(|(id, _)| id == target)?.1;
            let (other_id, other_rect) = rects.iter().find(|(id, _)| id != target)?;
            Some(anchor_between(target_rect, *other_rect, other_id))
        });

    let moved = herdr_json(
        runner,
        &[
            "pane",
            "move",
            target,
            "--new-tab",
            "--label",
            ZEN_LABEL,
            "--focus",
        ],
    )
    .context("herdr could not move the pane into a zen tab")?;
    let zen_tab = moved["result"]["move_result"]["created_tab"]["tab_id"]
        .as_str()
        .context("herdr did not report a zen tab")?
        .to_string();

    let mut session = Session {
        target: target.to_string(),
        zen_tab,
        origin_tab,
        gutters: Vec::new(),
        anchor,
    };

    // Gutters are decoration: if a split fails, the pane is already alone on the
    // screen, which is most of the point. Keep whatever was built and record it
    // so the exit still cleans up.
    let (right_ratio, left_ratio) = gutter_ratios(cfg.width);
    for ratio in [right_ratio, left_ratio] {
        match split_gutter(runner, target, ratio) {
            Some(gutter) => session.gutters.push(gutter),
            None => break,
        }
    }
    if session.gutters.len() == 2 {
        // Step 3: the target is sitting in the left gutter slot until this swap.
        herdr_do(
            runner,
            &["pane", "swap", "--pane", target, "--direction", "right"],
        );
    }

    if cfg.scrim {
        paint_gutters(runner, &session, cfg.color);
    }

    // Chrome comes last of the herdr work: `server reload-config` redraws
    // everything, and doing it while the splits are still settling is the one
    // way to have herdr lay the gutters out against a stale frame.
    //
    // A snapshot left by an earlier session (herdr crashed mid-zen, say) means
    // the chrome is *already* suppressed. Re-snapshotting there would record
    // zen's own values as the user's and lose the way home for good, so the
    // leftover is kept and nothing is touched.
    let overrides = if store.load_chrome().is_empty() {
        suppress_chrome(runner, target, cfg.chrome, notifier)
    } else {
        Vec::new()
    };
    if !overrides.is_empty() && store.save_chrome(&overrides).is_err() {
        // No snapshot on disk means no way home, so put the config back now
        // rather than leaving the user's chrome rewritten indefinitely.
        chrome::disengage(runner, &overrides);
    }

    store.save(&session)?;
    Ok(session)
}

/// Hand herdr's chrome to [`chrome::engage`] and check the part of it herdr may
/// refuse.
///
/// `sidebar_start_collapsed` is documented as taking effect on the *next launch*,
/// so `full` may leave the rail exactly where it was. Rather than assume either
/// way, measure: the tab's area widens when the sidebar goes. If it did not, say
/// so — herdr's own `prefix+b` is the answer, and a silent no-op would read as a
/// broken setting.
fn suppress_chrome<R: CommandRunner>(
    runner: &R,
    target: &str,
    level: chrome::Level,
    notifier: &Notifier,
) -> Vec<chrome::Override> {
    let before = layout(runner, target).map(|(area, _)| area.width);
    let overrides = chrome::engage(runner, level);
    if overrides.is_empty() || level != chrome::Level::Full {
        return overrides;
    }
    let after = layout(runner, target).map(|(area, _)| area.width);
    if before.is_some() && before == after {
        notifier.send_message(
            "Zen hid what herdr will hide live; the sidebar needs prefix+b (herdr applies \
             sidebar_start_collapsed on its next launch).",
            "auto",
        );
    }
    overrides
}

/// Split one gutter off the target and park it so no shell prompt shows through
/// when the scrim is off.
fn split_gutter<R: CommandRunner>(runner: &R, target: &str, ratio: f32) -> Option<String> {
    let value = herdr_json(
        runner,
        &[
            "pane",
            "split",
            target,
            "--direction",
            "right",
            "--ratio",
            &format!("{ratio:.4}"),
            "--no-focus",
        ],
    )?;
    let gutter = value["result"]["pane"]["pane_id"].as_str()?.to_string();
    // `clear` hides the prompt; the sleep parks the shell so nothing ever
    // redraws under the scrim. Failure is cosmetic only.
    herdr_do(
        runner,
        &["pane", "run", &gutter, "clear && exec sleep 2147483647"],
    );
    Some(gutter)
}

/// Paint each gutter, sized from the layout herdr reports *after* the splits
/// settled — computing the rects ourselves would drift from what herdr drew.
fn paint_gutters<R: CommandRunner>(runner: &R, session: &Session, color: [u8; 4]) {
    let Some((_, rects)) = layout(runner, &session.target) else {
        return;
    };
    for gutter in &session.gutters {
        if let Some((_, rect)) = rects.iter().find(|(id, _)| id == gutter) {
            socket::set_scrim(
                gutter,
                rect.width.max(0) as u32,
                rect.height.max(0) as u32,
                color,
            );
        }
    }
}

/// Undo a zen session: drop the gutters, put the target back, close the tab.
///
/// Every step tolerates having already happened. The user can close a gutter,
/// close the zen tab, or move the pane by hand between enter and leave, and the
/// worst outcome must be a no-op — never a wedged toggle that needs the state
/// file deleted by hand.
pub fn leave<R: CommandRunner>(
    runner: &R,
    session: &Session,
    notifier: &Notifier,
    store: &SessionStore,
) -> Result<()> {
    // Give the user's chrome back first, and keep the snapshot when herdr's
    // config could not be rewritten — it is the only copy of what those `[ui]`
    // keys used to be, and `zen chrome-restore` is what comes looking for it.
    let overrides = store.load_chrome();
    if chrome::disengage(runner, &overrides) {
        store.clear_chrome();
    } else {
        notifier.send_message(
            "Zen could not put herdr's config back — run `herdr-switchboard zen \
             chrome-restore` (the original is saved in the plugin's state dir).",
            "auto",
        );
    }

    // Delete the record: from here on the session is over regardless of which
    // of the following steps herdr refuses.
    store.clear();

    let live = list_panes(runner);
    let alive = |pane: &str| live.iter().any(|p| p.pane_id == pane);

    for gutter in &session.gutters {
        if alive(gutter) {
            socket::clear_scrim(gutter);
            herdr_do(runner, &["pane", "close", gutter]);
        }
    }

    if !alive(&session.target) {
        return Ok(());
    }

    if let Some(anchor) = &session.anchor {
        if alive(&anchor.pane) {
            let ratio = format!("{:.4}", anchor.ratio);
            let moved = herdr_do(
                runner,
                &[
                    "pane",
                    "move",
                    &session.target,
                    "--tab",
                    &session.origin_tab,
                    "--split",
                    &anchor.split,
                    "--target-pane",
                    &anchor.pane,
                    "--ratio",
                    &ratio,
                    "--focus",
                ],
            );
            // `pane move` can only insert *after* the anchor, so a target that
            // was originally first needs one swap to land back on its own side.
            if moved && anchor.target_first {
                let back = if anchor.split == "right" {
                    "left"
                } else {
                    "up"
                };
                herdr_do(
                    runner,
                    &[
                        "pane",
                        "swap",
                        "--pane",
                        &session.target,
                        "--direction",
                        back,
                    ],
                );
            }
            // Say so when the tab could not be rebuilt exactly, rather than
            // leaving the user to notice a quietly rearranged layout and wonder
            // what else zen touched. Nothing was lost — only re-nested.
            if moved && !anchor.exact {
                notifier.send_message(
                    "Zen restored every pane, but this tab's split nesting could not be \
                     reproduced exactly.",
                    "auto",
                );
            }
        }
    }

    // Only tidy the tab away if zen emptied it; a pane the user added there is
    // theirs, and closing it would be the destructive surprise.
    if runner
        .capture("herdr", &["tab", "get", &session.zen_tab])
        .and_then(|json| {
            serde_json::from_str::<Value>(&json).ok()?["result"]["tab"]["pane_count"].as_i64()
        })
        .is_some_and(|count| count == 0)
    {
        herdr_do(runner, &["tab", "close", &session.zen_tab]);
    }
    Ok(())
}

/// Enter if there is no live session, leave if there is. The session counts as
/// live only while its target pane still exists, so a stale file left by a
/// crashed herdr resolves to "enter" rather than trapping the user.
pub fn toggle<R: CommandRunner>(
    runner: &R,
    target: &str,
    cfg: &ZenConfig,
    notifier: &Notifier,
    store: &SessionStore,
) -> Result<()> {
    match store.load() {
        Some(session)
            if list_panes(runner)
                .iter()
                .any(|p| p.pane_id == session.target) =>
        {
            leave(runner, &session, notifier, store)
        }
        Some(_) => {
            store.clear();
            enter(runner, target, cfg, notifier, store).map(|_| ())
        }
        None => enter(runner, target, cfg, notifier, store).map(|_| ()),
    }
}
