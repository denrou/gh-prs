//! `Issue`, and the roles that say why it was found.

use std::collections::BTreeSet;

/// Why an issue is yours to look at: the search that matched it. The
/// variants are declared in the alphabetical order of their names, so a
/// `BTreeSet`'s order is the sorted order `--json` uses; a test pins it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Assignee,
    Author,
    Commenter,
    /// It lives in a repository the viewer owns.
    Owner,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Assignee, Self::Author, Self::Commenter, Self::Owner];

    /// How `--json` and messages name the role.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Assignee => "assignee",
            Self::Author => "author",
            Self::Commenter => "commenter",
            Self::Owner => "owner",
        }
    }

    /// The search qualifier that grants the role (`<qualifier>:@me`).
    #[must_use]
    pub const fn qualifier(self) -> &'static str {
        match self {
            Self::Assignee => "assignee",
            Self::Author => "author",
            Self::Commenter => "commenter",
            Self::Owner => "user",
        }
    }
}

/// Why an issue needs the viewer. Declared in the alphabetical order of
/// the names, like [`Role`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    /// Assigned to the viewer, and nothing more pressing.
    Assigned,
    /// Someone else spoke last in a thread the viewer has a say in.
    Reply,
    /// Opened by someone else in a repository of the viewer's, untouched.
    Triage,
}

impl Reason {
    pub const ALL: [Self; 3] = [Self::Assigned, Self::Reply, Self::Triage];

    /// How `--json` names the reason.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::Reply => "reply",
            Self::Triage => "triage",
        }
    }
}

/// The latest comment on an issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastComment {
    /// `None` when the account is gone.
    pub author: Option<String>,
    /// Raw ISO text as GitHub sent it.
    pub created_at: String,
}

/// One open issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    /// `owner/name`; an identity field, so required when parsing.
    pub repo: String,
    /// Attacker-controlled: control characters are stripped at ingestion.
    pub title: String,
    pub author: Option<String>,
    pub url: String,
    /// Raw ISO text as GitHub sent it: sorting, display and `--json` use it
    /// as is.
    pub updated_at: String,
    pub created_at: String,
    /// Stripped of control characters, like the title.
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    /// `None` when GitHub didn't say: unknown is not zero.
    pub comments: Option<u64>,
    pub last_comment: Option<LastComment>,
    pub roles: BTreeSet<Role>,
    /// Why it needs the viewer; empty when it doesn't.
    pub attention: BTreeSet<Reason>,
}

impl Issue {
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.attention.is_empty()
    }

    /// `owner/name#number`, how the CLI names an issue in messages.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_sort_by_name() {
        let names: Vec<&str> = Role::ALL.iter().map(|role| role.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let set: BTreeSet<Role> = Role::ALL.iter().rev().copied().collect();
        assert_eq!(set.into_iter().collect::<Vec<_>>(), Role::ALL);
    }

    #[test]
    fn reasons_sort_by_name() {
        let names: Vec<&str> = Reason::ALL.iter().map(|reason| reason.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let set: BTreeSet<Reason> = Reason::ALL.iter().rev().copied().collect();
        assert_eq!(set.into_iter().collect::<Vec<_>>(), Reason::ALL);
    }
}
