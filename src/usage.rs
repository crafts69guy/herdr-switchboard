//! Subscription quota for the AI agents you actually pay for.
//!
//! Herdr knows which agents are *running* and which are *installed*; neither
//! answers "how much of my plan is left". That number is not in herdr's JSON at
//! all, and the two providers hold it in two completely different places:
//!
//! - **Codex** writes it to disk. Every turn, the rollout JSONL gets a
//!   `token_count` event carrying the `rate_limits` OpenAI returned, so the
//!   number is exact and reading it costs no network at all — the file just has
//!   to be read from the *end*, because a rollout reaches tens of megabytes.
//! - **Claude Code** does not persist it anywhere. `stats-cache.json` is local
//!   token accounting, and a transcript only learns about a limit *after* a 429.
//!   The real number lives behind `/api/oauth/usage`, which is what `/usage`
//!   itself calls, so this module calls it too.
//!
//! That makes this the one surface in the plugin that touches the network, and
//! the one that touches a credential. Both are deliberate and both are fenced:
//! the request runs on a worker thread with a timeout so the popup never blocks
//! on it, and the token is handed to `curl` through a pipe because argv is
//! visible to every process the user owns (see [`Claude::fetch`]).
//!
//! A card also says when the plan *renews*, which is a different question from
//! when a window resets and is answered by a different source: Codex puts the
//! date in the ID token the account line already decodes, and Anthropic states
//! it nowhere readable, so that card says `unknown` rather than computing a
//! date from the day the subscription was created (see [`format_renewal`]).
//!
//! Everything degrades to a [`Slot::Unavailable`] card with a readable reason.
//! A dead endpoint, a denied keychain, a machine that has never run Codex — none
//! of them may take down the other provider's card, and none of them may panic.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use anyhow::{anyhow, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::config::Config;
use crate::data::Theme;
use crate::runner::{CommandRunner, SystemRunner};
use crate::state::now;
use crate::tui::{self, Flow, Pill, SimpleMode};

/// How urgent a window is, as the provider itself grades it.
///
/// Claude's usage endpoint ships a `severity` per limit, which beats any
/// threshold this plugin could invent — the provider knows what its own plan
/// considers close to the edge. Codex ships none, so its windows fall back to
/// the configured `usage.warn_percent` / `usage.alert_percent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Normal,
    Warning,
    Critical,
}

impl Severity {
    fn parse(raw: &str) -> Option<Self> {
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
pub struct Window {
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
pub struct Fact {
    pub key: String,
    pub value: String,
}

impl Fact {
    fn new(key: &str, value: impl Into<String>) -> Self {
        Fact {
            key: key.to_string(),
            value: value.into(),
        }
    }
}

/// What one provider's card shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
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
pub enum Slot {
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

// --- providers -------------------------------------------------------------

/// One source of quota numbers. Adding a provider is an impl plus a line in
/// [`providers`] — the same shape as the entry sources in [`crate::source`].
///
/// `offline` splits the registry in two: an offline provider is read on the
/// main thread before the terminal is claimed (it costs a file read), while the
/// rest are fetched on a worker so the first frame never waits on a socket.
trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn offline(&self) -> bool;
    fn load(&self, runner: &dyn CommandRunner, cfg: &Config) -> Result<Report>;
}

fn providers() -> Vec<Box<dyn Provider>> {
    vec![Box::new(Codex), Box::new(Claude)]
}

// --- codex -----------------------------------------------------------------

struct Codex;

impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn name(&self) -> &'static str {
        "Codex"
    }
    fn offline(&self) -> bool {
        true
    }
    fn load(&self, _runner: &dyn CommandRunner, _cfg: &Config) -> Result<Report> {
        let sessions = home()?.join(".codex/sessions");
        let rollouts = recent_rollouts(&sessions, ROLLOUT_TRIES);
        if rollouts.is_empty() {
            return Err(anyhow!("no Codex sessions on this machine"));
        }
        for path in &rollouts {
            let Some(line) = last_rate_limit_line(path) else {
                continue;
            };
            let mut report = parse_codex_rate_limits(&line)?;
            // Which account these numbers belong to, first — the quota is
            // meaningless without knowing whose it is, and the two providers
            // here are routinely signed in as two different people. The same
            // token dates the subscription, which the rollout never mentions.
            if let Some(identity) = codex_identity() {
                if let Some(email) = identity.email {
                    report.facts.insert(0, Fact::new("account", email));
                }
                report.renews_at = identity.renews_at;
            }
            return Ok(report);
        }
        Err(anyhow!("no rate limit data in recent sessions"))
    }
}

/// How many rollouts back to look before giving up. A session that never made a
/// request carries no `rate_limits`, and resuming an old thread writes a fresh
/// file, so the newest rollout is not always the informative one.
const ROLLOUT_TRIES: usize = 3;

/// Read this much of the file's end first. A `rate_limits` event is appended on
/// every turn, so the last one is almost always inside the final few KB — but a
/// single turn can carry a very large tool result, hence the second, wider pass.
const TAIL_FIRST: u64 = 256 * 1024;
const TAIL_WIDE: u64 = 2 * 1024 * 1024;

/// The newest rollout files under `~/.codex/sessions`, newest first.
///
/// The tree is `YYYY/MM/DD/rollout-*.jsonl` and holds hundreds of files totalling
/// gigabytes, so this walks down the newest-named directory at each level rather
/// than listing the tree. Names sort lexicographically *and* chronologically
/// there, which is the whole reason the layout is date-nested.
fn recent_rollouts(sessions: &Path, want: usize) -> Vec<PathBuf> {
    let mut dir = sessions.to_path_buf();
    for _ in 0..3 {
        let Some(next) = newest_child(&dir, true) else {
            return Vec::new();
        };
        dir = next;
    }
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    files.into_iter().take(want).map(|(_, path)| path).collect()
}

/// The lexicographically greatest entry in `dir`, restricted to directories when
/// `dirs` is set.
fn newest_child(dir: &Path, dirs: bool) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir() == dirs)
        .map(|entry| entry.path())
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

/// The last line of `path` mentioning `rate_limits`, read from the end.
///
/// Rollouts reach tens of megabytes; reading one whole to find its final line
/// would cost more than the rest of this popup put together. The first line of a
/// tail read is dropped because a byte offset lands mid-line.
fn last_rate_limit_line(path: &Path) -> Option<String> {
    for window in [TAIL_FIRST, TAIL_WIDE] {
        let text = read_tail(path, window)?;
        // Drop everything before the first newline: a byte offset lands mid-line,
        // and half a JSON object parses as nothing at best.
        let whole = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        if let Some(line) = whole
            .lines()
            .rev()
            .find(|line| line.contains("\"rate_limits\""))
        {
            return Some(line.to_string());
        }
    }
    None
}

fn read_tail(path: &Path, bytes: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(bytes))).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// Turn one rollout line into a [`Report`].
fn parse_codex_rate_limits(line: &str) -> Result<Report> {
    let value: serde_json::Value =
        serde_json::from_str(line).context("Codex rollout line is not JSON")?;
    let payload = &value["payload"];
    let limits = &payload["rate_limits"];
    if limits.is_null() {
        return Err(anyhow!("rollout line carries no rate_limits"));
    }
    let windows = ["primary", "secondary"]
        .iter()
        .filter_map(|key| codex_window(&limits[*key]))
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return Err(anyhow!("rate_limits carries no window"));
    }
    Ok(Report {
        name: "Codex".into(),
        plan: limits["plan_type"].as_str().map(str::to_string),
        // The rollout says nothing about the subscription; `Codex::load` fills
        // this in from the ID token.
        renews_at: None,
        windows,
        facts: codex_facts(payload),
        // The event's own timestamp, not the file's mtime: resuming a thread
        // rewrites the file without producing a new reading.
        measured_at: value["timestamp"].as_str().and_then(parse_rfc3339_epoch),
    })
}

/// Who Codex is logged in as.
///
/// `codex login status` says only "Logged in using ChatGPT", so the address has
/// to come from the ID token in `~/.codex/auth.json`. Only the `email` claim is
/// read, and only for display: the signature is not verified because nothing
/// here trusts the token, it just labels a card. **Nothing in that file is ever
/// logged, drawn, or passed on — the access and refresh tokens beside it are
/// read past and dropped.**
/// Who Codex is signed in as, and when that subscription renews.
///
/// Both come out of one file and one token: `~/.codex/auth.json` holds an ID
/// token whose payload carries the address and — under OpenAI's namespaced
/// claim — the subscription's active window. Read together rather than twice,
/// the same way [`ClaudeProfile`] takes both of its fields from one read.
///
/// The access and refresh tokens sitting beside the ID token in that file are
/// read past and dropped: nothing from this file is logged, drawn, or sent
/// anywhere except the two labels below.
#[derive(Clone, Debug, Default, PartialEq)]
struct CodexIdentity {
    email: Option<String>,
    renews_at: Option<u64>,
}

fn codex_identity() -> Option<CodexIdentity> {
    let text = fs::read_to_string(home().ok()?.join(".codex/auth.json")).ok()?;
    identity_from_codex_auth(&text)
}

/// The claim OpenAI namespaces its subscription facts under. A JWT claim name
/// is an opaque string, and this one happens to look like a URL; nothing is
/// fetched from it.
const CODEX_AUTH_CLAIM: &str = "https://api.openai.com/auth";

fn identity_from_codex_auth(text: &str) -> Option<CodexIdentity> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let claims = jwt_claims(value["tokens"]["id_token"].as_str()?)?;
    let identity = CodexIdentity {
        email: claims["email"]
            .as_str()
            .filter(|email| !email.is_empty())
            .map(str::to_string),
        renews_at: claims[CODEX_AUTH_CLAIM]["chatgpt_subscription_active_until"]
            .as_str()
            .and_then(parse_rfc3339_epoch),
    };
    (identity != CodexIdentity::default()).then_some(identity)
}

/// The claim set of a JWT, without verifying it.
///
/// A JWT is `header.payload.signature`, each base64url without padding. This
/// reads the payload only, and the caller must treat the result as a label
/// rather than as proof of anything.
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&base64url_decode(payload)?).ok()
}

/// base64url without padding — the one encoding a JWT uses. Hand-rolled because
/// this is the only base64 in the plugin and a crate for sixty-four characters
/// would be a dependency nobody could justify.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => return None,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let six = value(byte)?;
        buffer = (buffer << 6) | six;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// The `key  value` lines under a Codex card.
