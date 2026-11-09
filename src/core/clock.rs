//! Timestamps are UTC. They are stored as unix seconds and shown as RFC 3339.

use time::format_description::well_known::Rfc3339;
use time::{Duration, Time};

pub use time::OffsetDateTime as Timestamp;

/// The current time, to the second: the precision that is stored.
pub fn now() -> Timestamp {
    let now = Timestamp::now_utc();
    now.replace_nanosecond(0).unwrap_or(now)
}

pub fn to_rfc3339(t: Timestamp) -> String {
    t.format(&Rfc3339)
        .expect("a UTC timestamp formats as RFC 3339")
}

pub fn parse_rfc3339(s: &str) -> Result<Timestamp, time::error::Parse> {
    Timestamp::parse(s, &Rfc3339).map(|t| t.to_offset(time::UtcOffset::UTC))
}

pub fn from_unix(seconds: i64) -> Option<Timestamp> {
    Timestamp::from_unix_timestamp(seconds).ok()
}

/// Midnight UTC of `t`'s day: where daily limits reset.
pub fn start_of_day(t: Timestamp) -> Timestamp {
    t.to_offset(time::UtcOffset::UTC)
        .replace_time(Time::MIDNIGHT)
}

/// Midnight UTC on the first of `t`'s month: where monthly budgets reset.
pub fn start_of_month(t: Timestamp) -> Timestamp {
    let day = start_of_day(t);
    day - Duration::days(i64::from(day.day()) - 1)
}

/// A span like `30d` or `12h`, as accepted by `admin grant --expires-in`.
pub fn parse_span(s: &str) -> Option<Duration> {
    let (number, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = number.parse().ok().filter(|n| *n > 0)?;
    match unit {
        "d" => Duration::days(n).into(),
        "h" => Duration::hours(n).into(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_and_month_boundaries() {
        let t = parse_rfc3339("2026-11-09T15:04:05Z").unwrap();
        assert_eq!(to_rfc3339(start_of_day(t)), "2026-11-09T00:00:00Z");
        assert_eq!(to_rfc3339(start_of_month(t)), "2026-11-01T00:00:00Z");
    }

    #[test]
    fn offsets_normalize_to_utc() {
        let t = parse_rfc3339("2026-11-01T01:00:00+02:00").unwrap();
        assert_eq!(to_rfc3339(t), "2026-10-31T23:00:00Z");
        assert_eq!(to_rfc3339(start_of_month(t)), "2026-10-01T00:00:00Z");
    }

    #[test]
    fn spans() {
        assert_eq!(parse_span("30d"), Some(Duration::days(30)));
        assert_eq!(parse_span("12h"), Some(Duration::hours(12)));
        for bad in ["", "d", "0d", "-1d", "30", "30m"] {
            assert_eq!(parse_span(bad), None, "{bad}");
        }
    }
}
