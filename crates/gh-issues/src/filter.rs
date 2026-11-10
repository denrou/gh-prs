//! Narrowing the fetched issues, from the command line's filters. Pure.

use crate::issue::{Issue, Role};

/// What an issue must match to be shown: every criterion given.
/// Repository and label names are compared without case, as GitHub does,
/// so they are kept in lowercase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// The roles the viewer must hold on the issue, all of them.
    roles: Vec<Role>,
    /// The repositories it may live in, as `owner/name` or a bare name;
    /// none means any.
    repos: Vec<String>,
    /// The labels it must carry, all of them.
    labels: Vec<String>,
}

impl Filter {
    #[must_use]
    pub fn new(roles: Vec<Role>, repos: &[String], labels: &[String]) -> Self {
        let lowercase = |names: &[String]| names.iter().map(|name| name.to_lowercase()).collect();
        Self {
            roles,
            repos: lowercase(repos),
            labels: lowercase(labels),
        }
    }

    /// Nothing to narrow: every issue is kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty() && self.repos.is_empty() && self.labels.is_empty()
    }

    #[must_use]
    pub fn keeps(&self, issue: &Issue) -> bool {
        self.roles.iter().all(|role| issue.roles.contains(role))
            && self.in_a_wanted_repo(issue)
            && self.carries_every_wanted_label(issue)
    }

    fn in_a_wanted_repo(&self, issue: &Issue) -> bool {
        if self.repos.is_empty() {
            return true;
        }
        let full = issue.repo.to_lowercase();
        let name = full.rsplit('/').next().unwrap_or(&full);
        self.repos.iter().any(|wanted| {
            wanted
                == if wanted.contains('/') {
                    full.as_str()
                } else {
                    name
                }
        })
    }

    fn carries_every_wanted_label(&self, issue: &Issue) -> bool {
        if self.labels.is_empty() {
            return true;
        }
        let carried: Vec<String> = issue
            .labels
            .iter()
            .map(|label| label.to_lowercase())
            .collect();
        self.labels.iter().all(|wanted| carried.contains(wanted))
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
            attention: BTreeSet::new(),
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
        let filter = Filter::new(vec![Role::Author, Role::Owner], &[], &[]);
        assert!(!filter.is_empty());
        assert!(filter.keeps(&issue("a/b", &[], &[Role::Author, Role::Owner])));
        assert!(!filter.keeps(&issue("a/b", &[], &[Role::Author])));
        assert!(!filter.keeps(&issue("a/b", &[], &[Role::Owner, Role::Commenter])));
    }

    #[test]
    fn any_of_the_repositories_matches_without_case() {
        let filter = Filter::new(Vec::new(), &words(&["Acme/Widgets", "tool"]), &[]);
        assert!(filter.keeps(&issue("acme/widgets", &[], &[])));
        // A bare name matches the repository whoever owns it.
        assert!(filter.keeps(&issue("other/Tool", &[], &[])));
        assert!(!filter.keeps(&issue("acme/gadgets", &[], &[])));
        // A full name is not matched against the bare one.
        assert!(!filter.keeps(&issue("other/widgets", &[], &[])));
    }

    #[test]
    fn every_label_is_required_without_case() {
        let filter = Filter::new(Vec::new(), &[], &words(&["Bug", "deferred"]));
        assert!(filter.keeps(&issue("a/b", &["bug", "Deferred", "x"], &[])));
        assert!(!filter.keeps(&issue("a/b", &["bug"], &[])));
        assert!(!filter.keeps(&issue("a/b", &[], &[])));
    }

    #[test]
    fn criteria_combine() {
        let filter = Filter::new(vec![Role::Owner], &words(&["widgets"]), &words(&["bug"]));
        assert!(filter.keeps(&issue("acme/widgets", &["bug"], &[Role::Owner])));
        assert!(!filter.keeps(&issue("acme/widgets", &["bug"], &[Role::Author])));
        assert!(!filter.keeps(&issue("acme/gadgets", &["bug"], &[Role::Owner])));
        assert!(!filter.keeps(&issue("acme/widgets", &[], &[Role::Owner])));
    }
}