///
/// Everything here comes from the same `token_count` event as the percentages,
/// so it describes the session that produced them — which is the context the
/// percentage needs: "42% of the week" means something different after a
/// nineteen-million-token session than after a short one.
fn codex_facts(payload: &serde_json::Value) -> Vec<Fact> {
    let mut facts = Vec::new();
    let info = &payload["info"];
    let total = &info["total_token_usage"];
    if let Some(tokens) = total["total_tokens"].as_u64() {
        // Cached input is the bulk of any long session, and it is the number
        // that explains why a total this large has not spent the plan.
        let cached = total["cached_input_tokens"].as_u64().unwrap_or(0);
        let input = total["input_tokens"].as_u64().unwrap_or(0);
        let value = match percent_of(cached, input) {
            Some(share) => format!("{} · {share:.0}% cached", compact_count(tokens)),
            None => compact_count(tokens),
        };
        facts.push(Fact::new("session", value));
    }
    if let (Some(last), Some(window)) = (
        info["last_token_usage"]["total_tokens"].as_u64(),
        info["model_context_window"].as_u64(),
    ) {
        let value = match percent_of(last, window) {
            Some(share) => format!(
                "{} / {}  ({share:.0}%)",
                compact_count(last),
                compact_count(window)
            ),
            None => format!("{} / {}", compact_count(last), compact_count(window)),
        };
        facts.push(Fact::new("context", value));
    }
    let credits = &payload["rate_limits"]["credits"];
    if credits["unlimited"].as_bool().unwrap_or(false) {
        facts.push(Fact::new("credits", "unlimited"));
    } else if credits["has_credits"].as_bool().unwrap_or(false) {
        facts.push(Fact::new(
            "credits",
            credits["balance"].as_str().unwrap_or("?").to_string(),
        ));
    } else if credits.is_object() {
        facts.push(Fact::new("credits", "none"));
    }
    // Only worth a line when it happened; `null` is the normal state.
    if let Some(kind) = payload["rate_limits"]["rate_limit_reached_type"].as_str() {
        facts.push(Fact::new("limit hit", kind.to_string()));
    }
    facts
}

fn codex_window(value: &serde_json::Value) -> Option<Window> {
    let used = value["used_percent"].as_f64()?;
    let minutes = value["window_minutes"].as_u64().unwrap_or(0);
    Some(Window {
        label: window_label(minutes),
        used_percent: clamp_percent(used),
        resets_at: value["resets_at"].as_u64(),
        // Codex grades nothing; its colour comes from the configured thresholds.
        severity: None,
    })
}

/// `19.1M`, `162k`, `840`. Thousands separators would not fit the column, and a
/// raw nine-digit number is unreadable at a glance anyway.
fn compact_count(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// `part` as a percentage of `whole`, or `None` when the ratio would be a lie
/// (an empty or smaller-than-part denominator).
fn percent_of(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| (part as f64 / whole as f64 * 100.0).clamp(0.0, 100.0))
}

