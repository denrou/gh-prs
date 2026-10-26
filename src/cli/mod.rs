//! The command line: parse, dispatch, print.
//!
//! Exit codes: 0 success, 1 a failure (or a partial batch), 2 a usage error.

mod args;
mod hide;
mod merge;
mod render;
mod snooze;
mod ui;
mod view;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use jiff::Timestamp;
use jiff::tz::TimeZone;

use self::args::{Cli, Command};
use crate::config::{Config, config_path, load_config};
use crate::gh::{Runner, fetch_pr, resolve_pr};
use crate::pr::{GraphqlEnum, Oid, PrState, Reported};
use crate::snooze::normalize_pr_url;

const FAILURE: u8 = 1;
const USAGE: u8 = 2;

/// Run the CLI on `args` (the program name first), talking to GitHub
/// through `runner`.
pub fn run(args: impl IntoIterator<Item = OsString>, runner: &dyn Runner) -> ExitCode {
    let cli = Cli::try_parse_from(args).unwrap_or_else(|error| error.exit());
    if cli.no_color {
        anstream::ColorChoice::Never.write_global();
    }
    let Some(command) = &cli.command else {
        return view::run(&cli, runner);
    };
    // The view flags parse fine before a subcommand but don't apply to it.
    if cli.created || cli.review || cli.all || cli.count || cli.json || cli.stale_after.is_some() {
        ui::error(&format!(
            "-c/-r/-a, --count, --json and --stale-after do not apply to 'gh prs {}'",
            command.name()
        ));
        return ExitCode::from(USAGE);
    }
    match command {
        Command::Snooze(args) => {
            // --for without refs is a forgotten ref, not a listing request;
            // --prune with refs is a forgotten intent.
            if args.snooze_for.is_some() && args.refs.is_empty() {
                ui::error("--for requires at least one PR to snooze");
                return ExitCode::from(USAGE);
            }
            if args.prune && !args.refs.is_empty() {
                ui::error("--prune takes no PR arguments");
                return ExitCode::from(USAGE);
            }
            snooze::snooze(args, runner)
        }
        Command::Unsnooze(args) => snooze::unsnooze(args, runner),
        Command::Hide(args) => {
            if args.prune && !args.refs.is_empty() {
                ui::error("--prune takes no PR arguments");
                return ExitCode::from(USAGE);
            }
            hide::hide(args, runner)
        }
        Command::Unhide(args) => hide::unhide(args, runner),
        Command::Merge(args) => merge::run(args, runner),
    }
}

/// The settings from config.json, or the defaults (with a warning) when the
/// file is broken: a typo there must never break the tool.
fn settings() -> Config {
    load_config(&config_path()).unwrap_or_else(|error| {
        ui::warn(&format!("ignoring config: {error}"));
        Config::default()
    })
}

/// 1 when anything failed, as a partial batch must not look like success.
fn exit_code(failures: &[String]) -> ExitCode {
    for failure in failures {
        ui::error(failure);
    }
    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(FAILURE)
    }
}

fn is_bare_number(reference: &str) -> bool {
    !reference.is_empty() && reference.bytes().all(|b| b.is_ascii_digit())
}

/// The canonical URL of a PR reference: a bare number is resolved through
/// gh (against `repo`, or the current directory's repository), a full URL
/// is canonicalized offline.
fn ref_to_url(runner: &dyn Runner, reference: &str, repo: Option<&str>) -> Result<String, String> {
    let reference = reference.trim();
    if is_bare_number(reference) {
        resolve_pr(runner, reference, repo)
            .map(|(url, _)| url)
            .map_err(|e| e.to_string())
    } else {
        normalize_pr_url(reference).map_err(|e| e.to_string())
    }
}

/// The canonical URL and head commit of a PR reference: one gh call for a
/// bare number, an offline canonicalization then a head lookup for a URL.
fn ref_to_url_and_head(
    runner: &dyn Runner,
    reference: &str,
    repo: Option<&str>,
) -> Result<(String, Oid), String> {
    let reference = reference.trim();
    if is_bare_number(reference) {
        resolve_pr(runner, reference, repo).map_err(|e| e.to_string())
    } else {
        let url = normalize_pr_url(reference).map_err(|e| e.to_string())?;
        let head = crate::gh::fetch_pr_head(runner, &url).map_err(|e| e.to_string())?;
        Ok((url, head))
    }
}

/// A stored timestamp in the local time zone, to the minute.
fn local(timestamp: &str) -> Option<String> {
    let timestamp: Timestamp = timestamp.parse().ok()?;
    Some(
        timestamp
            .to_zoned(TimeZone::system())
            .strftime("%Y-%m-%d %H:%M")
            .to_string(),
    )
}

/// Drop the entries whose PR is no longer open (`hide --prune`,
/// `snooze --prune`).
///
/// Neither a hide nor an open-ended snooze expires, so entries for closed
/// and merged PRs would accumulate; they never show (the searches only
/// return open PRs), so the clean-up is this explicit, network-bound
/// command. An entry is dropped only on positive evidence that its PR is
/// gone; one that can't be inspected is kept and reported.
fn prune_closed<E>(
    runner: &dyn Runner,
    entries: &mut BTreeMap<String, E>,
    save: impl FnOnce(&BTreeMap<String, E>) -> Result<(), String>,
) -> ExitCode {
    let mut pruned = Vec::new();
    let mut failures = Vec::new();
    let urls: Vec<String> = entries.keys().cloned().collect();
    for url in urls {
        let lookup = ui::spinning(&format!("Checking {url}…"), |warn| {
            fetch_pr(runner, &url, &mut |w| warn(w))
        });
        let state = match lookup {
            Ok(pr) => pr.state,
            Err(error) => {
                failures.push(format!("{url}: {error}"));
                continue;
            }
        };
        let gone = match &state {
            Reported::Known(PrState::Open) | Reported::Missing => None,
            Reported::Known(state @ (PrState::Closed | PrState::Merged)) => {
                Some(state.graphql_name().to_lowercase())
            }
            Reported::Unrecognised(state) => Some(state.to_lowercase()),
        };
        if let Some(state) = gone {
            entries.remove(&url);
            pruned.push(format!(
                "{url} {}",
                ui::paint(&format!("({state})"), ui::DIM)
            ));
        }
    }
    if pruned.is_empty() {
        ui::out(&ui::paint("Nothing to prune.", ui::DIM));
    } else {
        if let Err(error) = save(entries) {
            ui::error(&error);
            return ExitCode::from(FAILURE);
        }
        for line in pruned {
            ui::out(&format!("Pruned {line}"));
        }
    }
    exit_code(&failures)
}
