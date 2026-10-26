//! Durations the way a person means them: exact hours, or whole days.
//!
//! "Snooze it for a day" at noon means "show it to me tomorrow morning", not
//! "at noon tomorrow"; "stale after 3 days" for a PR pushed on Monday means
//! "from Thursday", whatever the hour. So a day-based duration counts days on
//! the local calendar and lands at the local midnight that opens the target
//! day — before anyone starts work, so the PR is waiting on the first look of
//! the morning. An hour-based duration stays an exact span: someone who types
//! `4h` wants four hours.
//!
//! Pure calendar arithmetic, no I/O: the local time zone is a parameter.

use std::fmt;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, ToSpan};

/// Days a `w` stands for by default: a calendar week.
pub const CALENDAR_WEEK_DAYS: u32 = 7;

/// The longest duration accepted, about a century: far beyond any real use,
/// and far enough from the calendar's limits that turning a duration into a
/// timestamp can never overflow.
const MAX_DAYS: u32 = 36_500;

/// `Weekday::to_monday_zero_offset` of Friday, and the length of a working
/// week, for skipping Saturdays and Sundays.
const FRIDAY: i64 = 4;
const WORKING_DAYS: i64 = 5;

/// The unit a [`Duration`] counts in. Weeks are folded into days when
/// parsed, since how many days a week holds depends on the weekend policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Hours,
    Days,
}

/// A positive whole number of hours or days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Duration {
    pub amount: u32,
    pub unit: Unit,
}

/// A duration string that isn't a positive number of hours, days or weeks.
#[derive(Debug, PartialEq, Eq)]
pub struct DurationError(String);

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DurationError {}

impl Duration {
    #[must_use]
    pub const fn hours(amount: u32) -> Self {
        Self {
            amount,
            unit: Unit::Hours,
        }
    }

    #[must_use]
    pub const fn days(amount: u32) -> Self {
        Self {
            amount,
            unit: Unit::Days,
        }
    }

    /// When this duration, counted from `start`, runs out.
    ///
    /// Hours are an exact span, weekends included. Days land at the local
    /// midnight opening the `amount`-th day after `start`'s local date — or
    /// the `amount`-th working day under `skip_weekends`, so a day counted
    /// from Friday or the weekend lands on Monday. The error is the
    /// calendar's range, which `parse_duration`'s cap keeps out of reach.
    pub fn until(
        self,
        start: Timestamp,
        tz: &TimeZone,
        skip_weekends: bool,
    ) -> Result<Timestamp, jiff::Error> {
        match self.unit {
            Unit::Hours => start.checked_add(SignedDuration::from_hours(self.amount.into())),
            Unit::Days => {
                let day = start.to_zoned(tz.clone()).date();
                let target = add_days(day, self.amount, skip_weekends)?;
                // Midnight in the target day's own offset, so a DST change in
                // between still lands on the local midnight.
                Ok(target.to_zoned(tz.clone())?.timestamp())
            }
        }
    }
}

/// The `days`-th day after `start`, or the `days`-th working day.
///
/// Working days skip Saturdays and Sundays; counting from a weekend starts
/// as if from the Friday before, so the first working day after Saturday is
/// Monday, the same as after Friday. Constant time, however far ahead.
pub fn add_days(start: Date, days: u32, skip_weekends: bool) -> Result<Date, jiff::Error> {
    let days = i64::from(days);
    if !skip_weekends {
        return start.checked_add(days.days());
    }
    let mut start = start;
    let mut weekday = i64::from(start.weekday().to_monday_zero_offset());
    if weekday > FRIDAY {
        start = start.checked_sub((weekday - FRIDAY).days())?;
        weekday = FRIDAY;
    }
    let (weeks, rest) = (days / WORKING_DAYS, days % WORKING_DAYS);
    // The remainder crosses a weekend when it runs past Friday.
    let skipped = if weekday + rest > FRIDAY { 2 } else { 0 };
    start.checked_add((7 * weeks + rest + skipped).days())
}