/// A window length as the card says it: `weekly`, `5h`, `45m`.
fn window_label(minutes: u64) -> String {
    match minutes {
        0 => "window".into(),
        10080 => "weekly".into(),
        1440 => "daily".into(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

// --- claude ----------------------------------------------------------------

struct Claude;

/// The endpoint `/usage` itself calls. Not a documented API: treat every field
/// as optional and every shape as provisional (see [`parse_claude_usage`]).
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The keychain service Claude Code stores its OAuth credentials under on macOS.
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn name(&self) -> &'static str {
        "Claude Code"
    }
    fn offline(&self) -> bool {
        false
    }
    fn load(&self, runner: &dyn CommandRunner, cfg: &Config) -> Result<Report> {
        let token = claude_token(runner)?;
        let body = Claude::fetch(runner, &token, cfg.usage.timeout_ms)?;
        let mut report = parse_claude_usage(&body, now())?;
        if let Some(profile) = claude_profile() {
            if let Some(email) = profile.email {
                report.facts.insert(0, Fact::new("account", email));
            }
            // The usage endpoint names no plan, so this is the only place the
            // Claude card can learn it.
            report.plan = report.plan.or(profile.plan);
        }
        Ok(report)
    }
}

impl Claude {
    /// GET the usage endpoint, with the bearer token fed through stdin.
    ///
    /// `curl --config -` reads the same directives a `.curlrc` holds, which is
    /// the only way to give curl a header without putting it in argv. That
    /// matters: argv is readable through `ps` by every process this user owns,
    /// for the whole life of the call, so a token passed as `-H` would be a
    /// credential leak to anything running on the machine.
    fn fetch(runner: &dyn CommandRunner, token: &str, timeout_ms: u64) -> Result<String> {
        let seconds = (timeout_ms as f64 / 1000.0).max(0.5);
        let config = format!(
            "header = \"Authorization: Bearer {token}\"\nheader = \"anthropic-beta: oauth-2025-04-20\"\n"
        );
        let out = runner
            .output_stdin(
                "curl",
                &[
                    "--silent",
                    "--show-error",
                    "--fail",
                    "--max-time",
                    &format!("{seconds:.1}"),
                    "--config",
                    "-",
                    CLAUDE_USAGE_URL,
                ],
                &config,
            )
            .context("could not run curl")?;
        if !out.status.success() {
            return Err(anyhow!("usage request failed"));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The OAuth access token Claude Code holds.
///
/// macOS keeps it in the keychain, so the first read pops a system prompt; a
/// refusal is a plain error here, never a retry loop. Linux keeps it in a file.
/// The token is returned, used, and dropped — it is never traced, drawn, or
/// written anywhere.
fn claude_token(runner: &dyn CommandRunner) -> Result<String> {
    let raw = if cfg!(target_os = "macos") {
        runner
            .capture(
                "security",
                &["find-generic-password", "-s", CLAUDE_KEYCHAIN_SERVICE, "-w"],
            )
            .ok_or_else(|| anyhow!("keychain access denied or no credentials stored"))?
    } else {
        let path = home()?.join(".claude/.credentials.json");
        fs::read_to_string(&path).map_err(|_| anyhow!("no credentials at ~/.claude"))?
    };
    parse_claude_token(&raw)
}

fn parse_claude_token(raw: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(raw.trim()).context("credentials are not JSON")?;
    value["claudeAiOauth"]["accessToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("credentials carry no access token"))
}

/// Read the usage payload as leniently as the shape deserves.
///
/// This endpoint is undocumented, so nothing about it is assumed beyond "there
/// are named buckets with a utilization and a reset". Both `snake_case` and
/// `camelCase` reset keys are accepted, resets arrive as either epoch integers
/// or RFC 3339 strings, and an unrecognised payload is an error rather than a
/// card full of zeroes — a confident `0%` on a plan that is nearly spent is
/// worse than saying nothing.
fn parse_claude_usage(body: &str, measured_at: u64) -> Result<Report> {
    let value: serde_json::Value =
        serde_json::from_str(body.trim()).context("usage response is not JSON")?;
    let buckets = [
        ("five_hour", "5h"),
        ("seven_day", "7d"),
        ("seven_day_opus", "opus 7d"),
        ("seven_day_sonnet", "sonnet 7d"),
    ];
    // `limits` grades each window; the buckets carry the numbers. Reading the
    // grade from the provider beats grading it here — it knows what its own
    // plan considers close to the edge, and it says so per limit.
    let severities = claude_severities(&value["limits"]);
    let windows = buckets
        .iter()
        .filter_map(|(key, label)| claude_window(&value[*key], label))
        .map(|mut window| {
            window.severity = severities
                .get(&window.used_percent.round().to_string())
                .copied();
            window
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return Err(anyhow!("usage response carries no known window"));
    }
    Ok(Report {
        name: "Claude Code".into(),
        plan: value["plan"]
            .as_str()
            .or_else(|| value["plan_type"].as_str())
            .or_else(|| value["subscription"].as_str())
            .map(str::to_string),
        // Anthropic publishes no period end: not in this payload, not in
        // `~/.claude.json`. See `claude_profile`.
        renews_at: None,
        windows,
        facts: claude_facts(&value),
        measured_at: Some(measured_at),
    })
}

/// The severity of each limit, keyed by its rounded percentage.
///
/// `limits[]` and the named buckets are two views of the same numbers and share
/// no id, so the percentage is the join. It is a weak key — two windows sitting
/// at the same percentage would share a grade — but the consequence is a colour,
/// and both windows being at the same percentage means the colour is right for
/// both anyway.
fn claude_severities(limits: &serde_json::Value) -> std::collections::HashMap<String, Severity> {
    limits
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let percent = entry["percent"].as_f64()?;
                    let severity = Severity::parse(entry["severity"].as_str()?)?;
                    Some((percent.round().to_string(), severity))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Who Claude Code is logged in as, and which plan that account is on.
///
/// The usage endpoint names no plan, but Claude Code caches its own profile in
/// `~/.claude.json` — a plain settings file, not a credential store — and that
/// is where both the address and `claude_pro` / `claude_max` come from.
#[derive(Clone, Debug, PartialEq)]
struct ClaudeProfile {
    email: Option<String>,
    plan: Option<String>,
}

fn claude_profile() -> Option<ClaudeProfile> {
    let text = fs::read_to_string(home().ok()?.join(".claude.json")).ok()?;
    profile_from_claude_json(&text)
}

fn profile_from_claude_json(text: &str) -> Option<ClaudeProfile> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let account = &value["oauthAccount"];
    let profile = ClaudeProfile {
        email: account["emailAddress"]
            .as_str()
            .filter(|email| !email.is_empty())
            .map(str::to_string),
        plan: account["organizationType"]
            .as_str()
            .map(plan_label)
            .filter(|plan| !plan.is_empty()),
    };
    (profile.email.is_some() || profile.plan.is_some()).then_some(profile)
}

/// `claude_pro` as `pro`, so it sits beside Codex's `prolite` rather than
/// shouting a vendor prefix the card's own heading already established.
fn plan_label(raw: &str) -> String {
    raw.strip_prefix("claude_").unwrap_or(raw).replace('_', " ")
}

/// The `key  value` lines under a Claude card: what happens once the plan runs
/// out, which is the question the percentage raises.
fn claude_facts(value: &serde_json::Value) -> Vec<Fact> {
    let mut facts = Vec::new();
    let spend = &value["spend"];
    if spend["enabled"].as_bool().unwrap_or(false) {
        let used = money(&spend["used"]);
        let cap = spend["cap"]
            .as_f64()
            .map(|cap| format!(" / ${cap:.0}"))
            .unwrap_or_default();
        facts.push(Fact::new("credits", format!("{used}{cap}")));
    } else if spend.is_object() {
        facts.push(Fact::new("credits", "off"));
    }
    let extra = &value["extra_usage"];
    if extra["spend_limit_reached"].as_bool().unwrap_or(false) {
        facts.push(Fact::new("extra", "limit reached"));
    } else if extra["is_enabled"].as_bool().unwrap_or(false) {
        let used = extra["used_credits"]
            .as_f64()
            .map(|used| format!("{used:.2}"))
            .unwrap_or_else(|| "on".into());
        facts.push(Fact::new("extra", used));
    }
    facts
}

fn claude_window(value: &serde_json::Value, label: &str) -> Option<Window> {
    let used = value["utilization"]
        .as_f64()
        .or_else(|| value["used_percent"].as_f64())?;
    Some(Window {
        label: label.to_string(),
        used_percent: clamp_percent(used),
        resets_at: value["resets_at"]
            .as_u64()
            .or_else(|| value["resetsAt"].as_u64())
            .or_else(|| {
                value["resetsAt"]
                    .as_str()
                    .or_else(|| value["resets_at"].as_str())
                    .and_then(parse_rfc3339_epoch)
            }),
        severity: None,
    })
}

/// A minor-unit money object (`{"amount_minor": 250, "exponent": 2}`) as `$2.50`.
fn money(value: &serde_json::Value) -> String {
    let minor = value["amount_minor"].as_f64().unwrap_or(0.0);
    let exponent = value["exponent"].as_u64().unwrap_or(2) as i32;
    let amount = minor / 10f64.powi(exponent);
    let symbol = match value["currency"].as_str() {
        Some("USD") | None => "$",
        Some(other) => return format!("{amount:.2} {other}"),
    };
    format!("{symbol}{amount:.2}")
}

/// Both sources report on a 0..=100 scale — measured, not assumed: Codex writes
/// `used_percent: 41.0` and the usage endpoint answers `utilization: 51.0` for
/// an account half through its window.
///
/// So this only clamps. An earlier version guessed that a value at or below 1.0
/// was a fraction and multiplied it by 100, which reads a genuine `0.8%` — the
/// state of every plan at the start of its window — as `80%`. Guessing the scale
/// silently turns a quiet morning into a red card; clamping cannot.
fn clamp_percent(raw: f64) -> f64 {
    if raw.is_finite() {
        raw.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Enough of RFC 3339 to turn `2026-08-18T14:22:03Z` into epoch seconds.
///
/// Hand-rolled because this is the only date this plugin parses, and a date
/// crate would be a dependency for one field of one undocumented endpoint.
fn parse_rfc3339_epoch(text: &str) -> Option<u64> {
    let (date, rest) = text.split_once('T')?;
    let time = rest
        .split(['Z', '+', '.'])
        .next()
        .filter(|time| !time.is_empty())?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut time = time.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next().unwrap_or("0").parse().ok()?;
    // Howard Hinnant's days-from-civil, the standard branch-free conversion.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

fn home() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow!("HOME is not set"))
}

// --- the donut -------------------------------------------------------------

/// How finely the ring is sampled. Braille gives 2×4 dots per cell, so a card
/// this size holds roughly 40×40 dots; sampling well past that is what keeps the
/// arc solid instead of dotted at every radius.
const ARC_STEPS: usize = 720;
const RING_INNER: f64 = 0.58;
const RING_OUTER: f64 = 0.95;
/// Radial samples across the ring's thickness. This has to beat the dot pitch:
/// at this card's size the ring spans roughly fourteen braille dots, and five
/// samples across it drew five concentric circles with gaps between them rather
/// than one solid band.
const RING_BANDS: usize = 28;
/// The tallest a donut gets, in cells. Left unbounded it eats the whole card and
/// pushes the reset line off the bottom on a short pane.
const DONUT_MAX_H: u16 = 12;

/// The dots of one ring: the used arc, then the track behind it.
type Ring = (Vec<(f64, f64)>, Vec<(f64, f64)>);

/// The ring's dots, split into the used arc and the track behind it.
///
/// Pure on purpose: the geometry is the part worth testing, and it can be
/// checked without a terminal. The sweep starts at twelve o'clock and runs
/// clockwise, the way every gauge a reader has ever seen does.
fn donut_points(pct: f64, inner: f64, outer: f64, steps: usize) -> Ring {
    let fraction = (pct / 100.0).clamp(0.0, 1.0);
    let mut used = Vec::new();
    let mut track = Vec::new();
    for step in 0..steps {
        let t = step as f64 / steps as f64;
        let theta = std::f64::consts::FRAC_PI_2 - std::f64::consts::TAU * t;
        for band in 0..RING_BANDS {
            let radius = inner + (outer - inner) * band as f64 / (RING_BANDS - 1) as f64;
            let point = (radius * theta.cos(), radius * theta.sin());
            if t < fraction {
                used.push(point);
            } else {
                track.push(point);
            }
        }
    }
    (used, track)
}

/// `1d 14h`, `42m`, `now`. Coarse on purpose: nobody plans around the seconds,
/// and a ticking figure in a snapshot popup would be a lie the moment it stops.
fn format_reset(now: u64, resets_at: u64) -> String {
    let remaining = resets_at.saturating_sub(now);
    if remaining == 0 {
        return "now".into();
    }
    let (days, hours, minutes) = (
        remaining / 86_400,
        (remaining % 86_400) / 3_600,
        (remaining % 3_600) / 60,
    );
    match (days, hours, minutes) {
        (0, 0, m) => format!("{}m", m.max(1)),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// `16h 4m` today, `1d 12h · 19 Aug` when the rollover lands on another day.
///
/// The countdown stays the primary answer — it is the only "how long do I wait"
/// figure a window that missed the donut gets — and the date rides behind it for
/// the windows far enough out that `1d 12h` hides which working day that is. A
/// reset already past reads `now`, and a date beside that word would contradict
/// it, so that case stays bare.
fn format_reset_dated(now: u64, resets_at: u64, offset: i64) -> String {
    let relative = format_reset(now, resets_at);
    if resets_at <= now || local_day(resets_at, offset) == local_day(now, offset) {
        return relative;
    }
    format!("{relative} · {}", format_date(resets_at, offset))
}

/// The local UTC offset in seconds, read once per refresh.
///
/// There is no time zone crate here and `std` has no local-time API, so the
/// offset comes from `date +%z` — one command per refresh rather than a
/// dependency, and the same seam every other shell-out in this plugin uses. An
/// unreadable offset degrades to UTC, which is wrong by hours but never wrong
/// about *which* number resets.
fn local_offset(runner: &dyn CommandRunner) -> i64 {
    let Some(raw) = runner.capture("date", &["+%z"]) else {
        return 0;
    };
    parse_utc_offset(&raw).unwrap_or(0)
}

/// `+0700` / `-0430` as seconds east of UTC.
fn parse_utc_offset(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    let sign = match raw.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits = &raw[1..];
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[0..2].parse().ok()?;
    let minutes: i64 = digits[2..4].parse().ok()?;
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// `09:16 Fri` — the wall clock a window rolls over at, in the viewer's zone.
///
/// The relative form answers "how long do I wait", this answers "can I finish
/// before then"; a plan reset at 09:16 tomorrow is a different working day from
/// one at 23:50 tonight, and "in 1d 12h" hides that.
fn format_clock(epoch: u64, offset: i64) -> String {
    let local = epoch as i64 + offset;
    if local < 0 {
        return String::new();
    }
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    // 1970-01-01 was a Thursday, index 4 in a Sunday-first week.
    const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let day = DAY_NAMES[((days + 4).rem_euclid(7)) as usize];
    format!("{:02}:{:02} {day}", seconds / 3_600, (seconds % 3_600) / 60)
}

/// `14:55 Thu` when the window rolls over today, `14:55 Thu 21 Aug` when it
/// does not.
///
/// The weekday alone is unambiguous inside a seven-day window but it is not
/// legible: `Thu` six days out is a calendar lookup the reader has to do in
/// their head, and the weekly window is the one that lands that far away.
/// Today's reset stays short, so the date's presence is itself the signal that
/// the rollover is not today.
fn format_clock_dated(now: u64, epoch: u64, offset: i64) -> String {
    let clock = format_clock(epoch, offset);
    if clock.is_empty() || local_day(epoch, offset) == local_day(now, offset) {
        return clock;
    }
    format!("{clock} {}", format_date(epoch, offset))
}

/// What a card says when a date is not knowable. Written out rather than left
/// blank: a missing row reads as an oversight, `unknown` reads as an answer.
const UNKNOWN: &str = "unknown";

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `13 Sep` — the calendar day something falls on, in the viewer's zone.
///
/// The inverse of the conversion [`parse_rfc3339_epoch`] does on the way in,
/// and for the same reason: this is the only calendar arithmetic in the plugin
/// and a date crate would be a dependency for two lines.
fn format_date(epoch: u64, offset: i64) -> String {
    let local = epoch as i64 + offset;
    if local < 0 {
        return String::new();
    }
    let (_, month, day) = civil_from_days(local.div_euclid(86_400));
    format!("{day} {}", MONTHS[(month - 1) as usize])
}

/// The local calendar day an instant falls on, as a day number.
///
/// The unit every "is this today?" question on this card is asked in: elapsed
/// seconds answer a different question and get 23:59 → 00:01 wrong.
fn local_day(epoch: u64, offset: i64) -> i64 {
    (epoch as i64 + offset).div_euclid(86_400)
}

/// Howard Hinnant's civil-from-days, the inverse of the days-from-civil in
/// [`parse_rfc3339_epoch`]. Returns `(year, month 1..=12, day 1..=31)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month, day)
}

/// `13 Sep · in 25d` — when the plan is charged again and its allowance starts
/// over, which is a different question from when a rate-limit window rolls
/// over, and the one that decides whether it is worth pacing at all.
///
/// A date already in the past is reported as [`UNKNOWN`] rather than printed.
/// Codex's renewal comes from an ID token that is only refreshed while Codex
/// runs, so a machine left alone for a month still holds the previous period's
/// date — and a stale date under a heading that says *renews* reads as one that
/// is coming. Wrong in the reassuring direction is the failure this popup
/// exists to prevent; saying nothing cannot make it.
fn format_renewal(now: u64, renews_at: Option<u64>, offset: i64) -> String {
    let Some(at) = renews_at.filter(|at| *at > now) else {
        return UNKNOWN.into();
    };
    let date = format_date(at, offset);
    if date.is_empty() {
        return UNKNOWN.into();
    }
    // Counted in local calendar days, not in elapsed seconds: "tomorrow" means
    // the next day on the wall, and a renewal 20 hours away can be either.
    let when = match local_day(at, offset) - local_day(now, offset) {
        d if d <= 0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        d => format!("in {d}d"),
    };
    format!("{date} · {when}")
}

/// How much to trust the card: `just now`, `12m ago`, `2d ago`, and the wall
/// clock the window rolls over at.
///
/// Codex's numbers are as old as your last Codex turn, so "as of" is not a
/// footnote here — a two-day-old 42% read as current is exactly the mistake this
/// popup exists to prevent.
fn freshness(now: u64, measured_at: Option<u64>) -> String {
    let Some(at) = measured_at else {
        return "as of unknown".into();
    };
    let age = now.saturating_sub(at);
    match age {
        0..=59 => "as of just now".into(),
        60..=3_599 => format!("as of {}m ago", age / 60),
        3_600..=86_399 => format!("as of {}h ago", age / 3_600),
        _ => format!("as of {}d ago", age / 86_400),
    }
}

/// Green while there is room, yellow once the end is in sight, red when it is
/// close enough to change what you do next. This grades the per-window bars;
/// the donut uses fixed used/available semantic colours instead.
fn level_color(theme: &Theme, pct: f64, cfg: &Config) -> Color {
    if pct >= cfg.usage.alert_percent as f64 {
        theme.or("red", Color::Red)
    } else if pct >= cfg.usage.warn_percent as f64 {
        theme.or("yellow", Color::Yellow)
    } else {
        theme.or("green", Color::Green)
    }
}

// --- the popup -------------------------------------------------------------

pub struct App {
    theme: Theme,
    title_color: Color,
    cfg: Config,
    slots: Vec<Slot>,
    /// Results still in flight from the worker thread, one message per provider.
    inbox: Option<Receiver<(usize, Result<Report, String>)>>,
    /// The clock every relative time on screen is measured against, sampled once
    /// per read rather than per draw — so two cards drawn in the same frame
    /// cannot disagree about what "12m ago" means.
    now: u64,
    /// Seconds east of UTC, for the absolute reset times.
    offset: i64,
    /// The command bar's row and its pills, each carrying the key its cap
    /// names. Written by [`draw_bar`], the loop that lays them out.
    bar_row: u16,
    bar_zones: Vec<(u16, u16, KeyCode)>,
}

impl SimpleMode for App {
    fn draw(&mut self, f: &mut Frame) {
        self.drain();
        draw(f, self);
    }

    fn on_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => Flow::Quit,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Flow::Quit,
            KeyCode::Char('r') => {
                self.refresh();
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }

    /// Pills only. There is nothing here to scroll: `draw` lays every card to
    /// fit the pane by construction, so a wheel would have nowhere to move to.
    fn on_mouse(&mut self, m: MouseEvent) -> Flow {
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some(code) =
                tui::zone_at(&self.bar_zones, self.bar_row, (m.column, m.row).into())
            {
                return self.on_key(KeyEvent::from(code));
            }
        }
        Flow::Continue
    }
}

impl App {
    /// Collect whatever the worker has finished. Non-blocking: the draw loop
    /// wakes every 200ms anyway, so a card fills itself in within a frame of the
    /// request landing.
    fn drain(&mut self) {
        let Some(inbox) = &self.inbox else { return };
        let mut done = false;
        loop {
            match inbox.try_recv() {
                Ok((index, result)) => {
                    let name = self.slots[index].name().to_string();
                    self.slots[index] = match result {
                        Ok(report) => Slot::Ready(report),
                        Err(reason) => Slot::Unavailable { name, reason },
                    };
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        if done {
            self.inbox = None;
        }
    }

    /// Re-read everything: offline providers inline, networked ones on a worker.
    fn refresh(&mut self) {
        let cfg = self.cfg.clone();
        let enabled = enabled_providers(&cfg);
        let (tx, rx) = mpsc::channel();
        self.slots = enabled
            .iter()
            .map(|provider| Slot::Loading {
                name: provider.name().into(),
            })
            .collect();
        // Offline first and inline: it costs a file read, and having Codex on
        // screen while Claude's request is still open is the whole point of
        // splitting them.
        for (index, provider) in enabled.iter().enumerate() {
            if provider.offline() {
                self.slots[index] = slot_of(provider.name(), provider.load(&SystemRunner, &cfg));
            }
        }
        let networked = enabled
            .iter()
            .enumerate()
            .filter(|(_, provider)| !provider.offline())
            .map(|(index, provider)| (index, provider.id()))
            .collect::<Vec<_>>();
        self.now = now();
        self.offset = local_offset(&SystemRunner);
        if networked.is_empty() {
            self.inbox = None;
            return;
        }
        thread::spawn(move || {
            for (index, id) in networked {
                let Some(provider) = providers().into_iter().find(|p| p.id() == id) else {
                    continue;
                };
                let result = provider
                    .load(&SystemRunner, &cfg)
                    .map_err(|error| error.to_string());
                // A closed receiver means the popup is gone; stop, don't panic.
                if tx.send((index, result)).is_err() {
                    return;
                }
            }
        });
        self.inbox = Some(rx);
    }
}

fn slot_of(name: &str, result: Result<Report>) -> Slot {
    match result {
        Ok(report) => Slot::Ready(report),
        Err(error) => Slot::Unavailable {
            name: name.to_string(),
            reason: error.to_string(),
        },
    }
}

/// The providers this config asks for, in the order it lists them. An unknown
/// name is ignored rather than fatal — a config written for a later version
/// should not stop the popup opening.
fn enabled_providers(cfg: &Config) -> Vec<Box<dyn Provider>> {
    let wanted = &cfg.usage.providers;
    let mut available = providers();
    let mut chosen = Vec::new();
    for id in wanted {
        if let Some(position) = available.iter().position(|provider| provider.id() == id) {
            chosen.push(available.remove(position));
        }
    }
    chosen
}

/// The blank columns between two provider cards.
///
/// Nothing draws a divider — the pane is transparent and a rule between the
/// cards would compete with the one each card already draws above its facts —
/// so the whitespace is the only thing separating them, and adjacent cards read
/// as one wide table of rows rather than two independent answers.
const CARD_GAP: u16 = 3;

/// The width a card needs before it can spare the full gutter: the longest row
/// it draws, `label + bar + percentage + a dated countdown`, plus the column
/// `draw_card` keeps clear on the right.
const CARD_COMFORTABLE: u16 = 44;

/// The gutter, but never at the expense of a card that is already cutting its
/// own rows. Two providers on the 96-column popup get the full gap; a pane
/// squeezed narrower spends its columns on content instead.
fn card_gap(width: u16, cards: usize) -> u16 {
    let cards = cards.max(1) as u16;
    let each = width.saturating_sub(CARD_GAP * (cards - 1)) / cards;
    if each >= CARD_COMFORTABLE {
        CARD_GAP
    } else {
        1
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
    // Herdr frames and titles the popup pane already, so this draws no outer
    // border of its own — the same reason the changelog pane doesn't.
    if app.slots.is_empty() {
        let muted = app.theme.or("subtext0", Color::Gray);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " No usage providers are enabled.",
                Style::default().fg(muted),
            ))),
            rows[0],
        );
    } else {
        // `Fill` rather than `Percentage(100 / n)`, which throws away the
        // remainder — three providers took 99% of the pane and left a column
        // dark on the right.
        let columns = Layout::horizontal(
            app.slots
                .iter()
                .map(|_| Constraint::Fill(1))
                .collect::<Vec<_>>(),
        )
        .spacing(card_gap(rows[0].width, app.slots.len()))
        .split(rows[0]);
        // Built once, here, because the same list has to size the cards and
        // fill them: a count taken from one list and a render taken from
        // another drift the moment a row is added to only one of them.
        let facts = app
            .slots
            .iter()
            .map(|slot| card_facts(slot, app.now, app.offset))
            .collect::<Vec<_>>();
        // Every card gets the same row heights, taken from the busiest one.
        // Sized per card, a provider with one window and a provider with four
        // put their donuts — and every line under them — at different heights,
        // and two cards that will not line up read as two unrelated widgets.
        let rows = CardRows {
            bars: app
                .slots
                .iter()
                .map(|slot| slot.windows().len() as u16)
                .max()
                .unwrap_or(0),
            facts: facts.iter().map(|f| f.len() as u16).max().unwrap_or(0),
        };
        for ((slot, area), facts) in app.slots.iter().zip(columns.iter()).zip(facts.iter()) {
            draw_card(f, app, slot, facts, *area, rows);
        }
    }
    draw_bar(f, app, rows[1]);
}

/// One provider: a heading, a donut for the window closest to running out, a bar
/// per window, and the facts that give the percentage its context.
///
/// The vertical order is deliberate: the donut answers "am I fine?" from across
/// the room, the bars answer "which window?", and the facts answer "why?" — so
/// the card degrades gracefully when the pane is short, losing the explanation
/// before it loses the answer.
#[derive(Clone, Copy)]
struct CardRows {
    bars: u16,
    facts: u16,
}

/// A card's facts as they reach the screen: what the provider recorded, plus
/// the `renews` row, which only the drawing layer can build — it needs the
/// clock and the local offset the whole frame shares.
fn card_facts(slot: &Slot, now: u64, offset: i64) -> Vec<Fact> {
    let mut facts = slot.facts().to_vec();
    if let Slot::Ready(report) = slot {
        facts.push(Fact::new(
            "renews",
            format_renewal(now, report.renews_at, offset),
        ));
    }
    facts
}

fn draw_card(f: &mut Frame, app: &App, slot: &Slot, facts: &[Fact], area: Rect, card: CardRows) {
    let theme = &app.theme;
    let text = theme.or("text", Color::Reset);
    let muted = theme.or("subtext0", Color::Gray);
    let track = theme.or("overlay0", Color::DarkGray);

    let windows = slot.windows();
    // Bars, then a rule, then the facts — all reserved before the donut is
    // sized, so the donut takes what is left rather than crowding them out.
    let facts_h = if card.facts == 0 { 0 } else { card.facts + 1 };
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(card.bars),
        Constraint::Length(facts_h),
    ])
    .split(area);

    let mut head = vec![Span::styled(
        format!(" {}", slot.name()),
        Style::default()
            .fg(app.title_color)
            .add_modifier(Modifier::BOLD),
    )];
    if let Slot::Ready(report) = slot {
        if let Some(plan) = &report.plan {
            head.push(Span::styled(
                format!("  {plan}"),
                Style::default().fg(muted),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(head)), rows[0]);

    let hottest = slot.hottest();
    let pct = hottest.map(|window| window.used_percent);
    let (used_color, available_color) = donut_colors(theme, pct);
    draw_donut(f, pct, used_color, available_color, rows[1]);
    // The number sits in the hole rather than beside the ring, so the eye lands
    // on the figure and the colour reads as context around it.
    let label = match slot {
        Slot::Loading { .. } => "…".to_string(),
        Slot::Ready(_) => pct
            .map(|p| format!("{p:.0}%"))
            .unwrap_or_else(|| "—".into()),
        Slot::Unavailable { .. } => "—".into(),
    };
    let hole = centered(rows[1], label.chars().count() as u16 + 2, 1);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            label,
            Style::default().fg(used_color).add_modifier(Modifier::BOLD),
        )))
        .centered(),
        hole,
    );

    // The one line that says how much of this card to trust.
    let status = match slot {
        Slot::Loading { .. } => Line::from(Span::styled(" reading…", Style::default().fg(muted))),
        Slot::Unavailable { reason, .. } => Line::from(Span::styled(
            format!(" {reason}"),
            Style::default().fg(muted),
        )),
        Slot::Ready(report) => {
            let mut note = freshness(app.now, report.measured_at);
            if let Some(at) = hottest.and_then(|window| window.resets_at) {
                let clock = format_clock_dated(app.now, at, app.offset);
                if !clock.is_empty() {
                    note = format!("{note} · resets {clock}");
                }
            }
            Line::from(Span::styled(format!(" {note}"), Style::default().fg(muted)))
        }
    };
    f.render_widget(Paragraph::new(status), rows[2]);

    let width = rows[3].width.saturating_sub(1) as usize;
    let bars = windows
        .iter()
        .map(|window| window_line(window, theme, app, width, text, muted))
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(bars), rows[3]);

    if facts_h > 0 && !facts.is_empty() {
        let mut lines = vec![Line::from(Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(1))),
            Style::default().fg(track),
        ))];
        // A long value — an email on a narrow pane — is cut rather than wrapped:
        // one fact is one row, so a wrap would push every row below it out of
        // line with the card beside it.
        let value_w = width.saturating_sub(FACT_KEY + 1);
        lines.extend(facts.iter().map(|fact| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<FACT_KEY$}", clip(&fact.key, FACT_KEY)),
                    Style::default().fg(muted),
                ),
                Span::styled(clip(&fact.value, value_w), Style::default().fg(text)),
            ])
        }));
        f.render_widget(Paragraph::new(lines), rows[4]);
    }
}

