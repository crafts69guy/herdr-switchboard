//! Time-zone, reset, renewal, and freshness formatting for Usage reports.

use crate::runner::CommandRunner;

pub(super) fn format_reset(now: u64, resets_at: u64) -> String {
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
pub(super) fn format_reset_dated(now: u64, resets_at: u64, offset: i64) -> String {
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
pub(super) fn local_offset(runner: &dyn CommandRunner) -> i64 {
    let Some(raw) = runner.capture("date", &["+%z"]) else {
        return 0;
    };
    parse_utc_offset(&raw).unwrap_or(0)
}

/// `+0700` / `-0430` as seconds east of UTC.
pub(super) fn parse_utc_offset(raw: &str) -> Option<i64> {
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
pub(super) fn format_clock(epoch: u64, offset: i64) -> String {
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
pub(super) fn format_clock_dated(now: u64, epoch: u64, offset: i64) -> String {
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
pub(super) fn format_date(epoch: u64, offset: i64) -> String {
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
pub(super) fn local_day(epoch: u64, offset: i64) -> i64 {
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
pub(super) fn format_renewal(now: u64, renews_at: Option<u64>, offset: i64) -> String {
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
pub(super) fn freshness(now: u64, measured_at: Option<u64>) -> String {
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
