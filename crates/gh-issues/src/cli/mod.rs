//! The `gh issues` command line: parsing, the fetch, and the exit codes
//! (`0` success, `1` failure, `2` usage error, from clap).

mod args;
mod render;

use std::ffi::OsString;
use std::process::ExitCode;

use anstyle::AnsiColor;
use clap::Parser;
use gh_common::gh::Runner;
use gh_common::ui;

use self::args::Cli;
use crate::gh::fetch_issues;
use crate::issue::{Issue, Role};

const FAILURE: u8 = 1;

/// Run the CLI on `args` (the program name first), talking to GitHub
/// through `runner`.
pub fn run(args: impl IntoIterator<Item = OsString>, runner: &dyn Runner) -> ExitCode {
    let cli = Cli::try_parse_from(args).unwrap_or_else(|error| error.exit());
    if cli.no_color {
        ui::disable_color();
    }
    let fetched = ui::spinning("Fetching issues…", |warn| {
        fetch_issues(runner, &Role::ALL, &mut |warning| warn(warning))
    });
    let issues = match fetched {
        Ok(issues) => issues,
        Err(error) => {
            ui::error(&error.to_string());
            return ExitCode::from(FAILURE);
        }
    };
    // The searches are the same whatever the filter, so that an issue's
    // roles in `--json` never depend on the flags.
    let filter = cli.filter();
    let none_at_all = issues.is_empty();
    let issues: Vec<&Issue> = issues.iter().filter(|issue| filter.keeps(issue)).collect();
    let needing: Vec<&Issue> = issues
        .iter()
        .copied()
        .filter(|issue| issue.needs_attention())
        .collect();
    if cli.count {
        // Not styled: a status bar reads it.
        println!("{}", needing.len());
        return ExitCode::SUCCESS;
    }
    if cli.json {
        let shown = if cli.attention { &needing } else { &issues };
        return match render::json(shown) {
            Ok(json) => {
                ui::out(&json);
                ExitCode::SUCCESS
            }
            Err(error) => {
                ui::error(&format!("cannot write the issues as JSON: {error}"));
                ExitCode::from(FAILURE)
            }
        };
    }
    if cli.attention {
        if needing.is_empty() {
            ui::out(&format!(
                "{} Nothing needs you.",
                ui::paint("✓", ui::fg(AnsiColor::Green))
            ));
        } else {
            render::attention(&needing, ui::width());
        }
    } else if issues.is_empty() {
        let nothing = if none_at_all || filter.is_empty() {
            "No open issues."
        } else {
            "No matching issues."
        };
        ui::out(&ui::paint(nothing, ui::DIM));
    } else {
        // What needs you first, then the whole backlog, those included.
        if !needing.is_empty() {
            render::attention(&needing, ui::width());
            ui::out("");
        }
        render::by_repository(&issues, ui::width());
    }
    ExitCode::SUCCESS
}