/// The width of a fact's label column, wide enough for the longest key either
/// provider produces plus a gap.
const FACT_KEY: usize = 10;
/// How many cells a window's bar gets. Fixed so the bars of both cards line up
/// even when their labels differ in length.
const BAR_W: usize = 12;

/// `weekly  ████████░░░░  42%   1d 12h` — one window, on one line.
fn window_line(
    window: &Window,
    theme: &Theme,
    app: &App,
    width: usize,
    text: Color,
    muted: Color,
) -> Line<'static> {
    let color = window_color(theme, window, &app.cfg);
    let filled = ((window.used_percent / 100.0) * BAR_W as f64).round() as usize;
    let filled = filled.min(BAR_W);
    // The reservation is the bar plus " NNN%" plus the reset text, whose worst
    // case is the dated form (`1d 12h · 19 Aug`) and its two leading spaces. On
    // a card too narrow for all of it the label is what yields.
    let label_w = width.saturating_sub(BAR_W + 23).clamp(6, 8);
    let mut spans = vec![
        Span::styled(
            format!(" {:<label_w$}", clip(&window.label, label_w)),
            Style::default().fg(muted),
        ),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(BAR_W - filled),
            Style::default().fg(theme.or("overlay0", Color::DarkGray)),
        ),
        Span::styled(
            format!(" {:>3.0}%", window.used_percent),
            Style::default().fg(text),
        ),
    ];
    if let Some(at) = window.resets_at {
        spans.push(Span::styled(
            format!("  {}", format_reset_dated(app.now, at, app.offset)),
            Style::default().fg(muted),
        ));
    }
    Line::from(spans)
}

