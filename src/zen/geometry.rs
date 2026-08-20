//! Pure layout calculations for centring and restoring a pane.

use serde_json::Value;

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
    pub(super) fn from_json(value: &Value) -> Self {
        let get = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
        Self {
            x: get("x"),
            y: get("y"),
            width: get("width"),
            height: get("height"),
        }
    }
}
