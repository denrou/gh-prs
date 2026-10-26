//! The command line, as clap definitions.

use clap::{ArgGroup, Args, Parser, Subcommand};

const REF_HELP: &str =
    "a PR number (scoped by -R, or the current directory's repository) or a full URL";

#[derive(Debug, Parser)]
#[command(
    name = "gh prs",
    version,
    about = "List GitHub pull requests that need your attention.",
    // argparse accepted unambiguous prefixes of long options; keep that.
    infer_long_args = true,
    group(ArgGroup::new("view").multiple(false)),
)]
pub struct Cli {
    /// PRs you created
    #[arg(short = 'c', long = "created", visible_alias = "me", group = "view")]
    pub created: bool,

    /// PRs awaiting your review
    #[arg(short = 'r', long = "review", group = "view")]
    pub review: bool,

    /// all PRs you are involved with
    #[arg(short = 'a', long = "all", group = "view")]
    pub all: bool,

    /// output raw JSON instead of a table
    #[arg(long)]
    pub json: bool,

    /// print only the number of PRs in the selected view (for status bars)
    #[arg(long)]
    pub count: bool,

    /// flag PRs you created that have gone this long without activity while
    /// still awaiting review or still draft (e.g. 3d, 1w; default 3d,
    /// overrides config.json; days count from local midnight, and in
    /// working days when config.json sets skip_weekends)
    #[arg(long = "stale-after", value_name = "DURATION")]
    pub stale_after: Option<String>,

    /// disable colored output
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// hide PRs from the attention view until they change (with no
    /// arguments: list snoozed PRs)
    #[command(
        long_about = "Hide one or more PRs from the attention view until they get new commits or their status \
        changes — or, with --for, until a fixed window elapses, whichever comes first. With no PR arguments, list \
        the snoozed PRs instead."
    )]
    Snooze(SnoozeArgs),

    /// remove the snooze on one or more PRs
    Unsnooze(RefArgs),

    /// hide PRs from the attention view for good (with no arguments: list
    /// hidden PRs)
    #[command(
        long_about = "Hide one or more PRs from the attention view until you unhide them — unlike a snooze, \
        new commits or a change of status never bring a hidden PR back. With no PR arguments, list the hidden PRs \
        instead."
    )]
    Hide(HideArgs),

    /// bring one or more hidden PRs back
    Unhide(RefArgs),

    /// approve (when you can) and squash-merge one or more PRs
    #[command(
        long_about = "Squash-merge one or more PRs and delete their branches, approving each one first unless \
        you authored it or already approved it. Every PR is checked before anything is merged — open, not a draft, \
        no conflicts, not stacked on another open PR, checks green, review not in the way — and one blocked PR \
        aborts the whole batch. A merge that fails stops the batch at that PR."
    )]
    Merge(MergeArgs),
}

impl Command {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Snooze(_) => "snooze",
            Self::Unsnooze(_) => "unsnooze",
            Self::Hide(_) => "hide",
            Self::Unhide(_) => "unhide",
            Self::Merge(_) => "merge",
        }
    }
}

#[derive(Debug, Args)]
pub struct Repo {
    /// repository for bare PR numbers (default: the repository in the
    /// current directory)
    #[arg(short = 'R', long = "repo", value_name = "OWNER/REPO")]
    pub repo: Option<String>,
}

#[derive(Debug, Args)]
pub struct SnoozeArgs {
    #[arg(value_name = "PR", help = format!("{REF_HELP}; omit to list snoozed PRs"))]
    pub refs: Vec<String>,

    /// also bring the PRs back after this long (e.g. 12h, 3d, 1w; days end at
    /// local midnight, so 1d means tomorrow morning; counted in working days
    /// when config.json sets skip_weekends); without it a snooze lasts until
    /// the PR's head moves or its status changes
    #[arg(long = "for", value_name = "DURATION")]
    pub snooze_for: Option<String>,

    /// forget snoozed PRs that are closed or merged (looks each one up)
    #[arg(long)]
    pub prune: bool,

    #[command(flatten)]
    pub repo: Repo,
}

#[derive(Debug, Args)]
pub struct HideArgs {
    #[arg(value_name = "PR", help = format!("{REF_HELP}; omit to list hidden PRs"))]
    pub refs: Vec<String>,

    /// forget hidden PRs that are closed or merged (looks each one up)
    #[arg(long)]
    pub prune: bool,

    #[command(flatten)]
    pub repo: Repo,
}

#[derive(Debug, Args)]
pub struct RefArgs {
    #[arg(value_name = "PR", required = true, help = REF_HELP)]
    pub refs: Vec<String>,

    #[command(flatten)]
    pub repo: Repo,
}

#[derive(Debug, Args)]
pub struct MergeArgs {
    #[arg(value_name = "PR", required = true, help = REF_HELP)]
    pub refs: Vec<String>,

    /// use administrator privileges to merge a PR that does not meet branch
    /// requirements (checks and review decision are not preflighted)
    #[arg(long, conflicts_with = "auto")]
    pub admin: bool,

    /// enable auto-merge instead of merging now; GitHub merges once checks
    /// pass and reviews are in (neither is preflighted)
    #[arg(long)]
    pub auto: bool,

    #[command(flatten)]
    pub repo: Repo,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("gh-prs").chain(args.iter().copied()))
    }

    #[test]
    fn definitions_are_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn views_are_mutually_exclusive() {
        assert!(parse(&["-c", "-r"]).is_err());
        assert!(parse(&["--me"]).unwrap().created);
    }

    #[test]
    fn no_color_is_accepted_after_a_subcommand() {
        assert!(parse(&["snooze", "12", "--no-color"]).unwrap().no_color);
        assert!(parse(&["--no-color", "hide"]).unwrap().no_color);
    }

    #[test]
    fn admin_and_auto_conflict() {
        assert!(parse(&["merge", "1", "--admin", "--auto"]).is_err());
    }

    #[test]
    fn unsnooze_unhide_and_merge_need_a_ref() {
        for command in ["unsnooze", "unhide", "merge"] {
            assert!(parse(&[command]).is_err(), "{command}");
        }
    }

    #[test]
    fn unambiguous_prefixes_are_accepted() {
        assert!(parse(&["--cou"]).unwrap().count);
    }

    #[test]
    fn the_removed_snooze_flags_are_plain_errors() {
        for flag in ["--snooze", "--unsnooze", "--snoozed"] {
            assert!(parse(&[flag]).is_err(), "{flag}");
        }
    }
}
