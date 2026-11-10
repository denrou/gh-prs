//! The command line, as clap definitions.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "gh issues",
    // Usage lines name the gh alias, whatever the executable is called.
    bin_name = "gh issues",
    version,
    about = "List the open GitHub issues in repositories you own, and those you created, are assigned or commented on."
)]
pub struct Cli {
    /// output raw JSON instead of a list
    #[arg(long)]
    pub json: bool,

    /// disable colored output
    #[arg(long = "no-color")]
    pub no_color: bool,
}