/// Parse a duration like `12h`, `3d` or `1w`.
///
/// Hours stay hours; days and weeks become whole days, which callers count
/// on the calendar. `week_days` is how many days a `w` stands for: seven by
/// default, five for callers counting working days, so `1w` stays a
/// same-weekday anniversary instead of stretching to seven working days.
///
/// Zero is refused (a snooze that never hides anything is a typo, not a
/// request), and so is anything longer than about a century.
pub fn parse_duration(text: &str, week_days: u32) -> Result<Duration, DurationError> {
    let error = || {
        DurationError(format!(
            "invalid duration '{text}' (use a positive number of hours, days, or weeks: e.g. 12h, 3d, 1w)"
        ))
    };
    let normalized = text.trim().to_lowercase();
    let Some(unit) = normalized.chars().last() else {
        return Err(error());
    };
    let digits = normalized[..normalized.len() - unit.len_utf8()].trim_end();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error());
    }
    // Too many digits for a u32 is far past the cap anyway.
    let amount: u32 = digits.parse().map_err(|_| error())?;
    let (amount, unit) = match unit {
        'h' => (Some(amount), Unit::Hours),
        'd' => (Some(amount), Unit::Days),
        'w' => (amount.checked_mul(week_days), Unit::Days),
        _ => return Err(error()),
    };
    let limit = match unit {
        Unit::Hours => MAX_DAYS * 24,
        Unit::Days => MAX_DAYS,
    };
    match amount {
        Some(amount) if (1..=limit).contains(&amount) => Ok(Duration { amount, unit }),
        Some(_) | None => Err(error()),
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    /// An instant given as Paris wall-clock time.
    fn at_paris(y: i16, m: i8, d: i8, h: i8) -> Timestamp {
        date(y, m, d)
            .at(h, 0, 0, 0)
            .to_zoned(paris())
            .unwrap()
            .timestamp()
    }

    #[test]
    fn calendar_days_include_weekends() {
        assert_eq!(
            add_days(date(2026, 7, 24), 1, false).unwrap(),
            date(2026, 7, 25)
        );
    }

    #[test]
    fn working_days_skip_weekends() {
        let cases = [
            (date(2026, 7, 20), 1, date(2026, 7, 21)), // Monday → Tuesday
            (date(2026, 7, 20), 4, date(2026, 7, 24)), // Monday → Friday
            (date(2026, 7, 23), 3, date(2026, 7, 28)), // Thursday → Tuesday
            (date(2026, 7, 24), 1, date(2026, 7, 27)), // Friday → Monday
            (date(2026, 7, 25), 1, date(2026, 7, 27)), // Saturday → Monday
            (date(2026, 7, 26), 2, date(2026, 7, 28)), // Sunday → Tuesday
            (date(2026, 7, 24), 5, date(2026, 7, 31)), // a working week
            (date(2026, 7, 6), 15, date(2026, 7, 27)), // three working weeks
        ];
        for (start, days, expected) in cases {
            assert_eq!(
                add_days(start, days, true).unwrap(),
                expected,
                "{start} + {days}"
            );
        }
    }

    #[test]
    fn working_days_match_a_day_by_day_walk() {
        // The closed form must agree with the obvious loop from every weekday.
        for offset in 0..7 {
            let start = date(2026, 7, 20).checked_add(offset.days()).unwrap();
            for days in 1..30 {
                let (mut walked, mut left) = (start, days);
                while left > 0 {
                    walked = walked.tomorrow().unwrap();
                    if walked.weekday().to_monday_zero_offset() < 5 {
                        left -= 1;
                    }
                }
                assert_eq!(add_days(start, days, true).unwrap(), walked);
            }
        }
    }

    #[test]
    fn a_day_at_noon_ends_at_the_next_midnight() {
        let until = Duration::days(1).until(at_paris(2026, 7, 22, 12), &paris(), false);
        assert_eq!(until.unwrap(), at_paris(2026, 7, 23, 0));
    }

    #[test]
    fn a_day_early_in_the_morning_also_ends_at_the_next_midnight() {
        // Seventeen hours, not twenty-four: the day is over at midnight.
        let until = Duration::days(1).until(at_paris(2026, 7, 22, 7), &paris(), false);
        assert_eq!(until.unwrap(), at_paris(2026, 7, 23, 0));
    }

    #[test]
    fn days_count_from_the_local_date() {
        // 23:30 UTC on Wednesday is already Thursday in Paris.
        let start: Timestamp = "2026-07-22T23:30:00+00:00".parse().unwrap();
        let until = Duration::days(1).until(start, &paris(), false);
        assert_eq!(until.unwrap(), at_paris(2026, 7, 24, 0));
    }

    #[test]
    fn friday_plus_one_working_day_is_monday() {
        let start = at_paris(2026, 7, 24, 12);
        let calendar = Duration::days(1).until(start, &paris(), false);
        let working = Duration::days(1).until(start, &paris(), true);
        assert_eq!(calendar.unwrap(), at_paris(2026, 7, 25, 0));
        assert_eq!(working.unwrap(), at_paris(2026, 7, 27, 0));
    }

    #[test]
    fn dst_change_keeps_the_local_midnight() {
        // Paris falls back on Sunday 2026-10-25: Monday's midnight is UTC+1.
        let until = Duration::days(3).until(at_paris(2026, 10, 23, 12), &paris(), false);
        let until = until.unwrap();
        assert_eq!(until, at_paris(2026, 10, 26, 0));
        assert_eq!(until.to_string(), "2026-10-25T23:00:00Z");
    }

    #[test]
    fn hours_are_an_exact_span_even_over_a_weekend() {
        let start = at_paris(2026, 7, 24, 12);
        let expected = start.checked_add(SignedDuration::from_hours(12)).unwrap();
        for skip_weekends in [false, true] {
            let until = Duration::hours(12).until(start, &paris(), skip_weekends);
            assert_eq!(until.unwrap(), expected);
        }
    }

    #[test]
    fn valid_durations() {
        assert_eq!(parse_duration("12h", 7), Ok(Duration::hours(12)));
        // Hours stay hours, never folded into a day.
        assert_eq!(parse_duration("24h", 7), Ok(Duration::hours(24)));
        assert_eq!(parse_duration("3d", 7), Ok(Duration::days(3)));
        assert_eq!(parse_duration("1w", 7), Ok(Duration::days(7)));
        // Case- and whitespace-insensitive.
        assert_eq!(parse_duration(" 2D ", 7), Ok(Duration::days(2)));
    }

    #[test]
    fn invalid_durations_rejected() {
        for bad in ["", "h", "12", "12m", "1.5d", "-1d", "0h", "forever"] {
            assert!(parse_duration(bad, 7).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn week_days_resizes_only_the_week_unit() {
        assert_eq!(parse_duration("1w", 5), Ok(Duration::days(5)));
        assert_eq!(parse_duration("2w", 5), Ok(Duration::days(10)));
        assert_eq!(parse_duration("3d", 5), Ok(Duration::days(3)));
        assert_eq!(parse_duration("12h", 5), Ok(Duration::hours(12)));
    }

    #[test]
    fn a_huge_working_week_is_a_clean_error() {
        assert!(parse_duration(&("9".repeat(100) + "w"), 5).is_err());
        // Fits a u32 but overflows once multiplied by the week length.
        assert!(parse_duration("1000000000w", 5).is_err());
    }

    #[test]
    fn overflowing_duration_is_a_clean_error() {
        assert!(parse_duration(&("9".repeat(100) + "d"), 7).is_err());
        assert!(parse_duration(&("9".repeat(5000) + "d"), 7).is_err());
    }

    #[test]
    fn durations_are_capped_at_a_century() {
        assert_eq!(parse_duration("36500d", 7), Ok(Duration::days(36_500)));
        assert!(parse_duration("36501d", 7).is_err());
        assert_eq!(parse_duration("876000h", 7), Ok(Duration::hours(876_000)));
        assert!(parse_duration("876001h", 7).is_err());
    }

    #[test]
    fn error_names_the_input_and_the_expected_form() {
        assert_eq!(
            parse_duration("12m", 7).unwrap_err().to_string(),
            "invalid duration '12m' (use a positive number of hours, days, or weeks: e.g. 12h, 3d, 1w)"
        );
    }
}
