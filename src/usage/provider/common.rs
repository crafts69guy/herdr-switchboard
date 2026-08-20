//! Small primitives shared by quota-provider adapters.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// Provider percentages use a measured 0..=100 scale. Clamp malformed values
/// without guessing that small percentages are fractions.
pub(in crate::usage) fn clamp_percent(raw: f64) -> f64 {
    if raw.is_finite() {
        raw.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Parse the RFC 3339 subset emitted by both provider sources.
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

    // Howard Hinnant's days-from-civil conversion.
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
