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

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use crossterm::event::{KeyCode, KeyModifiers};
use serde_json::Value;

use crate::config::Config;
use crate::data::Theme;
use crate::notify::Notifier;
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crate::runner::{CommandRunner, SystemRunner};
use crate::socket;
use crate::state;

/// Where the toggle remembers an active session. Written last on the way in and
/// deleted first on the way out, so a crash mid-enter leaves no file claiming a
/// zen that is not there.
const STATE_FILE: &str = "zen.json";

/// The label given to the tab a zen'd pane lives in.
const ZEN_LABEL: &str = "zen";

/// A live zen session, reduced to what [`leave`] needs to undo it.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub target: String,
    pub zen_tab: String,
    pub origin_tab: String,
    pub gutters: Vec<String>,
    /// The pane the target sat next to before it left, and how. `None` when the
    /// target was alone in its tab and there is nothing to restore it against.
    pub anchor: Option<Anchor>,
}

/// How to put the target back where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub pane: String,
    /// `right` when the panes sat side by side, `down` when stacked.
    pub split: String,
    pub ratio: f32,
    /// True when the target was the *first* of the pair, which `pane move`
    /// cannot express — it always inserts after the anchor, so the restore adds
    /// a swap.
    pub target_first: bool,
    /// Whether putting the target back beside `pane` reproduces the original
    /// tree exactly.
    ///
    /// It does when the target's sibling in the split tree was a single pane.
    /// When the sibling was a whole subtree, there is no way to say so: `pane
    /// move --target-pane` can only insert *next to one pane*, which nests the
    /// target one level too deep. herdr 0.8.0 has no verb for "insert at this
    /// point in the tree", and `layout.apply` — the one call that could express
    /// it — destroys and recreates every pane, so it is not an option. Such a
    /// restore puts every pane back, in a slightly different arrangement, and
    /// says so rather than pretending otherwise.
    pub exact: bool,
}

// ---------------------------------------------------------------------------
// Pure geometry
// ---------------------------------------------------------------------------

/// The two `--ratio` values that centre a target occupying `width_pct` of the
/// tab, as `(right_gutter_split, left_gutter_split)`.
///
/// Both are ratios *for the target*, because that is what `pane split` takes.
/// The width is clamped rather than rejected: a nonsensical config should make
/// zen look odd, not refuse to open.
pub fn gutter_ratios(width_pct: u16) -> (f32, f32) {
    let width = (width_pct.clamp(20, 95) as f32) / 100.0;
    let gutter = (1.0 - width) / 2.0;
    (1.0 - gutter, gutter / (1.0 - gutter))
}

/// Find the target's sibling in an exported split tree, and how to rejoin it.
///
/// This reads the real tree rather than inferring one from rectangles, which
/// matters as soon as a tab has more than two panes: two panes can *look*
/// adjacent while sitting in different branches, and rejoining the wrong one
/// rebuilds the tab inside out.
pub fn sibling_anchor(node: &Value, target: &str) -> Option<Anchor> {
    let is_target = |child: &Value| child.get("pane_id").and_then(Value::as_str) == Some(target);

    if node.get("type").and_then(Value::as_str) == Some("split") {
        let (first, second) = (node.get("first")?, node.get("second")?);
        let ratio = node.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
        let split = node
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or("right")
            .to_string();

        for (target_first, sibling) in [(true, second), (false, first)] {
            let child = if target_first { first } else { second };
            if !is_target(child) {
                continue;
            }
            // The exported ratio always describes `first`, so a target sitting
            // second owns the remainder.
            let ratio = if target_first { ratio } else { 1.0 - ratio };
            let exact = sibling.get("type").and_then(Value::as_str) == Some("pane");
            return Some(Anchor {
                pane: first_leaf(sibling)?,
                split,
                ratio,
                target_first,
                exact,
            });
        }
        return sibling_anchor(first, target).or_else(|| sibling_anchor(second, target));
    }
    None
}

