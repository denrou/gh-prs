//! `--follow`: the view redrawn in place at an interval, until Ctrl-C.
//!
//! The frames go to the terminal's alternate screen, as with `watch` or
//! `top`: clearing the main screen pushes a copy of every frame into the
//! scrollback in some terminals. Ctrl-C leaves it, so the shell gets its
//! screen back.

use std::fmt::Write as _;
use std::io::Write;
use std::time::Duration;

use anstyle::Style;
use gh_common::table::truncate;
use gh_common::ui;

/// A refresh takes several seconds and spends API quota shared with every
/// other `gh` command, so the interval has a floor.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// The longest wait after failures in a row, unless the interval is longer.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

const ENTER: &str = "\x1b[?1049h\x1b[?25l";
const LEAVE: &str = "\x1b[?25h\x1b[?1049l";
const HOME_CLEAR_LINE: &str = "\x1b[H\x1b[2K";
const HOME_CLEAR_SCREEN: &str = "\x1b[H\x1b[2J";

/// `30s`, `2m` or `1h`, at least [`MIN_INTERVAL`] (the clap value parser
/// of `--follow`).
pub fn parse_interval(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let invalid = || {
        format!(
            "invalid interval '{text}' (use a number of seconds, minutes or hours: e.g. 30s, 2m, 1h)"
        )
    };
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (amount, unit) = text.split_at(digits);
    let amount: u64 = amount.parse().map_err(|_| invalid())?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(invalid()),
    };
    let interval = Duration::from_secs(amount.checked_mul(factor).ok_or_else(invalid)?);
    if interval < MIN_INTERVAL {
        return Err(format!(
            "the interval is at least {}: each refresh takes seconds and spends GitHub API quota",
            human(MIN_INTERVAL)
        ));
    }
    Ok(interval)
}

/// `1h30m`, `2m`, `45s`.
#[must_use]
pub fn human(duration: Duration) -> String {
    let secs = duration.as_secs();
    let mut text = String::new();
    for (amount, unit) in [(secs / 3600, 'h'), (secs / 60 % 60, 'm'), (secs % 60, 's')] {
        if amount > 0 {
            let _ = write!(text, "{amount}{unit}");
        }
    }
    if text.is_empty() {
        text.push_str("0s");
    }
    text
}

/// The wait before the next refresh: the interval, doubled after each
/// failure in a row, so an outage or a rate limit is not hammered.
#[must_use]
pub fn wait(interval: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return interval;
    }
    interval
        .saturating_mul(2_u32.saturating_pow(failures))
        .min(MAX_BACKOFF.max(interval))
}

/// The alternate screen, for as long as the value lives or until Ctrl-C.
pub struct Screen;

impl Screen {
    /// Enter the alternate screen, with Ctrl-C leaving it and exiting 130
    /// (the shell's code for an interrupt, as without `--follow`).
    pub fn enter() -> Result<Self, String> {
        ctrlc::set_handler(|| {
            // Deliberate: the lock is held until the process exits, so the
            // main thread cannot draw on the main screen once it is back.
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(LEAVE.as_bytes());
            let _ = out.flush();
            std::process::exit(130);
        })
        .map_err(|error| format!("cannot handle Ctrl-C: {error}"))?;
        raw(ENTER);
        Ok(Self)
    }

    /// Replace the top line, leaving the rest of the frame as it is.
    pub fn status(&self, text: &str, style: Style) {
        raw(HOME_CLEAR_LINE);
        anstream::print!("{}", status_line(text, style));
        let _ = anstream::stdout().flush();
    }

    /// Start a new frame: a clear screen with the status line on top. What
    /// is printed next is the frame's body.
    pub fn frame(&self, text: &str, style: Style) {
        raw(HOME_CLEAR_SCREEN);
        anstream::print!("{}\n\n", status_line(text, style));
        let _ = anstream::stdout().flush();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        raw(LEAVE);
    }
}

/// One line, cut to the screen so it never wraps onto the frame below.
fn status_line(text: &str, style: Style) -> String {
    let first = text.lines().next().unwrap_or_default();
    ui::paint(&truncate(first, ui::width()), style)
}

/// Terminal control sequences go straight to stdout: `anstream` would strip
/// them with the colours under `--no-color` or `NO_COLOR`.
fn raw(sequence: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(sequence.as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_in_seconds_minutes_or_hours() {
        assert_eq!(parse_interval("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_interval("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_interval(" 1h "), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_interval("10s"), Ok(MIN_INTERVAL));
    }

    #[test]
    fn malformed_intervals_are_rejected() {
        for text in [
            "",
            "s",
            "30",
            "30x",
            "1.5m",
            "-30s",
            "1m30s",
            "99999999999999999999h",
        ] {
            let error = parse_interval(text).unwrap_err();
            assert!(error.starts_with("invalid interval"), "{text}: {error}");
        }
        assert!(parse_interval("18446744073709551615h").is_err());
    }

    #[test]
    fn intervals_below_the_floor_are_rejected() {
        let error = parse_interval("9s").unwrap_err();
        assert!(error.starts_with("the interval is at least 10s"), "{error}");
        assert!(parse_interval("0m").is_err());
    }

    #[test]
    fn durations_read_as_typed() {
        assert_eq!(human(Duration::from_secs(45)), "45s");
        assert_eq!(human(Duration::from_secs(60)), "1m");
        assert_eq!(human(Duration::from_secs(90)), "1m30s");
        assert_eq!(human(Duration::from_secs(5400)), "1h30m");
        assert_eq!(human(Duration::ZERO), "0s");
    }

    #[test]
    fn failures_back_off_up_to_ten_minutes() {
        let minute = Duration::from_secs(60);
        assert_eq!(wait(minute, 0), minute);
        assert_eq!(wait(minute, 1), 2 * minute);
        assert_eq!(wait(minute, 3), 8 * minute);
        assert_eq!(wait(minute, 4), MAX_BACKOFF);
        assert_eq!(wait(minute, u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn a_long_interval_is_never_shortened_by_a_failure() {
        let hour = Duration::from_secs(3600);
        assert_eq!(wait(hour, 1), hour);
    }
}
