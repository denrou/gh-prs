//! `gh prs snooze` and `gh prs unsnooze`.
//!
//! A corrupt store is fatal here (a write would clobber it, and the listing
//! must not show a half-parsed store), unlike in the view where it degrades
//! to "nothing snoozed".

use std::collections::HashMap;
use std::process::ExitCode;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::args::{RefArgs, SnoozeArgs};
use super::view::ATTENTION;
use super::{
    FAILURE, exit_code, local, prune_closed, ref_to_url, ref_to_url_and_head, settings, ui,
};
use crate::attention::Staleness;
use crate::config::{Config, week_days};
use crate::duration::{Unit, parse_duration};
use crate::gh::{Runner, fetch_prs};
use crate::pr::Oid;
use crate::snooze::{Snooze, Snoozes, load_snoozes, save_snoozes, snooze_path};
use crate::store::format_timestamp;

fn load() -> Result<Snoozes, ExitCode> {
    load_snoozes(&snooze_path()).map_err(|error| {
        ui::error(&error.to_string());
        ExitCode::from(FAILURE)
    })
}

fn save(snoozes: &Snoozes) -> Result<(), String> {
    save_snoozes(snoozes, &snooze_path()).map_err(|e| e.to_string())
}

pub fn snooze(args: &SnoozeArgs, runner: &dyn Runner) -> ExitCode {
    let mut snoozes = match load() {
        Ok(snoozes) => snoozes,
        Err(code) => return code,
    };
    if !args.refs.is_empty() {
        match add(args, runner, &mut snoozes) {
            Ok(code) | Err(code) => code,
        }
    } else if args.prune {
        prune_closed(runner, &mut snoozes, save)
    } else {
        list(&snoozes);
        ExitCode::SUCCESS
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    ui::error(&message.to_string());
    ExitCode::from(FAILURE)
}

/// Snooze every PR in `args.refs`.
///
/// Without --for the snooze is open-ended: lifted only when the head moves
/// or the attention reasons change, so it needs those reasons, which means
/// the PR must be in the attention view. With --for the window is a third
/// way out and the reasons are best-effort. Refs resolve independently: a
/// bad one is reported and skipped (a partial batch exits 1), and the store
/// is written once.
fn add(
    args: &SnoozeArgs,
    runner: &dyn Runner,
    snoozes: &mut Snoozes,
) -> Result<ExitCode, ExitCode> {
    // The duration is validated before any network round trip. The config's
    // weekend policy decides how long a 'w' is and which days count.
    let config = settings();
    let now = Timestamp::now();
    let until = match &args.snooze_for {
        None => None,
        Some(text) => {
            let duration = parse_duration(text, week_days(config.skip_weekends)).map_err(fail)?;
            let until = duration
                .until(now, &TimeZone::system(), config.skip_weekends)
                .map_err(fail)?;
            // Hours are stored in UTC and days at the local midnight, the way
            // the stores have always been written.
            let tz = match duration.unit {
                Unit::Hours => TimeZone::UTC,
                Unit::Days => TimeZone::system(),
            };
            Some(format_timestamp(until, &tz))
        }
    };

    let mut resolved: Vec<(String, Oid)> = Vec::new();
    let mut failures = Vec::new();
    for reference in &args.refs {
        let lookup = ui::spinning(&format!("Looking up {reference}…"), |_| {
            ref_to_url_and_head(runner, reference, args.repo.repo.as_deref())
        });
        match lookup {
            Ok((url, head)) => match resolved.iter_mut().find(|(seen, _)| *seen == url) {
                Some(entry) => entry.1 = head,
                None => resolved.push((url, head)),
            },
            Err(error) => failures.push(format!("{reference}: {error}")),
        }
    }

    // The reasons let the snooze lapse when they change (a review lands and
    // a waiting PR becomes ready), not only when the head moves.
    let reasons = if resolved.is_empty() {
        HashMap::new()
    } else {
        attention_reasons_by_url(runner, &config, until.is_none()).map_err(fail)?
    };

    let mut snoozed = Vec::new();
    for (url, head) in resolved {
        let reasons: Option<Vec<&str>> = reasons
            .get(&url)
            .map(|r| r.iter().map(String::as_str).collect());
        let entry = match (&until, reasons) {
            (None, None) => {
                failures.push(format!(
                    "{url} is not in your attention view, so its status cannot be tracked; pass --for to snooze it for a fixed window"
                ));
                continue;
            }
            (None, Some(reasons)) => Snooze::open_ended(head.as_str(), &reasons),
            (Some(until), reasons) => Snooze::windowed(head.as_str(), until, reasons.as_deref()),
        };
        snoozes.insert(url.clone(), entry);
        snoozed.push(url);
    }
    if !snoozed.is_empty() {
        save(snoozes).map_err(fail)?;
        for url in snoozed {
            let detail = match &until {
                None => "until its head moves or its status changes".to_owned(),
                Some(until) => format!(
                    "until {}, or sooner if its head moves or its status changes",
                    local(until).unwrap_or_default()
                ),
            };
            ui::out(&format!(
                "Snoozed {url} {}",
                ui::paint(&format!("({detail})"), ui::DIM)
            ));
        }
    }
    Ok(exit_code(&failures))
}

/// Each attention-view PR's current reasons, sorted, by URL.
///
/// It must use the persisted staleness settings, as later (unflagged) views
/// will, or a captured 'stale' would differ from the rendered one and lift
/// the snooze on the next run. For a windowed snooze a failure degrades to
/// no reasons; an open-ended one has nothing else to watch, so there it is
/// an error and nothing gets snoozed.
fn attention_reasons_by_url(
    runner: &dyn Runner,
    config: &Config,
    required: bool,
) -> Result<HashMap<String, Vec<String>>, String> {
    let staleness = config.stale_after.map(|after| Staleness {
        now: Timestamp::now(),
        after,
        skip_weekends: config.skip_weekends,
        tz: TimeZone::system(),
    });
    let fetched = ui::spinning("Reading attention state…", |warn| {
        fetch_prs(runner, &ATTENTION, staleness.as_ref(), &mut |w| warn(w))
    });
    match fetched {
        Ok(prs) => Ok(prs
            .into_iter()
            .map(|pr| {
                (
                    pr.url,
                    pr.attention.iter().map(|r| r.as_str().to_owned()).collect(),
                )
            })
            .collect()),
        Err(error) if required => Err(format!(
            "could not read attention state ({error}); an open-ended snooze needs it — pass --for to snooze for a fixed window instead"
        )),
        Err(error) => {
            ui::warn(&format!(
                "could not read attention state ({error}); snoozing without it"
            ));
            Ok(HashMap::new())
        }
    }
}

fn list(snoozes: &Snoozes) {
    if snoozes.is_empty() {
        ui::out(&ui::paint("No snoozed PRs.", ui::DIM));
    }
    let now = Timestamp::now();
    for (url, snooze) in snoozes {
        let head: String = snooze.oid().chars().take(12).collect();
        let detail = match snooze {
            _ if snooze.is_expired(now) => "expired".to_owned(),
            Snooze::OpenEnded { .. } => format!("until head moving off {head} or status changing"),
            Snooze::Windowed { until, .. } => {
                format!(
                    "until {}, or head moving off {head}",
                    local(until).unwrap_or_default()
                )
            }
        };
        ui::out(&format!(
            "{url} {}",
            ui::paint(&format!("({detail})"), ui::DIM)
        ));
    }
}

pub fn unsnooze(args: &RefArgs, runner: &dyn Runner) -> ExitCode {
    let mut snoozes = match load() {
        Ok(snoozes) => snoozes,
        Err(code) => return code,
    };
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    for reference in &args.refs {
        match ref_to_url(runner, reference, args.repo.repo.as_deref()) {
            Err(error) => failures.push(format!("{reference}: {error}")),
            Ok(url) if snoozes.remove(&url).is_none() => {
                failures.push(format!("{reference} ({url}) is not snoozed"))
            }
            Ok(url) => removed.push(url),
        }
    }
    if !removed.is_empty() {
        if let Err(error) = save(&snoozes) {
            return fail(error);
        }
        for url in removed {
            ui::out(&format!("Unsnoozed {url}"));
        }
    }
    exit_code(&failures)
}
