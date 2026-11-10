//! `gh prs hide` and `gh prs unhide`.
//!
//! As for snoozes, a corrupt store is fatal here: a write would clobber it,
//! and the listing must not show a half-parsed store.

use std::process::ExitCode;

use jiff::Timestamp;

use super::args::{HideArgs, RefArgs};
use super::{FAILURE, exit_code, local, prune_closed, ref_to_url, ui};
use crate::gh::Runner;
use crate::hide::{Hidden, Hide, hide_path, load_hidden, save_hidden};

fn load() -> Result<Hidden, ExitCode> {
    load_hidden(&hide_path()).map_err(|error| {
        ui::error(&error.to_string());
        ExitCode::from(FAILURE)
    })
}

fn save(hidden: &Hidden) -> Result<(), String> {
    save_hidden(hidden, &hide_path()).map_err(|e| e.to_string())
}

pub fn hide(args: &HideArgs, runner: &dyn Runner) -> ExitCode {
    let mut hidden = match load() {
        Ok(hidden) => hidden,
        Err(code) => return code,
    };
    if !args.refs.is_empty() {
        add(args, runner, &mut hidden)
    } else if args.prune {
        prune_closed(runner, &mut hidden, save)
    } else {
        list(&hidden);
        ExitCode::SUCCESS
    }
}

/// Hide every PR in `args.refs`. Hiding an already hidden PR is a no-op
/// that keeps the original `since`: the decision stands either way.
fn add(args: &HideArgs, runner: &dyn Runner, hidden: &mut Hidden) -> ExitCode {
    let mut added = Vec::new();
    let mut failures = Vec::new();
    for reference in &args.refs {
        let lookup = ui::spinning(&format!("Looking up {reference}…"), |_| {
            ref_to_url(runner, reference, args.repo.repo.as_deref())
        });
        match lookup {
            Err(error) => failures.push(format!("{reference}: {error}")),
            Ok(url) if hidden.contains_key(&url) => {
                ui::out(&format!("{url} {}", ui::paint("(already hidden)", ui::DIM)));
            }
            Ok(url) => {
                hidden.insert(url.clone(), Hide::new(Timestamp::now()));
                added.push(url);
            }
        }
    }
    if !added.is_empty() {
        if let Err(error) = save(hidden) {
            ui::error(&error);
            return ExitCode::from(FAILURE);
        }
        for url in added {
            let detail = ui::paint(
                "(until 'gh prs unhide', whatever happens to the PR)",
                ui::DIM,
            );
            ui::out(&format!("Hidden {url} {detail}"));
        }
    }
    exit_code(&failures)
}

fn list(hidden: &Hidden) {
    if hidden.is_empty() {
        ui::out(&ui::paint("No hidden PRs.", ui::DIM));
    }
    for (url, hide) in hidden {
        let since = local(&hide.since).unwrap_or_else(|| "?".to_owned());
        ui::out(&format!(
            "{url} {}",
            ui::paint(&format!("(since {since})"), ui::DIM)
        ));
    }
}

pub fn unhide(args: &RefArgs, runner: &dyn Runner) -> ExitCode {
    let mut hidden = match load() {
        Ok(hidden) => hidden,
        Err(code) => return code,
    };
    let mut removed = Vec::new();
    let mut failures = Vec::new();
    for reference in &args.refs {
        match ref_to_url(runner, reference, args.repo.repo.as_deref()) {
            Err(error) => failures.push(format!("{reference}: {error}")),
            Ok(url) if hidden.remove(&url).is_none() => {
                failures.push(format!("{reference} ({url}) is not hidden"))
            }
            Ok(url) => removed.push(url),
        }
    }
    if !removed.is_empty() {
        if let Err(error) = save(&hidden) {
            ui::error(&error);
            return ExitCode::from(FAILURE);
        }
        for url in removed {
            ui::out(&format!("Unhidden {url}"));
        }
    }
    exit_code(&failures)
}
