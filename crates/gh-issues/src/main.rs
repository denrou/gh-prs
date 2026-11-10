use std::process::ExitCode;

fn main() -> ExitCode {
    gh_issues::cli::run(std::env::args_os(), &gh_common::gh::GhCli)
}
