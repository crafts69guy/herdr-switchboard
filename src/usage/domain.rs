//! Usage report values and closed loading states.

/// How urgent a window is, as the provider itself grades it.
///
/// Claude's usage endpoint ships a `severity` per limit, which beats any
/// threshold this plugin could invent — the provider knows what its own plan
/// considers close to the edge. Codex ships none, so its windows fall back to
/// the configured `usage.warn_percent` / `usage.alert_percent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Severity {
    Normal,
    Warning,
    Critical,
}

impl Severity {
    pub(in crate::usage) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "normal" | "ok" => Some(Severity::Normal),
            "warning" | "warn" => Some(Severity::Warning),
            "critical" | "exceeded" | "reached" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// One rate-limit window of one provider: a five-hour bucket, a weekly bucket.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Window {
    pub label: String,
    /// 0..=100. Both sources are normalised to this scale on the way in.
    pub used_percent: f64,
    /// Unix epoch seconds, when the source says when the window rolls over.
    pub resets_at: Option<u64>,
    /// The provider's own grading, when it offers one.
    pub severity: Option<Severity>,
}

/// One `key  value` line under a card's windows: what the session spent, how
/// much context the last turn used, whether credits are on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Fact {
    pub key: String,
    pub value: String,
}

impl Fact {
    pub(in crate::usage) fn new(key: &str, value: impl Into<String>) -> Self {
        Fact {
            key: key.to_string(),
            value: value.into(),
        }
    }
}

/// What one provider's card shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Report {
    pub name: String,
    pub plan: Option<String>,
    /// When the subscription itself renews, in epoch seconds.
    ///
    /// This is *not* a rate-limit rollover — see [`Window::resets_at`] for that.
    /// A window reset says when you may work again; a renewal says when the
    /// plan is charged and its allowance starts over. `None` when the provider
    /// publishes no such date anywhere readable.
    pub renews_at: Option<u64>,
    /// Ordered as the provider reports them; the card promotes the busiest.
    pub windows: Vec<Window>,
    pub facts: Vec<Fact>,
    /// When the numbers were true, in epoch seconds.
    ///
    /// This is not decoration. Codex's figures come out of the session log it
    /// last wrote, so a machine that has not run Codex since Tuesday shows
    /// Tuesday's percentage — and a stale number that looks live is worse than
    /// no number, because it is the one you would plan around.
    pub measured_at: Option<u64>,
}

/// A provider's cell in the popup. `Loading` exists because Claude's number
/// arrives over the network, so the first frame must be drawable without it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Slot {
    Loading { name: String },
    Ready(Report),
    Unavailable { name: String, reason: String },
}

impl Slot {
    pub fn name(&self) -> &str {
        match self {
            Slot::Loading { name } => name,
            Slot::Ready(report) => &report.name,
            Slot::Unavailable { name, .. } => name,
        }
    }

    pub fn windows(&self) -> &[Window] {
        match self {
            Slot::Ready(report) => &report.windows,
            _ => &[],
        }
    }

    pub fn facts(&self) -> &[Fact] {
        match self {
            Slot::Ready(report) => &report.facts,
            _ => &[],
        }
    }

    /// The window the card draws big: the one closest to running out.
    pub fn hottest(&self) -> Option<&Window> {
        let Slot::Ready(report) = self else {
            return None;
        };
        report
            .windows
            .iter()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }
}
