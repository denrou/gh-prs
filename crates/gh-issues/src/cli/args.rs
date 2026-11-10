//! The command line, as clap definitions.

use clap::Parser;

use crate::filter::Filter;
use crate::issue::Role;

#[derive(Debug, Parser)]
#[command(
    name = "gh issues",
    // Usage lines name the gh alias, whatever the executable is called.
    bin_name = "gh issues",
    version,
    about = "List the open GitHub issues in repositories you own, and those you created, are assigned or commented on."
)]
pub struct Cli {
    /// only the issues you created
    #[arg(short = 'c', long = "created")]
    pub created: bool,

    /// only the issues in repositories you own
    #[arg(short = 'o', long = "owned")]
    pub owned: bool,

    /// only the issues of this repository; a bare name matches whoever owns
    /// it (repeat for several)
    #[arg(short = 'R', long = "repo", value_name = "[OWNER/]REPO")]
    pub repo: Vec<String>,

    /// only the issues carrying this label (repeat to require several)
    #[arg(short = 'l', long = "label", value_name = "NAME")]
    pub label: Vec<String>,

    /// output raw JSON instead of tables
    #[arg(long)]
    pub json: bool,

    /// disable colored output
    #[arg(long = "no-color")]
    pub no_color: bool,
}

impl Cli {
    /// What the filter flags ask for.
    pub fn filter(&self) -> Filter {
        let roles = [(self.created, Role::Author), (self.owned, Role::Owner)]
            .into_iter()
            .filter_map(|(asked, role)| asked.then_some(role))
            .collect();
        Filter {
            roles,
            repos: self.repo.clone(),
            labels: self.label.clone(),
        }
    }
}
