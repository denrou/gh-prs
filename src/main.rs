use std::process::ExitCode;

fn main() -> ExitCode {
    gh_prs::cli::run(std::env::args_os(), &gh_prs::gh::GhCli)
}
