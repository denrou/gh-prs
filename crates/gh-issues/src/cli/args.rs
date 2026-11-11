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
    #[arg(short = 'R', long = "repo", value_name = "[OWNER/]REPO", value_parser = repo_name)]
    pub repo: Vec<String>,

    /// only the issues carrying this label (repeat to require several)
    #[arg(short = 'l', long = "label", value_name = "NAME")]
    pub label: Vec<String>,

    /// only the issues that need you: a reply, a triage, or assigned to you
    // Deliberate: no `-a`, which widens to everything in `gh prs`.
    #[arg(long = "attention")]
    pub attention: bool,

    /// print only the number of issues that need you (for status bars)
    #[arg(long, conflicts_with = "json")]
    pub count: bool,

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
        let mut roles = Vec::new();
        if self.created {
            roles.push(Role::Author);
        }
        if self.owned {
            roles.push(Role::Owner);
        }
        Filter::new(roles, &self.repo, &self.label)
    }
}

/// `owner/name` or a bare name: anything else (a URL, a trailing slash)
/// would match no repository, which must not read as "no issue there".
fn repo_name(value: &str) -> Result<String, String> {
    let parts: Vec<&str> = value.split('/').collect();
    let named = |part: &&str| !part.is_empty() && !part.contains(char::is_whitespace);
    if parts.len() <= 2 && parts.iter().all(named) {
        Ok(value.to_owned())
    } else {
        Err("expected OWNER/REPO or a bare repository name".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_a_name_with_or_without_its_owner() {
        for value in ["widgets", "acme/widgets", "Acme/wid.gets-2"] {
            assert_eq!(repo_name(value), Ok(value.to_owned()));
        }
        for value in [
            "",
            "acme/",
            "/widgets",
            "acme/widgets/",
            "https://github.com/acme/widgets",
            "acme widgets",
        ] {
            assert!(repo_name(value).is_err(), "{value}");
        }
    }
}