/// The first pane id under a node, which for a subtree sibling is the closest
/// thing to "the pane the target used to border".
fn first_leaf(node: &Value) -> Option<String> {
    if let Some(pane) = node.get("pane_id").and_then(Value::as_str) {
        return Some(pane.to_string());
    }
    first_leaf(node.get("first")?).or_else(|| first_leaf(node.get("second")?))
}

/// Work out how the target sat relative to `other` from geometry alone — the
/// fallback for when `layout.export` is unavailable (an older herdr, a closed
/// socket). Less reliable than [`sibling_anchor`], so it never claims exactness.
pub fn anchor_between(target: Rect, other: Rect, other_id: &str) -> Anchor {
    let horizontal = target.x != other.x;
    let (split, span_a, span_b) = if horizontal {
        ("right", target.width, other.width)
    } else {
        ("down", target.height, other.height)
    };
    let total = (span_a + span_b).max(1);
    Anchor {
        pane: other_id.to_string(),
        split: split.to_string(),
        ratio: span_a as f32 / total as f32,
        target_first: if horizontal {
            target.x < other.x
        } else {
            target.y < other.y
        },
        exact: false,
    }
}

/// A pane's cell rectangle, as `herdr pane layout` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    fn from_json(value: &Value) -> Self {
        let get = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
        Self {
            x: get("x"),
            y: get("y"),
            width: get("width"),
            height: get("height"),
        }
    }
}

// ---------------------------------------------------------------------------
// State file
// ---------------------------------------------------------------------------

/// The session is stored as flat lines rather than JSON: it is written and read
/// only here, and hand-rolling it keeps the format as inspectable as the rest of
/// this plugin's state files (`recent.tsv`, `update.tsv`).
fn encode(session: &Session) -> String {
    let mut out = format!(
        "target\t{}\nzen_tab\t{}\norigin_tab\t{}\n",
        session.target, session.zen_tab, session.origin_tab
    );
    for gutter in &session.gutters {
        out.push_str(&format!("gutter\t{gutter}\n"));
    }
    if let Some(anchor) = &session.anchor {
        out.push_str(&format!(
            "anchor\t{}\t{}\t{}\t{}\t{}\n",
            anchor.pane, anchor.split, anchor.ratio, anchor.target_first, anchor.exact
        ));
    }
    out
}

fn decode(text: &str) -> Option<Session> {
    let mut session = Session {
        target: String::new(),
        zen_tab: String::new(),
        origin_tab: String::new(),
        gutters: Vec::new(),
        anchor: None,
    };
    for line in text.lines() {
        let mut parts = line.split('\t');
        match (parts.next(), parts.next()) {
            (Some("target"), Some(v)) => session.target = v.to_string(),
            (Some("zen_tab"), Some(v)) => session.zen_tab = v.to_string(),
            (Some("origin_tab"), Some(v)) => session.origin_tab = v.to_string(),
            (Some("gutter"), Some(v)) => session.gutters.push(v.to_string()),
            (Some("anchor"), Some(pane)) => {
                session.anchor = Some(Anchor {
                    pane: pane.to_string(),
                    split: parts.next().unwrap_or("right").to_string(),
                    ratio: parts.next().and_then(|r| r.parse().ok()).unwrap_or(0.5),
                    target_first: parts.next() == Some("true"),
                    exact: parts.next() == Some("true"),
                })
            }
            _ => {}
        }
    }
    (!session.target.is_empty() && !session.zen_tab.is_empty()).then_some(session)
}

/// Where a zen session is remembered.
///
/// Passed in explicitly rather than reached for through [`state::state_file`],
/// because `enter` and `leave` genuinely write and delete this file: a unit test
/// exercising them against the default path deletes the *user's live session*
/// out from under them mid-run. (It did, once — a `cargo test` between two
/// toggles left a real zen'd pane stranded in an orphan tab.) Tests point it at
/// a temp dir; production uses the state dir.
pub struct SessionStore(Option<PathBuf>);

