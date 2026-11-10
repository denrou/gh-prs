//! The `gh issues` command line: parsing, the fetch, and the exit codes
//! (`0` success, `1` failure, `2` usage error, from clap).

mod args;
mod render;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Parser;
use gh_common::gh::Runner;
use gh_common::ui;

use self::args::Cli;
use crate::gh::fetch_issues;
use crate::issue::Role;

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
    if cli.json {
        ui::out(&render::json(&issues));
    } else {
        render::list(&issues);
    }
    ExitCode::SUCCESS
}