/// A window's colour: the provider's own grading when it offers one, and the
/// configured thresholds when it does not.
fn window_color(theme: &Theme, window: &Window, cfg: &Config) -> Color {
    match window.severity {
        Some(Severity::Normal) => theme.or("green", Color::Green),
        Some(Severity::Warning) => theme.or("yellow", Color::Yellow),
        Some(Severity::Critical) => theme.or("red", Color::Red),
        None => level_color(theme, window.used_percent, cfg),
    }
}

/// A ready donut is a composition rather than a severity gauge: red is quota
/// already spent and green is quota still available. Without a reading both
/// arcs stay muted so an unavailable provider cannot look fully available.
fn donut_colors(theme: &Theme, pct: Option<f64>) -> (Color, Color) {
    if pct.is_some() {
        (theme.or("red", Color::Red), theme.or("green", Color::Green))
    } else {
        let muted = theme.or("overlay0", Color::DarkGray);
        (muted, muted)
    }
}

fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
}

fn draw_donut(
    f: &mut Frame,
    pct: Option<f64>,
    used_color: Color,
    available_color: Color,
    area: Rect,
) {
    // A terminal cell is about twice as tall as it is wide, so a ring drawn on
    // equal x/y bounds comes out as an ellipse unless the canvas is given twice
    // the columns it has rows. Squaring it here is what makes it look round.
    let height = area.height.min(DONUT_MAX_H).min(area.width / 2);
    if height == 0 {
        return;
    }
    let width = (height * 2).min(area.width);
    let square = centered(area, width, height);
    let (used, rest) = donut_points(pct.unwrap_or(0.0), RING_INNER, RING_OUTER, ARC_STEPS);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([-1.0, 1.0])
        .y_bounds([-1.0, 1.0])
        .paint(move |ctx| {
            ctx.draw(&Points {
                coords: &rest,
                color: available_color,
            });
            if pct.is_some() {
                ctx.draw(&Points {
                    coords: &used,
                    color: used_color,
                });
            }
        });
    f.render_widget(canvas, square);
}

