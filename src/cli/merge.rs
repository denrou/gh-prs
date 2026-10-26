//! `gh prs merge`: the tool's only write to GitHub.
//!
//! Two phases, because a merge is irreversible. Every ref is resolved and
//! preflighted first; one blocked or unresolvable ref aborts the batch
//! before anything is written, so a typo in the last ref never leaves the
//! first ones merged. Then the PRs are approved (when the viewer can) and
//! merged in order, and the first failure stops the batch, naming the PRs
//! not attempted.

use std::collections::HashSet;
use std::process::ExitCode;

use super::args::MergeArgs;
use super::{FAILURE, ref_to_url, ui};
use crate::gh::{Runner, approve_pr, fetch_pr, merge_pr};
use crate::merge::{MergeMode, merge_blockers, should_approve};
use crate::pr::PullRequest;

pub fn run(args: &MergeArgs, runner: &dyn Runner) -> ExitCode {
    let mode = if args.admin {
        MergeMode::Admin
    } else if args.auto {
        MergeMode::Auto
    } else {
        MergeMode::Now
    };

    let mut planned: Vec<PullRequest> = Vec::new();
    let mut blocked = Vec::new();
    let mut seen = HashSet::new();
    for reference in &args.refs {
        let lookup = ui::spinning(&format!("Checking {reference}…"), |warn| {
            let url = ref_to_url(runner, reference, args.repo.repo.as_deref())?;
            if !seen.insert(url.clone()) {
                return Ok(None);
            }
            fetch_pr(runner, &url, &mut |w| warn(w))
                .map(Some)
                .map_err(|e| e.to_string())
        });
        match lookup {
            Ok(None) => {}
            Ok(Some(pr)) => {
                let blockers = merge_blockers(&pr, mode);
                if blockers.is_empty() {
                    planned.push(pr);
                } else {
                    blocked.push(format!("{}: {}", pr.id(), blockers.join(", ")));
                }
            }
            Err(error) => blocked.push(format!("{reference}: {error}")),
        }
    }
    if !blocked.is_empty() {
        for problem in &blocked {
            ui::error(problem);
        }
        ui::err(&ui::paint("Nothing was merged.", ui::DIM));
        return ExitCode::from(FAILURE);
    }

    let done = if mode == MergeMode::Auto {
        "auto-merge enabled"
    } else {
        "merged"
    };
    for (index, pr) in planned.iter().enumerate() {
        if let Err(error) = merge_one(runner, pr, mode, done) {
            ui::error(&error);
            let remaining: Vec<String> = planned[index + 1..].iter().map(PullRequest::id).collect();
            if !remaining.is_empty() {
                ui::err(&ui::paint(
                    &format!("Stopped; not attempted: {}", remaining.join(", ")),
                    ui::DIM,
                ));
            }
            return ExitCode::from(FAILURE);
        }
    }
    ExitCode::SUCCESS
}

fn merge_one(
    runner: &dyn Runner,
    pr: &PullRequest,
    mode: MergeMode,
    done: &str,
) -> Result<(), String> {
    let id = pr.id();
    if should_approve(pr) {
        ui::spinning(&format!("Approving {id}…"), |_| {
            approve_pr(runner, &pr.url)
        })
        .map_err(|e| e.to_string())?;
        ui::out(&format!("{id}: approved"));
    }
    // The preflight blocks a PR without a head, so this is the belt to that
    // brace: never merge a state nobody looked at.
    let head = pr.head.as_ref().ok_or_else(|| {
        format!(
            "Merging {}: head commit unknown, refusing to merge blindly",
            pr.url
        )
    })?;
    ui::spinning(&format!("Merging {id}…"), |_| {
        merge_pr(runner, &pr.url, head, mode)
    })
    .map_err(|e| e.to_string())?;
    ui::out(&format!(
        "{id}: {done} {}",
        ui::paint("(squash, branch deleted)", ui::DIM)
    ));
    Ok(())
}
