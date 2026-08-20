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
//! visible to every process the user owns (see `provider::Claude::fetch`).
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

mod domain;
mod provider;
mod time;
mod view;

use std::sync::mpsc::{self, Receiver};
use std::thread;

#[cfg(test)]
use std::fs;

use anyhow::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::style::Color;
use ratatui::Frame;

use crate::config::Config;
use crate::data::Theme;
use crate::runner::SystemRunner;
use crate::state::now;
use crate::surface::{Surface, Transition};
use crate::tui;
use domain::{Fact, Report, Severity, Slot, Window};
use provider::{providers, Provider};
use time::{format_clock_dated, format_renewal, format_reset_dated, freshness, local_offset};
use view::draw;

#[cfg(test)]
use provider::*;
#[cfg(test)]
use time::{format_clock, format_date, format_reset, parse_utc_offset};
#[cfg(test)]
use view::{card_gap, donut_colors, window_color, CARD_GAP};

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

impl Surface for App {
    type Output = ();

    fn draw(&mut self, f: &mut Frame) {
        draw(f, self);
    }

    fn on_event(&mut self, event: Event) -> Result<Transition<Self::Output>> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => Ok(self.on_key(key)),
            Event::Mouse(mouse) => Ok(self.on_mouse(mouse)),
            _ => Ok(Transition::Wait),
        }
    }

    fn on_tick(&mut self) -> Result<Transition<Self::Output>> {
        Ok(if self.drain() {
            Transition::Redraw
        } else {
            Transition::Wait
        })
    }
}

impl App {
    fn on_key(&mut self, k: KeyEvent) -> Transition<()> {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => Transition::Exit(()),
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                Transition::Exit(())
            }
            KeyCode::Char('r') => {
                self.refresh();
                Transition::Redraw
            }
            _ => Transition::Wait,
        }
    }

    /// Pills only. There is nothing here to scroll: `draw` lays every card to
    /// fit the pane by construction, so a wheel would have nowhere to move to.
    fn on_mouse(&mut self, m: MouseEvent) -> Transition<()> {
        if let MouseEventKind::Down(MouseButton::Left) = m.kind {
            if let Some(code) =
                tui::zone_at(&self.bar_zones, self.bar_row, (m.column, m.row).into())
            {
                return self.on_key(KeyEvent::from(code));
            }
        }
        Transition::Wait
    }

    /// Collect whatever the worker has finished. Non-blocking: the draw loop
    /// wakes every 200ms anyway, so a card fills itself in within a frame of the
    /// request landing.
    fn drain(&mut self) -> bool {
        let Some(inbox) = &self.inbox else {
            return false;
        };
        let mut done = false;
        let mut changed = false;
        loop {
            match inbox.try_recv() {
                Ok((index, result)) => {
                    changed = true;
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
        changed || done
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
    crate::surface::run(&mut app)
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
