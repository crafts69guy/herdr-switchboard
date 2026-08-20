//! Codex rollout and identity adapter.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use super::claude::{clamp_percent, home, parse_rfc3339_epoch};
use super::Provider;
use crate::config::Config;
use crate::runner::CommandRunner;
use crate::usage::{Fact, Report, Window};

pub(in crate::usage) struct Codex;

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
pub(in crate::usage) const ROLLOUT_TRIES: usize = 3;

/// Read this much of the file's end first. A `rate_limits` event is appended on
/// every turn, so the last one is almost always inside the final few KB — but a
/// single turn can carry a very large tool result, hence the second, wider pass.
pub(in crate::usage) const TAIL_FIRST: u64 = 256 * 1024;
pub(in crate::usage) const TAIL_WIDE: u64 = 2 * 1024 * 1024;

/// The newest rollout files under `~/.codex/sessions`, newest first.
///
/// The tree is `YYYY/MM/DD/rollout-*.jsonl` and holds hundreds of files totalling
/// gigabytes, so this walks down the newest-named directory at each level rather
/// than listing the tree. Names sort lexicographically *and* chronologically
/// there, which is the whole reason the layout is date-nested.
pub(in crate::usage) fn recent_rollouts(sessions: &Path, want: usize) -> Vec<PathBuf> {
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
pub(in crate::usage) fn newest_child(dir: &Path, dirs: bool) -> Option<PathBuf> {
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
pub(in crate::usage) fn last_rate_limit_line(path: &Path) -> Option<String> {
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

pub(in crate::usage) fn read_tail(path: &Path, bytes: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(bytes))).ok()?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// Turn one rollout line into a [`Report`].
pub(in crate::usage) fn parse_codex_rate_limits(line: &str) -> Result<Report> {
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
pub(in crate::usage) struct CodexIdentity {
    pub(in crate::usage) email: Option<String>,
    pub(in crate::usage) renews_at: Option<u64>,
}

pub(in crate::usage) fn codex_identity() -> Option<CodexIdentity> {
    let text = fs::read_to_string(home().ok()?.join(".codex/auth.json")).ok()?;
    identity_from_codex_auth(&text)
}

/// The claim OpenAI namespaces its subscription facts under. A JWT claim name
/// is an opaque string, and this one happens to look like a URL; nothing is
/// fetched from it.
pub(in crate::usage) const CODEX_AUTH_CLAIM: &str = "https://api.openai.com/auth";

pub(in crate::usage) fn identity_from_codex_auth(text: &str) -> Option<CodexIdentity> {
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
pub(in crate::usage) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&base64url_decode(payload)?).ok()
}

/// base64url without padding — the one encoding a JWT uses. Hand-rolled because
/// this is the only base64 in the plugin and a crate for sixty-four characters
/// would be a dependency nobody could justify.
pub(in crate::usage) fn base64url_decode(input: &str) -> Option<Vec<u8>> {
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
pub(in crate::usage) fn codex_facts(payload: &serde_json::Value) -> Vec<Fact> {
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

pub(in crate::usage) fn codex_window(value: &serde_json::Value) -> Option<Window> {
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
pub(in crate::usage) fn compact_count(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// `part` as a percentage of `whole`, or `None` when the ratio would be a lie
/// (an empty or smaller-than-part denominator).
pub(in crate::usage) fn percent_of(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| (part as f64 / whole as f64 * 100.0).clamp(0.0, 100.0))
}

/// A window length as the card says it: `weekly`, `5h`, `45m`.
pub(in crate::usage) fn window_label(minutes: u64) -> String {
    match minutes {
        0 => "window".into(),
        10080 => "weekly".into(),
        1440 => "daily".into(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}