impl SessionStore {
    pub fn new() -> Self {
        Self(state::state_file(STATE_FILE))
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self(Some(path))
    }

    pub fn load(&self) -> Option<Session> {
        decode(&fs::read_to_string(self.0.as_ref()?).ok()?)
    }

    fn save(&self, session: &Session) -> Result<()> {
        let path = self.0.as_ref().context("no state dir for zen")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(path, encode(session)).with_context(|| format!("write {}", path.display()))
    }

    fn clear(&self) {
        if let Some(path) = &self.0 {
            fs::remove_file(path).ok();
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// herdr queries
// ---------------------------------------------------------------------------

/// Run a herdr verb for effect, swallowing its JSON reply.
///
/// [`CommandRunner::ok`] goes through `status()`, which **inherits stdout** — and
/// herdr answers every mutating verb with a line of JSON. For the pane-opening
/// actions that lands nowhere, but `zen-toggle` opens no pane, so its output goes
/// straight to the plugin log (or the user's terminal when run by hand). Capture
/// it instead and report only whether it worked.
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
    fn from_json(value: &Value) -> Option<Self> {
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
}

impl ZenConfig {
    pub fn from(cfg: &Config) -> Self {
        Self {
            width: cfg.get("zen_width", "70").parse().unwrap_or(70),
            scrim: cfg.bool("zen_scrim", true),
            color: socket::parse_hex(&cfg.get("zen_scrim_color", "#11111b"))
                .unwrap_or([0x11, 0x11, 0x1b, 0xff]),
        }
    }
}

/// Move `target` into a tab of its own, flank it with gutters, and dim them.
pub fn enter<R: CommandRunner>(
    runner: &R,
    target: &str,
    cfg: &ZenConfig,
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

    store.save(&session)?;
    Ok(session)
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
    // Delete the record first: from here on the session is over regardless of
    // which of the following steps herdr refuses.
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
            enter(runner, target, cfg, store).map(|_| ())
        }
        None => enter(runner, target, cfg, store).map(|_| ()),
    }
}

/// `herdr-switchboard zen toggle|on|off [--pane ID]`, the no-UI entry point
/// behind the `zen-toggle` plugin action.
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
                enter(runner, &pane, &zen, &store).map(|_| ())
            } else {
                toggle(runner, &pane, &zen, &notifier, &store)
            }
        }
        other => bail!("unknown zen verb '{other}' (expected toggle, on, or off)"),
    }
}

// ---------------------------------------------------------------------------
// The `--zen` picker mode
// ---------------------------------------------------------------------------

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let normal = cfg.common.keymode == "normal";
    picker::run(ZenMode::new(cfg), theme, normal)
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
                key_label: "↵",
                label: "zen",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "exit",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x",
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
                enter(&SystemRunner, item_id, &self.cfg, &self.store)?;
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
        }
    }

    #[test]
    fn entering_moves_the_target_out_then_builds_two_gutters_and_swaps() {
        let runner = entering();
        let session = enter(&runner, "w1:p1", &cfg(false), &temp_store()).unwrap();

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
    fn entering_records_the_way_home_before_the_pane_moves() {
        let runner = entering();
        let session = enter(&runner, "w1:p1", &cfg(false), &temp_store()).unwrap();
        let anchor = session.anchor.expect("neighbour should have been recorded");
        assert_eq!(anchor.pane, "w1:p2");
        assert_eq!(anchor.split, "right");
        assert!(anchor.target_first);
    }

    #[test]
    fn entering_parks_each_gutter_so_no_prompt_shows_through() {
        let runner = entering();
        enter(&runner, "w1:p1", &cfg(false), &temp_store()).unwrap();
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
        assert!(enter(&runner, "w1:pZZ", &cfg(false), &temp_store()).is_err());
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
        let session = enter(&runner, "w1:p1", &cfg(false), &temp_store()).unwrap();
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
