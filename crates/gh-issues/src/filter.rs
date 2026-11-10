//! Narrowing the fetched issues, from the command line's filters. Pure.

use crate::issue::{Issue, Role};

/// What an issue must match to be shown: every criterion given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// The roles the viewer must hold on the issue, all of them.
    pub roles: Vec<Role>,
    /// The repositories it may live in, as `owner/name` or a bare name;
    /// none means any.
    pub repos: Vec<String>,
    /// The labels it must carry, all of them.
    pub labels: Vec<String>,
}

impl Filter {
    /// Nothing to narrow: every issue is kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.repos.is_empty() && self.labels.is_empty()
    }

    /// GitHub compares repository and label names without case, so does this.
    #[must_use]
    pub fn keeps(&self, issue: &Issue) -> bool {
        let in_repo = |wanted: &String| {
            let name = issue.repo.rsplit('/').next().unwrap_or(&issue.repo);
            let target = if wanted.contains('/') {
                &issue.repo
            } else {
                name
            };
            target.eq_ignore_ascii_case(wanted)
        };
        let labelled = |wanted: &String| {
            issue
                .labels
                .iter()
                .any(|label| label.to_lowercase() == wanted.to_lowercase())
        };
        self.roles.iter().all(|role| issue.roles.contains(role))
            && (self.repos.is_empty() || self.repos.iter().any(in_repo))
            && self.labels.iter().all(labelled)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn issue(repo: &str, labels: &[&str], roles: &[Role]) -> Issue {
        Issue {
            number: 1,
            repo: repo.to_owned(),
            title: "Title".to_owned(),
            author: None,
            url: String::new(),
            updated_at: String::new(),
            created_at: String::new(),
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            assignees: Vec::new(),
            comments: Some(0),
            last_comment: None,
            roles: roles.iter().copied().collect::<BTreeSet<_>>(),
        }
    }

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn an_empty_filter_keeps_everything() {
        let filter = Filter::default();
        assert!(filter.is_empty());
        assert!(filter.keeps(&issue("acme/widgets", &[], &[])));
    }

    #[test]
    fn every_role_is_required() {
        let filter = Filter {
            roles: vec![Role::Author, Role::Owner],
            ..Filter::default()
        };
        assert!(!filter.is_empty());
        assert!(filter.keeps(&issue("a/b", &[], &[Role::Author, Role::Owner])));
        assert!(!filter.keeps(&issue("a/b", &[], &[Role::Author])));
        assert!(!filter.keeps(&issue("a/b", &[], &[Role::Owner, Role::Commenter])));
    }

    #[test]
    fn any_of_the_repositories_matches_without_case() {
        let filter = Filter {
            repos: words(&["Acme/Widgets", "tool"]),
            ..Filter::default()
        };
        assert!(filter.keeps(&issue("acme/widgets", &[], &[])));
        // A bare name matches the repository whoever owns it.
        assert!(filter.keeps(&issue("other/Tool", &[], &[])));
        assert!(!filter.keeps(&issue("acme/gadgets", &[], &[])));
        // A full name is not matched against the bare one.
        assert!(!filter.keeps(&issue("other/widgets", &[], &[])));
    }

    #[test]
    fn every_label_is_required_without_case() {
        let filter = Filter {
            labels: words(&["Bug", "deferred"]),
            ..Filter::default()
        };
        assert!(filter.keeps(&issue("a/b", &["bug", "Deferred", "x"], &[])));
        assert!(!filter.keeps(&issue("a/b", &["bug"], &[])));
        assert!(!filter.keeps(&issue("a/b", &[], &[])));
    }

    #[test]
    fn criteria_combine() {
        let filter = Filter {
            roles: vec![Role::Owner],
            repos: words(&["widgets"]),
            labels: words(&["bug"]),
        };
        assert!(filter.keeps(&issue("acme/widgets", &["bug"], &[Role::Owner])));
        assert!(!filter.keeps(&issue("acme/widgets", &["bug"], &[Role::Author])));
        assert!(!filter.keeps(&issue("acme/gadgets", &["bug"], &[Role::Owner])));
        assert!(!filter.keeps(&issue("acme/widgets", &[], &[Role::Owner])));
    }
}
