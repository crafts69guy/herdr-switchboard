//! Claude credential and usage endpoint adapter.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use super::Provider;
use crate::config::Config;
use crate::runner::CommandRunner;
use crate::state::now;
use crate::usage::{Fact, Report, Severity, Window};

pub(in crate::usage) struct Claude;

/// The endpoint `/usage` itself calls. Not a documented API: treat every field
/// as optional and every shape as provisional (see [`parse_claude_usage`]).
pub(in crate::usage) const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The keychain service Claude Code stores its OAuth credentials under on macOS.
pub(in crate::usage) const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

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
    pub(in crate::usage) fn fetch(
        runner: &dyn CommandRunner,
        token: &str,
        timeout_ms: u64,
    ) -> Result<String> {
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
pub(in crate::usage) fn claude_token(runner: &dyn CommandRunner) -> Result<String> {
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

pub(in crate::usage) fn parse_claude_token(raw: &str) -> Result<String> {
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
pub(in crate::usage) fn parse_claude_usage(body: &str, measured_at: u64) -> Result<Report> {
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
pub(in crate::usage) fn claude_severities(
    limits: &serde_json::Value,
) -> std::collections::HashMap<String, Severity> {
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
pub(in crate::usage) struct ClaudeProfile {
    pub(in crate::usage) email: Option<String>,
    pub(in crate::usage) plan: Option<String>,
}

pub(in crate::usage) fn claude_profile() -> Option<ClaudeProfile> {
    let text = fs::read_to_string(home().ok()?.join(".claude.json")).ok()?;
    profile_from_claude_json(&text)
}

pub(in crate::usage) fn profile_from_claude_json(text: &str) -> Option<ClaudeProfile> {
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
pub(in crate::usage) fn plan_label(raw: &str) -> String {
    raw.strip_prefix("claude_").unwrap_or(raw).replace('_', " ")
}

/// The `key  value` lines under a Claude card: what happens once the plan runs
/// out, which is the question the percentage raises.
pub(in crate::usage) fn claude_facts(value: &serde_json::Value) -> Vec<Fact> {
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

pub(in crate::usage) fn claude_window(value: &serde_json::Value, label: &str) -> Option<Window> {
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
pub(in crate::usage) fn money(value: &serde_json::Value) -> String {
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
pub(in crate::usage) fn clamp_percent(raw: f64) -> f64 {
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
pub(in crate::usage) fn parse_rfc3339_epoch(text: &str) -> Option<u64> {
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

pub(in crate::usage) fn home() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow!("HOME is not set"))
}
