//! The views: attention (the default), `-c`, `-r` and `-a`.

use std::process::ExitCode;

use anstyle::{AnsiColor, Style};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::args::Cli;
use super::{FAILURE, render, settings, ui};
use crate::attention::Staleness;
use crate::config::week_days;
use crate::duration::parse_duration;
use crate::gh::{Runner, count_prs, fetch_prs};
use crate::hide::{hide_path, load_hidden, split_hidden};
use crate::mute::split_muted;
use crate::pr::{PullRequest, Role};
use crate::snooze::{load_snoozes, save_snoozes, snooze_path, split_snoozed};

/// The searches behind the attention view.
pub const ATTENTION: [Role; 3] = [Role::Author, Role::ReviewRequested, Role::ReviewedBy];

/// Every qualifier, in the order the searches have always run.
const ALL: [Role; 5] = [
    Role::Author,
    Role::ReviewRequested,
    Role::ReviewedBy,
    Role::Assignee,
    Role::Involves,
];

pub fn run(cli: &Cli, runner: &dyn Runner) -> ExitCode {
    let (qualifiers, title, style): (&[Role], &str, Style) = if cli.created {
        (
            &[Role::Author],
            "PRs you created",
            ui::bold(AnsiColor::Blue),
        )
    } else if cli.review {
        (
            &[Role::ReviewRequested],
            "PRs awaiting your review",
            ui::bold(AnsiColor::Cyan),
        )
    } else if cli.all {
        (&ALL, "All PRs you are involved with", ui::BOLD)
    } else {
        (&ATTENTION, "", Style::new())
    };
    let attention_view = !(cli.created || cli.review || cli.all);

    // A single-qualifier count needs no node data and no de-duplication:
    // a count-only query answers it in a fraction of the time, exactly.
    if cli.count && qualifiers.len() == 1 {
        return match ui::spinning("Fetching pull requests…", |_| {
            count_prs(runner, qualifiers[0])
        }) {
            Ok(count) => {
                println!("{count}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                ui::error(&error.to_string());
                ExitCode::from(FAILURE)
            }
        };
    }

    // The threshold: --stale-after, else config.json, else the default. A
    // bad flag is a hard error (explicit input), a bad config only warns.
    // The weekend policy stays the config's: it decides what the flag's 'w'
    // means.
    let config = settings();
    let mut stale_after = config.stale_after;
    if let Some(text) = &cli.stale_after {
        match parse_duration(text, week_days(config.skip_weekends)) {
            Ok(duration) => stale_after = Some(duration),
            Err(error) => {
                ui::error(&error.to_string());
                return ExitCode::from(FAILURE);
            }
        }
    }
    let staleness = stale_after.map(|after| Staleness {
        now: Timestamp::now(),
        after,
        skip_weekends: config.skip_weekends,
        tz: TimeZone::system(),
    });

    let fetched = ui::spinning("Fetching pull requests…", |warn| {
        fetch_prs(runner, qualifiers, staleness.as_ref(), &mut |w| warn(w))
    });
    let mut prs = match fetched {
        Ok(prs) => prs,
        Err(error) => {
            ui::error(&error.to_string());
            return ExitCode::from(FAILURE);
        }
    };

    // Only the attention view (table and --count) honours mute rules, hides
    // and snoozes; the explicit views and --json stay exact.
    if attention_view && !cli.json {
        prs = withhold(prs, &config.mute);
    }

    if cli.count {
        let count = if attention_view {
            prs.iter().filter(|pr| pr.needs_attention()).count()
        } else {
            prs.len()
        };
        println!("{count}");
    } else if cli.json {
        println!("{}", render::json(&prs));
    } else if attention_view {
        render::attention(&prs, ui::width());
    } else {
        render::list(&prs, title, style, ui::width());
    }
    ExitCode::SUCCESS
}

/// Apply mute rules, then hides, then snoozes, saying how many PRs each
/// withheld that would have needed attention: hiding is never silent, even
/// for --count. The order matters: a lingering snooze on a muted or hidden
/// PR must neither count as "snoozed hidden" nor get pruned.
fn withhold(prs: Vec<PullRequest>, mute: &[crate::mute::MuteRule]) -> Vec<PullRequest> {
    let attention_count =
        |prs: &[PullRequest]| prs.iter().filter(|pr| pr.needs_attention()).count();

    let (prs, muted) = split_muted(prs, mute);
    let muted = attention_count(&muted);
    if muted > 0 {
        ui::err(&ui::paint(
            &format!("{muted} PR(s) muted by config — 'gh prs -r' still lists them"),
            ui::DIM,
        ));
    }

    // An unreadable store shows more, never less.
    let hidden = load_hidden(&hide_path()).unwrap_or_else(|error| {
        ui::warn(&format!("ignoring hidden PRs: {error}"));
        Default::default()
    });
    let (prs, withheld) = split_hidden(prs, &hidden);
    let withheld = attention_count(&withheld);
    if withheld > 0 {
        ui::err(&ui::paint(
            &format!("{withheld} hidden PR(s) withheld — 'gh prs hide' to list"),
            ui::DIM,
        ));
    }

    let path = snooze_path();
    let mut snoozes = load_snoozes(&path).unwrap_or_else(|error| {
        ui::warn(&format!("ignoring snoozes: {error}"));
        Default::default()
    });
    if snoozes.is_empty() {
        return prs;
    }
    let fetched: Vec<String> = prs.iter().map(|pr| pr.url.clone()).collect();
    let split = split_snoozed(prs, &snoozes, Timestamp::now());
    for (url, why) in &split.dead {
        snoozes.remove(url);
        // Entries for absent PRs (closed, or beyond the search cap) go
        // quietly: nothing resurfaced.
        if fetched.contains(url) {
            ui::warn(&format!("snooze expired for {url} ({why})"));
        }
    }
    if !split.dead.is_empty()
        && let Err(error) = save_snoozes(&snoozes, &path)
    {
        ui::warn(&format!("could not prune expired snoozes: {error}"));
    }
    let snoozed = attention_count(&split.hidden);
    if snoozed > 0 {
        ui::err(&ui::paint(
            &format!("{snoozed} snoozed PR(s) hidden — 'gh prs snooze' to list"),
            ui::DIM,
        ));
    }
    split.visible
}