/// A `width`×`height` rect in the middle of `area`, clamped to it.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let ink = theme.or("panel_bg", Color::Rgb(16, 18, 20));
    let muted = theme.or("subtext0", Color::Gray);
    // Each pill beside the key its cap advertises, so a click cannot mean
    // something other than what the label says.
    let caps = [
        (
            Pill::new("r", "refresh", theme.or("accent", Color::Cyan)),
            KeyCode::Char('r'),
        ),
        (
            Pill::new("esc", "close", theme.or("red", Color::Red)),
            KeyCode::Esc,
        ),
    ];
    let pills: Vec<Pill> = caps
        .iter()
        .map(|(p, _)| Pill::new(p.key, p.label, p.color))
        .collect();
    let (mut spans, zones) = tui::pill_row(&pills, ink, area.x);
    app.bar_row = area.y;
    app.bar_zones = zones
        .into_iter()
        .zip(caps.iter())
        .map(|((a, b), (_, code))| (a, b, *code))
        .collect();
    if app.inbox.is_some() {
        spans.push(Span::styled("fetching…", Style::default().fg(muted)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Entry point for `herdr-switchboard --usage`.
pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let title_color = theme
        .resolve(&cfg.common.title_color)
        .unwrap_or_else(|| theme.or("peach", Color::Yellow));
    let mut app = App {
        theme,
        title_color,
        cfg,
        slots: Vec::new(),
        inbox: None,
        now: now(),
        offset: 0,
        bar_row: 0,
        bar_zones: Vec::new(),
    };
    // Load before claiming the terminal, like the projects picker: the offline
    // provider is already on screen in the first frame.
    app.refresh();
    tui::run_simple(&mut app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    /// A real rollout line, trimmed to the fields this module reads.
    /// The same event with the `info` block Codex actually writes beside the
    /// limits, trimmed to the fields the card reads.
    const CODEX_FULL: &str = r#"{"timestamp":"2026-08-18T15:05:51.095Z","payload":{"type":"token_count","info":{"last_token_usage":{"total_tokens":162409},"model_context_window":258400,"total_token_usage":{"cached_input_tokens":18398720,"input_tokens":19008954,"total_tokens":19074393}},"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":10080,"resets_at":1787196988},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"prolite","rate_limit_reached_type":null}}}"#;

    const CODEX_LINE: &str = r#"{"timestamp":"2026-08-18T14:22:03.089Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":41.0,"window_minutes":10080,"resets_at":1787196988},"secondary":null,"credits":{"has_credits":false,"unlimited":false,"balance":"0"},"plan_type":"prolite"}}}"#;

    #[test]
    fn a_codex_rollout_line_becomes_a_report() {
        let report = parse_codex_rate_limits(CODEX_LINE).expect("parses");
        assert_eq!(report.plan.as_deref(), Some("prolite"));
        assert_eq!(
            report.windows,
            vec![Window {
                label: "weekly".into(),
                used_percent: 41.0,
                resets_at: Some(1787196988),
                severity: None
            }]
        );
        // A plain subscription reports a zero balance; that is not a credit note.
        assert_eq!(fact(&report, "credits").as_deref(), Some("none"));
    }

    #[test]
    fn both_codex_windows_are_read_when_the_plan_has_two() {
        let line = CODEX_LINE.replace(
            r#""secondary":null"#,
            r#""secondary":{"used_percent":12.5,"window_minutes":300,"resets_at":1787000000}"#,
        );
        let report = parse_codex_rate_limits(&line).expect("parses");
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[1].label, "5h");
        assert_eq!(report.windows[1].used_percent, 12.5);
    }

    #[test]
    fn a_codex_line_without_rate_limits_is_an_error_not_an_empty_card() {
        let line = r#"{"payload":{"type":"token_count"}}"#;
        assert!(parse_codex_rate_limits(line).is_err());
        assert!(parse_codex_rate_limits("not json at all").is_err());
    }

    #[test]
    fn codex_credits_only_show_when_the_account_has_them() {
        let line = CODEX_LINE.replace(
            r#""has_credits":false,"unlimited":false,"balance":"0""#,
            r#""has_credits":true,"unlimited":false,"balance":"12.50""#,
        );
        let report = parse_codex_rate_limits(&line).expect("parses");
        assert_eq!(fact(&report, "credits").as_deref(), Some("12.50"));
    }

    #[test]
    fn window_lengths_read_the_way_the_card_says_them() {
        assert_eq!(window_label(10080), "weekly");
        assert_eq!(window_label(1440), "daily");
        assert_eq!(window_label(300), "5h");
        assert_eq!(window_label(60), "1h");
        assert_eq!(window_label(45), "45m");
        assert_eq!(window_label(4320), "3d");
        assert_eq!(window_label(0), "window");
    }

    #[test]
    fn a_reset_is_stated_coarsely_and_never_in_the_past() {
        assert_eq!(format_reset(1000, 1000 + 86_400 + 14 * 3_600), "1d 14h");
        assert_eq!(format_reset(1000, 1000 + 42 * 60), "42m");
        assert_eq!(format_reset(1000, 1000 + 3 * 3_600 + 5 * 60), "3h 5m");
        assert_eq!(format_reset(1000, 1000), "now");
        // A window that has already rolled over reads as `now`, not as a huge
        // number wrapped around by the subtraction.
        assert_eq!(format_reset(5000, 1000), "now");
        // Under a minute still says a minute: `0m` reads as broken.
        assert_eq!(format_reset(1000, 1030), "1m");
    }

    #[test]
    fn a_percentage_is_clamped_but_never_rescaled() {
        assert_eq!(clamp_percent(41.0), 41.0);
        assert_eq!(clamp_percent(0.0), 0.0);
        assert_eq!(clamp_percent(140.0), 100.0);
        assert_eq!(clamp_percent(-3.0), 0.0);
        assert_eq!(clamp_percent(f64::NAN), 0.0);
        // A barely-used plan stays barely used. Reading 0.8 as "80%" is the bug
        // this replaced, and it fired every time a window rolled over.
        assert_eq!(clamp_percent(0.8), 0.8);
    }

    #[test]
    fn the_claude_payload_is_read_under_either_key_convention() {
        let snake = r#"{"five_hour":{"utilization":78.0,"resets_at":1787200000},
                        "seven_day":{"utilization":31.0,"resets_at":1787600000}}"#;
        let camel = r#"{"five_hour":{"utilization":78,"resetsAt":1787200000},
                        "seven_day":{"utilization":31,"resetsAt":1787600000}}"#;
        for body in [snake, camel] {
            let report = parse_claude_usage(body, 1_787_000_000).expect("parses");
            assert_eq!(report.name, "Claude Code");
            assert_eq!(report.windows[0].label, "5h");
            assert_eq!(report.windows[0].used_percent, 78.0);
            assert_eq!(report.windows[0].resets_at, Some(1787200000));
            assert_eq!(report.windows[1].used_percent, 31.0);
        }
    }

    /// Trimmed from a live response. Two things here are not guesses: buckets a
    /// plan does not have arrive as `null` rather than absent, and a reset is an
    /// RFC 3339 string with microseconds and an explicit offset, not an integer.
    #[test]
    fn the_shape_the_endpoint_actually_returns_is_handled() {
        let body = r#"{"five_hour":{"limit_dollars":null,"remaining_dollars":null,
                          "resets_at":"2026-08-18T19:09:59.713825+00:00",
                          "used_dollars":null,"utilization":51.0},
                       "seven_day":{"resets_at":"2026-08-20T08:59:59.713848+00:00",
                          "utilization":37.0},
                       "seven_day_opus":null,"seven_day_sonnet":null}"#;
        let report = parse_claude_usage(body, 1_787_000_000).expect("parses");
        assert_eq!(
            report.windows,
            vec![
                Window {
                    label: "5h".into(),
                    used_percent: 51.0,
                    resets_at: Some(1787080199),
                    severity: None
                },
                Window {
                    label: "7d".into(),
                    used_percent: 37.0,
                    resets_at: Some(1787216399),
                    severity: None
                },
            ],
            "a null bucket is skipped, not drawn as zero"
        );
        // This endpoint names no plan, so the card simply shows none.
        assert_eq!(report.plan, None);
    }

    #[test]
    fn an_iso_reset_timestamp_is_understood_too() {
        let body = r#"{"five_hour":{"utilization":0.5,"resetsAt":"2026-08-18T14:22:03Z"}}"#;
        let report = parse_claude_usage(body, 1_787_000_000).expect("parses");
        // 2026-08-18T14:22:03Z, cross-checked against `date -u -j`.
        assert_eq!(report.windows[0].resets_at, Some(1787062923));
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_epoch("2000-03-01T00:00:00Z"), Some(951868800));
        assert_eq!(parse_rfc3339_epoch("nonsense"), None);
    }

    #[test]
    fn an_unknown_claude_payload_is_an_error_rather_than_a_confident_zero() {
        // Saying `0%` on a plan that is nearly spent is worse than saying nothing.
        assert!(parse_claude_usage(r#"{"error":"unauthorized"}"#, 0).is_err());
        assert!(parse_claude_usage("<html>nope</html>", 0).is_err());
    }

    #[test]
    fn the_claude_token_is_read_out_of_the_credentials_blob() {
        let raw = r#"{"claudeAiOauth":{"accessToken":"sk-test","expiresAt":1}}"#;
        assert_eq!(parse_claude_token(raw).unwrap(), "sk-test");
        assert!(parse_claude_token(r#"{"claudeAiOauth":{}}"#).is_err());
        assert!(parse_claude_token("").is_err());
    }

    #[test]
    fn the_token_reaches_curl_through_stdin_and_never_through_argv() {
        // argv is readable through `ps` by every process this user owns, so a
        // token on the command line would leak for the length of the call.
        let runner = MockRunner::new().on("curl", r#"{"five_hour":{"utilization":0.1}}"#);
        Claude::fetch(&runner, "sk-secret", 3000).expect("fetches");

        let argv = runner.calls().concat().join(" ");
        assert!(
            !argv.contains("sk-secret"),
            "token leaked into argv: {argv}"
        );
        assert!(
            argv.contains("--config"),
            "curl must read its headers from a config"
        );
        assert!(runner
            .stdins()
            .iter()
            .any(|body| body.contains("sk-secret")));
    }

    #[test]
    fn a_failed_request_is_an_error_not_an_empty_report() {
        let runner = MockRunner::new().failing("curl");
        assert!(Claude::fetch(&runner, "sk-secret", 3000).is_err());
    }

    #[test]
    fn codex_facts_explain_the_percentage_beside_them() {
        let report = parse_codex_rate_limits(CODEX_FULL).expect("parses");
        assert_eq!(
            fact(&report, "session").as_deref(),
            Some("19.1M · 97% cached")
        );
        assert_eq!(
            fact(&report, "context").as_deref(),
            Some("162k / 258k  (63%)")
        );
        assert_eq!(fact(&report, "credits").as_deref(), Some("none"));
        // No limit has been hit, so no line claims one has.
        assert_eq!(fact(&report, "limit hit"), None);
        assert_eq!(report.measured_at, Some(1_787_065_551));
    }

    #[test]
    fn a_codex_card_says_when_a_limit_was_actually_hit() {
        let line = CODEX_FULL.replace(
            r#""rate_limit_reached_type":null"#,
            r#""rate_limit_reached_type":"weekly""#,
        );
        let report = parse_codex_rate_limits(&line).expect("parses");
        assert_eq!(fact(&report, "limit hit").as_deref(), Some("weekly"));
    }

    #[test]
    fn claude_takes_its_grading_from_the_api_not_from_our_thresholds() {
        // The provider knows what its own plan considers close to the edge, and
        // says so per limit; matching is by percentage because the buckets and
        // the limits array share no id.
        let body = r#"{"five_hour":{"utilization":91.0},"seven_day":{"utilization":38.0},
                       "limits":[{"percent":91,"severity":"critical","kind":"session"},
                                 {"percent":38,"severity":"normal","kind":"weekly_all"}]}"#;
        let report = parse_claude_usage(body, 1_787_000_000).expect("parses");
        assert_eq!(report.windows[0].severity, Some(Severity::Critical));
        assert_eq!(report.windows[1].severity, Some(Severity::Normal));

        let theme = Theme::from_slots(&[("red", "#ff0000"), ("green", "#00ff00")]);
        let cfg = Config::default();
        // 91% would be red on our thresholds too, but 38% would be green either
        // way — what matters is that the API's word is what got used.
        assert_eq!(
            window_color(&theme, &report.windows[0], &cfg),
            Color::Rgb(255, 0, 0)
        );
    }

    #[test]
    fn a_window_the_api_did_not_grade_falls_back_to_the_configured_thresholds() {
        let theme = Theme::from_slots(&[
            ("green", "#00ff00"),
            ("yellow", "#ffff00"),
            ("red", "#ff0000"),
        ]);
        let cfg = Config::default(); // warn 60, alert 85
        let ungraded = |pct| Window {
            label: "weekly".into(),
            used_percent: pct,
            resets_at: None,
            severity: None,
        };
        assert_eq!(
            window_color(&theme, &ungraded(10.0), &cfg),
            Color::Rgb(0, 255, 0)
        );
        assert_eq!(
            window_color(&theme, &ungraded(70.0), &cfg),
            Color::Rgb(255, 255, 0)
        );
        assert_eq!(
            window_color(&theme, &ungraded(90.0), &cfg),
            Color::Rgb(255, 0, 0)
        );
    }

    #[test]
    fn claude_facts_say_what_happens_when_the_plan_runs_out() {
        let off = r#"{"five_hour":{"utilization":10.0},
                      "spend":{"enabled":false,"percent":0},
                      "extra_usage":{"is_enabled":false,"spend_limit_reached":false}}"#;
        let report = parse_claude_usage(off, 0).expect("parses");
        assert_eq!(fact(&report, "credits").as_deref(), Some("off"));
        assert_eq!(fact(&report, "extra"), None);

        let on = r#"{"five_hour":{"utilization":10.0},
                     "spend":{"enabled":true,"cap":25.0,
                              "used":{"amount_minor":250,"currency":"USD","exponent":2}},
                     "extra_usage":{"is_enabled":true,"used_credits":1.5}}"#;
        let report = parse_claude_usage(on, 0).expect("parses");
        assert_eq!(fact(&report, "credits").as_deref(), Some("$2.50 / $25"));
        assert_eq!(fact(&report, "extra").as_deref(), Some("1.50"));
    }

    /// `{"alg":"RS256"}` . `{"email":"cuong.cn0103@gmail.com","name":"Cao Ngoc Cuong"}` . sig
    const ID_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6ImN1b25nLmNuMDEwM0BnbWFpbC5jb20iLCJuYW1lIjoiQ2FvIE5nb2MgQ3VvbmcifQ.not-a-real-signature";

    /// The same, plus the namespaced claim a real ChatGPT token carries:
    /// `{"chatgpt_plan_type":"pro","chatgpt_subscription_active_start":"2026-08-13T07:09:59+00:00",
    /// "chatgpt_subscription_active_until":"2026-09-13T07:09:59+00:00"}`.
    const ID_TOKEN_WITH_SUBSCRIPTION: &str = "eyJhbGciOiJSUzI1NiJ9.eyJlbWFpbCI6ImN1b25nLmNuMDEwM0BnbWFpbC5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJwcm8iLCJjaGF0Z3B0X3N1YnNjcmlwdGlvbl9hY3RpdmVfc3RhcnQiOiIyMDI2LTA4LTEzVDA3OjA5OjU5KzAwOjAwIiwiY2hhdGdwdF9zdWJzY3JpcHRpb25fYWN0aXZlX3VudGlsIjoiMjAyNi0wOS0xM1QwNzowOTo1OSswMDowMCJ9fQ.not-a-real-signature";

    fn codex_auth(token: &str) -> String {
        format!(
            r#"{{"auth_mode":"chatgpt","OPENAI_API_KEY":null,
                 "tokens":{{"id_token":"{token}","access_token":"secret-access",
                            "refresh_token":"secret-refresh","account_id":"acc"}}}}"#
        )
    }

    #[test]
    fn the_codex_account_comes_out_of_the_id_token() {
        let identity = identity_from_codex_auth(&codex_auth(ID_TOKEN)).expect("reads");
        assert_eq!(identity.email.as_deref(), Some("cuong.cn0103@gmail.com"));
        // Anything unreadable simply leaves the card without an account line.
        assert_eq!(identity_from_codex_auth("{}"), None);
        assert_eq!(
            identity_from_codex_auth(r#"{"tokens":{"id_token":"junk"}}"#),
            None
        );
        assert_eq!(identity_from_codex_auth("not json"), None);
    }

    #[test]
    fn the_codex_renewal_date_comes_out_of_the_same_token() {
        // One read, one decode, two labels: the address and the date the plan
        // is charged again. The rollout that carries the percentages knows
        // nothing about the subscription.
        let identity =
            identity_from_codex_auth(&codex_auth(ID_TOKEN_WITH_SUBSCRIPTION)).expect("reads");
        assert_eq!(identity.email.as_deref(), Some("cuong.cn0103@gmail.com"));
        assert_eq!(identity.renews_at, Some(1_789_283_399)); // 2026-09-13T07:09:59Z

        // A token without the claim still names the account; the card simply
        // has no date to show.
        let plain = identity_from_codex_auth(&codex_auth(ID_TOKEN)).expect("reads");
        assert_eq!(plain.renews_at, None);
    }

    #[test]
    fn a_jwt_payload_is_read_without_being_trusted() {
        // No padding, and `-`/`_` for `+`/`/` — the base64url a JWT uses.
        assert_eq!(base64url_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64url_decode("").unwrap(), b"");
        assert_eq!(base64url_decode("_-8").unwrap(), vec![0xff, 0xef]);
        // Padded or otherwise non-base64url input is refused rather than
        // silently decoded into nonsense.
        assert_eq!(base64url_decode("aGVsbG8="), None);
        assert_eq!(base64url_decode("hello!"), None);
        assert!(jwt_claims("only.two").is_none());
    }

    #[test]
    fn the_claude_account_and_plan_come_out_of_claude_json() {
        let raw = r#"{"userID":"abc","oauthAccount":{
            "emailAddress":"dangduyz934@gmail.com","displayName":"dang duy",
            "organizationType":"claude_pro","billingType":"apple_subscription"}}"#;
        let profile = profile_from_claude_json(raw).expect("reads");
        assert_eq!(profile.email.as_deref(), Some("dangduyz934@gmail.com"));
        assert_eq!(profile.plan.as_deref(), Some("pro"));

        // A file with neither is no profile at all, rather than an empty one.
        assert_eq!(profile_from_claude_json(r#"{"oauthAccount":{}}"#), None);
        assert_eq!(profile_from_claude_json("not json"), None);
    }

    #[test]
    fn a_plan_drops_the_vendor_prefix_the_heading_already_carries() {
        assert_eq!(plan_label("claude_pro"), "pro");
        assert_eq!(plan_label("claude_max"), "max");
        assert_eq!(plan_label("claude_enterprise"), "enterprise");
        // An unfamiliar shape is shown as-is rather than mangled.
        assert_eq!(plan_label("team_seat"), "team seat");
        assert_eq!(plan_label("prolite"), "prolite");
    }

    #[test]
    fn a_long_account_line_is_cut_rather_than_wrapped() {
        // One fact is one row; a wrap would push every row below it out of line
        // with the card beside it.
        let mut app = app_with(vec![Slot::Ready(Report {
            name: "Claude Code".into(),
            plan: Some("pro".into()),
            renews_at: None,
            windows: vec![Window {
                label: "5h".into(),
                used_percent: 58.0,
                resets_at: None,
                severity: None,
            }],
            facts: vec![Fact::new(
                "account",
                "a-very-long-address-indeed@example-domain.com",
            )],
            measured_at: Some(1_787_000_000),
        })]);
        let screen = render(&mut app, 40, 26);
        assert!(screen.contains('…'), "the value is elided: {screen}");
        for row in screen.lines() {
            assert!(row.chars().count() <= 40, "row overflows: {row:?}");
        }
    }

    #[test]
    fn large_counts_are_written_the_way_a_glance_reads_them() {
        assert_eq!(compact_count(19_074_393), "19.1M");
        assert_eq!(compact_count(162_409), "162k");
        assert_eq!(compact_count(840), "840");
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(2_500_000_000), "2.5B");
        // A ratio against nothing is not zero percent, it is no answer.
        assert_eq!(percent_of(5, 0), None);
        assert_eq!(percent_of(1, 2), Some(50.0));
    }

    #[test]
    fn a_reading_states_its_own_age() {
        let now = 1_787_000_000;
        assert_eq!(freshness(now, Some(now)), "as of just now");
        assert_eq!(freshness(now, Some(now - 12 * 60)), "as of 12m ago");
        assert_eq!(freshness(now, Some(now - 5 * 3_600)), "as of 5h ago");
        assert_eq!(freshness(now, Some(now - 2 * 86_400)), "as of 2d ago");
        // A clock that ran backwards must not read as an enormous age.
        assert_eq!(freshness(now, Some(now + 500)), "as of just now");
        assert_eq!(freshness(now, None), "as of unknown");
    }

    #[test]
    fn a_reset_is_also_given_as_a_wall_clock() {
        // 1787065551 is 2026-08-18T15:05:51Z, a Tuesday.
        assert_eq!(format_clock(1_787_065_551, 0), "15:05 Tue");
        // …which is 22:05 the same day in UTC+7, and 08:05 Wed in UTC+17.
        assert_eq!(format_clock(1_787_065_551, 7 * 3_600), "22:05 Tue");
        assert_eq!(format_clock(1_787_065_551, 17 * 3_600), "08:05 Wed");
        assert_eq!(format_clock(1_787_065_551, -18 * 3_600), "21:05 Mon");
    }

    #[test]
    fn a_reset_on_another_day_carries_its_date() {
        // 1787065551 is 2026-08-18T15:05:51Z, a Tuesday.
        let now = 1_787_065_551;
        // Later the same day: the weekday alone already says everything a date
        // would, and the status line is one row on a half-pane-wide card.
        assert_eq!(format_clock_dated(now, now + 3_600, 0), "16:05 Tue");
        // Another day, so the date earns its columns.
        assert_eq!(
            format_clock_dated(now, now + 86_400 + 3_600, 0),
            "16:05 Wed 19 Aug"
        );
        // Whether two instants share a day is only answerable in the viewer's
        // zone: this pair is one day in UTC and two at +7h.
        let evening = 1_787_065_551 + 6 * 3_600; // 21:05 Tue UTC
        assert_eq!(format_clock_dated(now, evening, 0), "21:05 Tue");
        assert_eq!(
            format_clock_dated(now, evening, 7 * 3_600),
            "04:05 Wed 19 Aug"
        );
    }

    #[test]
    fn a_countdown_names_the_day_when_it_is_not_today() {
        // 1787000000 is 2026-08-17T20:53:20Z, a Monday.
        let now = 1_787_000_000;
        // 23:33 the same evening — the countdown is the whole answer.
        assert_eq!(
            format_reset_dated(now, now + 2 * 3_600 + 40 * 60, 0),
            "2h 40m"
        );
        // A day and a half out, where "1d 12h" hides which working day it is.
        assert_eq!(
            format_reset_dated(now, now + 86_400 + 12 * 3_600, 0),
            "1d 12h · 19 Aug"
        );
        // A reset already past reads `now`; a date beside that word would
        // contradict it.
        assert_eq!(format_reset_dated(now, now - 2 * 86_400, 0), "now");
    }

    #[test]
    fn a_renewal_is_written_as_a_date_and_a_countdown() {
        // 1787065551 is 2026-08-18T15:05:51Z.
        let now = 1_787_065_551;
        assert_eq!(format_date(now, 0), "18 Aug");
        // A date is a calendar day, so the viewer's zone can move it: 22:05 the
        // same day in UTC+7, but already Wednesday in UTC+17.
        assert_eq!(format_date(now, 7 * 3_600), "18 Aug");
        assert_eq!(format_date(now, 17 * 3_600), "19 Aug");
        // Leap day and the year boundary, the two dates a hand-rolled calendar
        // gets wrong: 2028-02-29 and 2027-01-01.
        assert_eq!(format_date(1_835_395_200, 0), "29 Feb");
        assert_eq!(format_date(1_798_761_600, 0), "1 Jan");

        assert_eq!(
            format_renewal(now, Some(now + 25 * 86_400), 0),
            "12 Sep · in 25d"
        );
        // Counted in calendar days, not in elapsed seconds: twenty hours ahead
        // is tomorrow when it crosses midnight and today when it does not.
        assert_eq!(
            format_renewal(now, Some(now + 20 * 3_600), 0),
            "19 Aug · tomorrow"
        );
        assert_eq!(format_renewal(now, Some(now + 3_600), 0), "18 Aug · today");
    }

    #[test]
    fn a_renewal_that_is_not_knowable_says_so_rather_than_reading_as_coming() {
        let now = 1_787_065_551;
        // A provider that publishes no date at all — Anthropic keeps none
        // anywhere readable on this machine.
        assert_eq!(format_renewal(now, None, 0), "unknown");
        // And a date already past. Codex's renewal rides an ID token that is
        // only refreshed while Codex runs, so a machine left alone for a month
        // still holds the previous period's date — which under a heading that
        // says *renews* would read as one that is coming.
        assert_eq!(format_renewal(now, Some(now - 86_400), 0), "unknown");
        assert_eq!(format_renewal(now, Some(now), 0), "unknown");
    }

    #[test]
    fn the_local_offset_is_read_from_date_and_degrades_to_utc() {
        assert_eq!(parse_utc_offset("+0700"), Some(25_200));
        assert_eq!(parse_utc_offset("-0430\n"), Some(-16_200));
        assert_eq!(parse_utc_offset("+0000"), Some(0));
        assert_eq!(parse_utc_offset("garbage"), None);
        assert_eq!(parse_utc_offset("+07"), None);

        let runner = MockRunner::new().on("date", "+0700");
        assert_eq!(local_offset(&runner), 25_200);
        // A machine whose `date` cannot answer still gets a usable card.
        assert_eq!(local_offset(&MockRunner::new().failing("date")), 0);
    }

    #[test]
    fn the_donut_splits_its_ring_in_proportion_to_the_percentage() {
        let (used, track) = donut_points(0.0, RING_INNER, RING_OUTER, 360);
        assert!(used.is_empty());
        assert!(!track.is_empty());

        let (used, track) = donut_points(100.0, RING_INNER, RING_OUTER, 360);
        assert!(track.is_empty());
        assert!(!used.is_empty());

        let (used, track) = donut_points(50.0, RING_INNER, RING_OUTER, 360);
        assert_eq!(used.len(), track.len());

        // Out-of-range input is clamped rather than producing a partial ring.
        assert!(donut_points(180.0, RING_INNER, RING_OUTER, 360)
            .1
            .is_empty());
        assert!(donut_points(-5.0, RING_INNER, RING_OUTER, 360).0.is_empty());
    }

    #[test]
    fn the_donut_uses_semantic_colors_and_missing_data_stays_muted() {
        let theme = Theme::from_slots(&[
            ("red", "#ff0000"),
            ("green", "#00ff00"),
            ("overlay0", "#777777"),
        ]);

        for pct in [0.0, 52.0, 100.0] {
            assert_eq!(
                donut_colors(&theme, Some(pct)),
                (Color::Rgb(255, 0, 0), Color::Rgb(0, 255, 0))
            );
        }
        assert_eq!(
            donut_colors(&theme, None),
            (Color::Rgb(119, 119, 119), Color::Rgb(119, 119, 119))
        );
    }

    #[test]
    fn the_donut_starts_at_twelve_oclock_and_runs_clockwise() {
        let (used, _) = donut_points(25.0, 1.0, 1.0, 360);
        let first = used[0];
        assert!(first.0.abs() < 1e-9 && first.1 > 0.9, "starts at the top");
        // A quarter turn clockwise from the top is the three o'clock side.
        let last = used[used.len() - 1];
        assert!(
            last.0 > 0.9 && last.1.abs() < 0.05,
            "sweeps clockwise: {last:?}"
        );
    }

    fn fact(report: &Report, key: &str) -> Option<String> {
        report
            .facts
            .iter()
            .find(|fact| fact.key == key)
            .map(|fact| fact.value.clone())
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with(slots: Vec<Slot>) -> App {
        App {
            theme: Theme::default(),
            title_color: Color::Yellow,
            cfg: Config::default(),
            slots,
            inbox: None,
            now: 1_787_000_000,
            // Fixed rather than local, so `resets 09:16 Fri` in a test means the
            // same thing on a machine in Saigon and one in CI.
            offset: 0,
            bar_row: 0,
            bar_zones: Vec::new(),
        }
    }

    fn codex_slot() -> Slot {
        Slot::Ready(Report {
            name: "Codex".into(),
            plan: Some("prolite".into()),
            // 2026-09-11, twenty-five days past the fixture's `now`.
            renews_at: Some(1_787_000_000 + 25 * 86_400 + 3_600),
            windows: vec![
                Window {
                    label: "weekly".into(),
                    used_percent: 42.0,
                    resets_at: Some(1_787_000_000 + 86_400 + 12 * 3_600),
                    severity: None,
                },
                Window {
                    label: "5h".into(),
                    used_percent: 12.0,
                    resets_at: Some(1_787_000_000 + 2 * 3_600 + 40 * 60),
                    severity: None,
                },
            ],
            facts: vec![
                Fact::new("session", "19.1M · 97% cached"),
                Fact::new("context", "162k / 258k  (63%)"),
                Fact::new("credits", "none"),
            ],
            measured_at: Some(1_787_000_000 - 12 * 60),
        })
    }

    #[test]
    fn a_card_shows_every_window_its_facts_and_how_old_the_reading_is() {
        let mut app = app_with(vec![codex_slot()]);
        let screen = render(&mut app, 60, 26);
        assert!(screen.contains("Codex"), "{screen}");
        assert!(screen.contains("prolite"), "{screen}");
        assert!(
            screen.contains("42%"),
            "the donut carries the hottest window: {screen}"
        );
        // Every window gets its own bar, not just the one the donut took.
        assert!(screen.contains("weekly"), "{screen}");
        assert!(
            screen.contains("12%"),
            "the quieter window is still shown: {screen}"
        );
        // The countdown, and — because that rollover is not today — the day it
        // lands on.
        assert!(screen.contains("1d 12h · 19 Aug"), "{screen}");
        // The other half of the rule: a window rolling over later the same
        // evening stays bare, so the date's presence means "not today".
        assert!(
            screen.contains("2h 40m") && !screen.contains("2h 40m · "),
            "a same-day rollover carries no date: {screen}"
        );
        // The facts that give the percentage its context.
        assert!(screen.contains("19.1M · 97% cached"), "{screen}");
        assert!(screen.contains("162k / 258k"), "{screen}");
        // And when the plan is charged again, which no window reset answers.
        assert!(
            screen.contains("renews") && screen.contains("11 Sep · in 25d"),
            "{screen}"
        );
        assert!(
            screen.contains("refresh"),
            "the command bar is drawn: {screen}"
        );
    }

    #[test]
    fn a_card_says_how_stale_its_numbers_are() {
        // Codex reports whatever its last session wrote, so a card that does not
        // date itself invites planning around a two-day-old percentage.
        let mut app = app_with(vec![codex_slot()]);
        let screen = render(&mut app, 60, 26);
        assert!(screen.contains("as of 12m ago"), "{screen}");
        assert!(
            screen.contains("resets 08:53 Wed 19 Aug"),
            "the wall clock and its date too: {screen}"
        );
    }

    #[test]
    fn cards_are_separated_by_a_gutter_that_yields_before_the_content_does() {
        // Two providers on the real 96-column popup: 46 columns each and the
        // full gutter between them.
        assert_eq!(card_gap(96, 2), CARD_GAP);
        assert_eq!(card_gap(96, 1), CARD_GAP);
        // Squeezed, the cards are already cutting their own rows, so the
        // whitespace is the first thing to go rather than the last.
        assert_eq!(card_gap(96, 3), 1);
        assert_eq!(card_gap(60, 2), 1);
        // Never a divide by zero, however the slots come out.
        assert_eq!(card_gap(0, 0), 1);
    }

    #[test]
    fn a_gutter_actually_separates_two_cards_on_screen() {
        // The pane paints no background and draws no divider, so the blank
        // columns are the only thing that keeps two cards from reading as one
        // wide table of rows.
        let mut app = app_with(vec![codex_slot(), codex_slot()]);
        let screen = render(&mut app, 96, 26);
        let bar = screen
            .lines()
            .find(|line| line.contains("weekly"))
            .expect("a window row is drawn");
        let first = bar.find("weekly").expect("the left card");
        let second = bar.rfind("weekly").expect("the right card");
        assert!(first < second, "two distinct cards: {bar:?}");
        let between = &bar[first..second];
        let blanks = between.len() - between.trim_end().len();
        assert!(
            blanks >= 4,
            "at least the gutter plus each card's own margin: {bar:?}"
        );
    }

    #[test]
    fn every_card_puts_its_rows_at_the_same_height() {
        // A provider with one window and a provider with four must still line
        // up: two cards that do not read as two unrelated widgets.
        let sparse = Slot::Ready(Report {
            name: "Claude Code".into(),
            plan: None,
            renews_at: None,
            windows: vec![Window {
                label: "5h".into(),
                used_percent: 58.0,
                resets_at: None,
                severity: Some(Severity::Normal),
            }],
            facts: vec![Fact::new("credits", "off")],
            measured_at: Some(1_787_000_000),
        });
        let mut app = app_with(vec![codex_slot(), sparse]);
        let screen = render(&mut app, 96, 26);
        // A provider that publishes no renewal still gets the row, so the two
        // cards keep the same fact count and the same height.
        assert!(screen.contains("unknown"), "{screen}");
        let rows: Vec<&str> = screen.lines().collect();
        let row_of = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{screen}"))
        };
        assert_eq!(
            row_of("weekly"),
            row_of("5h      "),
            "bars must align:\n{screen}"
        );
        assert_eq!(
            row_of("session"),
            row_of("credits   off"),
            "facts must align:\n{screen}"
        );
        // Both rules are drawn on the same row.
        let rules: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.contains("──────"))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(rules.len(), 1, "one shared rule row:\n{screen}");
    }

    #[test]
    fn a_provider_that_could_not_be_read_says_why_and_leaves_the_other_alone() {
        let mut app = app_with(vec![
            codex_slot(),
            Slot::Unavailable {
                name: "Claude Code".into(),
                reason: "keychain access denied".into(),
            },
        ]);
        let screen = render(&mut app, 96, 26);
        assert!(screen.contains("keychain access denied"), "{screen}");
        assert!(
            screen.contains("42%"),
            "the other card still reports: {screen}"
        );
    }

    #[test]
    fn a_popup_with_nothing_enabled_still_draws() {
        let mut app = app_with(Vec::new());
        let screen = render(&mut app, 40, 10);
        assert!(screen.contains("No usage providers"), "{screen}");
    }

    #[test]
    fn a_tiny_pane_does_not_panic_the_draw() {
        // Herdr sizes the popup, and a user can end up with far less than it
        // asked for; every rect here is clamped rather than assumed.
        let mut app = app_with(vec![Slot::Loading {
            name: "Codex".into(),
        }]);
        for (w, h) in [(1, 1), (4, 2), (10, 3), (20, 6)] {
            render(&mut app, w, h);
        }
    }

    #[test]
    fn the_hottest_window_is_the_one_the_donut_shows() {
        let slot = Slot::Ready(Report {
            name: "Claude Code".into(),
            plan: None,
            renews_at: None,
            windows: vec![
                Window {
                    label: "5h".into(),
                    used_percent: 12.0,
                    resets_at: None,
                    severity: None,
                },
                Window {
                    label: "7d".into(),
                    used_percent: 78.0,
                    resets_at: None,
                    severity: None,
                },
            ],
            facts: Vec::new(),
            measured_at: Some(1_787_000_000),
        });
        assert_eq!(slot.hottest().map(|w| w.label.as_str()), Some("7d"));
        assert_eq!(
            Slot::Loading {
                name: "Codex".into()
            }
            .hottest(),
            None,
            "a loading card has nothing to promote"
        );
    }

    #[test]
    fn config_chooses_which_providers_run_and_in_what_order() {
        let mut cfg = Config::default();
        cfg.usage.providers = vec!["claude".into(), "codex".into()];
        let ids = enabled_providers(&cfg)
            .iter()
            .map(|provider| provider.id())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["claude", "codex"]);

        cfg.usage.providers = vec!["codex".into()];
        assert_eq!(enabled_providers(&cfg).len(), 1);

        // A name from a later version must not stop the popup opening.
        cfg.usage.providers = vec!["codex".into(), "gemini".into()];
        assert_eq!(enabled_providers(&cfg).len(), 1);
    }

    #[test]
    fn the_newest_rollouts_are_found_without_walking_the_whole_tree() {
        let root = std::env::temp_dir().join(format!("switchboard-usage-{}", std::process::id()));
        let old = root.join("2026/07/01");
        let new = root.join("2026/08/18");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("rollout-old.jsonl"), "{}").unwrap();
        fs::write(new.join("rollout-a.jsonl"), "{}").unwrap();
        fs::write(new.join("rollout-b.jsonl"), "{}").unwrap();
        fs::write(new.join("notes.txt"), "ignored").unwrap();

        let found = recent_rollouts(&root, 3);
        assert_eq!(found.len(), 2, "only the newest day, only jsonl: {found:?}");
        assert!(found.iter().all(|path| path.starts_with(&new)));

        assert!(recent_rollouts(&root.join("missing"), 3).is_empty());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_rate_limit_line_is_found_from_the_end_of_a_large_file() {
        let dir = std::env::temp_dir().join(format!("switchboard-tail-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout.jsonl");
        // Two rate-limit events with padding between them: the later one wins.
        let stale = CODEX_LINE.replace("41.0", "9.0");
        let padding = "{\"payload\":{\"type\":\"noise\"}}\n".repeat(4000);
        fs::write(&path, format!("{stale}\n{padding}{CODEX_LINE}\n{padding}")).unwrap();

        let line = last_rate_limit_line(&path).expect("finds the event");
        let report = parse_codex_rate_limits(&line).expect("parses");
        assert_eq!(report.windows[0].used_percent, 41.0);

        fs::write(&path, "{\"payload\":{}}\n").unwrap();
        assert_eq!(last_rate_limit_line(&path), None);
        fs::remove_dir_all(&dir).ok();
    }
}
