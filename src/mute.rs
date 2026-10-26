//! Mute rules: review requests you never intend to answer, declared once.
//!
//! A snooze silences one state of one PR; a mute rule silences a *kind* of
//! PR for good, typically a bot's dependency bumps for a stack you don't
//! own. Rules are authored in `config.json` (parsed by `config`); this module
//! only decides whether a fetched PR matches one.
//!
//! Muting hides, so it needs positive evidence: the author must match, so
//! must the repository when the rule names any (`repos` scopes a rule *in*,
//! so a misspelt repository makes the rule match nothing and the PRs show),
//! and an exempting label can be ruled out only when the PR's label list is
//! known to be complete. A PR the viewer authored is never muted: authored
//! reasons are the actionable ones.

use std::collections::BTreeSet;

use crate::pr::{Labels, PullRequest};

/// One rule, normalised to lowercase once: GitHub treats logins, label
/// names and repository names case-insensitively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuteRule {
    /// The author login as GitHub's GraphQL reports it: a GitHub App is its
    /// slug, without the `[bot]` suffix the REST API adds.
    author: String,
    /// Labels any one of which exempts a PR; empty means no exemption.
    unless_labels: BTreeSet<String>,
    /// `owner/name` repositories the rule is confined to; empty means all.
    repos: BTreeSet<String>,
}

impl MuteRule {
    #[must_use]
    pub fn new<'a>(
        author: &str,
        unless_labels: impl IntoIterator<Item = &'a str>,
        repos: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        Self {
            author: author.to_lowercase(),
            unless_labels: unless_labels.into_iter().map(str::to_lowercase).collect(),
            repos: repos.into_iter().map(str::to_lowercase).collect(),
        }
    }

    fn matches(&self, author: &str, repo: &str, labels: &Labels) -> bool {
        if self.author != author || !(self.repos.is_empty() || self.repos.contains(repo)) {
            return false;
        }
        if self.unless_labels.is_empty() {
            return true;
        }
        match labels {
            Labels::Complete(labels) => !labels
                .iter()
                .any(|label| self.unless_labels.contains(&label.to_lowercase())),
            // A hidden label might be the exempting one: can't prove it isn't.
            Labels::Partial(_) => false,
        }
    }
}

/// Whether some rule silences this PR (positive evidence only).
#[must_use]
pub fn is_muted(pr: &PullRequest, rules: &[MuteRule]) -> bool {
    let Some(author) = pr.author.as_deref().filter(|a| !a.is_empty()) else {
        return false;
    };
    if pr.is_authored() {
        return false;
    }
    let (author, repo) = (author.to_lowercase(), pr.repo.to_lowercase());
    rules
        .iter()
        .any(|rule| rule.matches(&author, &repo, &pr.labels))
}

/// Partition PRs into (visible, muted), preserving order.
#[must_use]
pub fn split_muted(
    prs: Vec<PullRequest>,
    rules: &[MuteRule],
) -> (Vec<PullRequest>, Vec<PullRequest>) {
    prs.into_iter().partition(|pr| !is_muted(pr, rules))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::Role;
    use crate::pr::testing::{pr, roles};

    /// A Renovate bump in acme/widgets, labels known to be complete.
    fn bump() -> PullRequest {
        PullRequest {
            author: Some("renovate".to_owned()),
            labels: complete(&["C-Deps", "S-Helm"]),
            roles: roles(&[Role::ReviewRequested]),
            ..pr()
        }
    }

    fn complete(labels: &[&str]) -> Labels {
        Labels::Complete(labels.iter().map(|l| (*l).to_owned()).collect())
    }

    fn partial(labels: &[&str]) -> Labels {
        Labels::Partial(labels.iter().map(|l| (*l).to_owned()).collect())
    }

    fn in_repo(repo: &str) -> PullRequest {
        PullRequest {
            repo: repo.to_owned(),
            ..bump()
        }
    }

    fn with_labels(labels: Labels) -> PullRequest {
        PullRequest { labels, ..bump() }
    }

    fn by(author: &str) -> PullRequest {
        PullRequest {
            author: Some(author.to_owned()),
            ..bump()
        }
    }

    fn renovate() -> MuteRule {
        MuteRule::new("renovate", [], [])
    }

    fn renovate_unless_python() -> MuteRule {
        MuteRule::new("renovate", ["S-Python"], [])
    }

    fn renovate_in_widgets() -> MuteRule {
        MuteRule::new("renovate", ["S-Python"], ["acme/widgets"])
    }

    #[test]
    fn normalizes_case_once() {
        let rule = MuteRule::new(
            "Centreon-Renovate",
            ["S-Python"],
            ["Centreon/Centreon-Pulse"],
        );
        assert_eq!(rule.author, "centreon-renovate");
        assert_eq!(rule.unless_labels, BTreeSet::from(["s-python".to_owned()]));
        assert_eq!(
            rule.repos,
            BTreeSet::from(["centreon/centreon-pulse".to_owned()])
        );
    }

    #[test]
    fn no_rules_mutes_nothing() {
        assert!(!is_muted(&bump(), &[]));
    }

    #[test]
    fn author_rule_without_exemption_mutes() {
        assert!(is_muted(&bump(), &[renovate()]));
    }

    #[test]
    fn author_match_is_case_insensitive() {
        assert!(is_muted(&by("Renovate"), &[renovate()]));
    }

    #[test]
    fn other_author_is_not_muted() {
        assert!(!is_muted(&by("octocat"), &[renovate()]));
    }

    #[test]
    fn missing_author_never_matches() {
        // A deleted account comes back as a null author.
        assert!(!is_muted(&by(""), &[renovate()]));
        assert!(!is_muted(
            &PullRequest {
                author: None,
                ..bump()
            },
            &[renovate()]
        ));
    }

    #[test]
    fn exempting_label_keeps_the_pr_visible() {
        let pr = with_labels(complete(&["C-Deps", "S-Python"]));
        assert!(!is_muted(&pr, &[renovate_unless_python()]));
    }

    #[test]
    fn exempting_label_is_case_insensitive() {
        let pr = with_labels(complete(&["s-python"]));
        assert!(!is_muted(&pr, &[renovate_unless_python()]));
    }

    #[test]
    fn without_exempting_label_the_pr_is_muted() {
        assert!(is_muted(&bump(), &[renovate_unless_python()]));
    }

    #[test]
    fn incomplete_label_list_cannot_prove_absence() {
        // A label beyond the cap (or a missing block) could be the exempting one.
        let pr = with_labels(partial(&["C-Deps"]));
        assert!(!is_muted(&pr, &[renovate_unless_python()]));
    }

    #[test]
    fn incomplete_label_list_still_mutes_unconditional_rule() {
        // No exemption to disprove: the author alone is positive evidence.
        assert!(is_muted(&with_labels(partial(&[])), &[renovate()]));
    }

    #[test]
    fn no_labels_at_all_is_muted_when_the_list_is_known_empty() {
        assert!(is_muted(
            &with_labels(complete(&[])),
            &[renovate_unless_python()]
        ));
    }

    #[test]
    fn authored_pr_is_never_muted() {
        let pr = PullRequest {
            roles: roles(&[Role::Author]),
            ..by("me")
        };
        assert!(!is_muted(&pr, &[MuteRule::new("me", [], [])]));
    }

    #[test]
    fn any_matching_rule_suffices() {
        let rules = [
            MuteRule::new("dependabot", [], []),
            renovate_unless_python(),
        ];
        assert!(is_muted(&bump(), &rules));
        let dependabot = PullRequest {
            labels: complete(&["S-Python"]),
            ..by("dependabot")
        };
        assert!(is_muted(&dependabot, &rules));
    }

    #[test]
    fn a_later_unconditional_rule_mutes_what_an_earlier_one_exempted() {
        let pr = with_labels(complete(&["S-Python"]));
        assert!(is_muted(&pr, &[renovate_unless_python(), renovate()]));
    }

    #[test]
    fn repo_scoped_rule_mutes_inside_its_repos() {
        assert!(is_muted(&bump(), &[renovate_in_widgets()]));
    }

    #[test]
    fn repo_scoped_rule_leaves_other_repos_visible() {
        assert!(!is_muted(
            &in_repo("acme/gadgets"),
            &[renovate_in_widgets()]
        ));
    }

    #[test]
    fn repo_match_is_case_insensitive() {
        assert!(is_muted(&in_repo("Acme/Widgets"), &[renovate_in_widgets()]));
    }

    #[test]
    fn repo_scoped_rule_still_honours_the_exempting_label() {
        let pr = with_labels(complete(&["S-Python"]));
        assert!(!is_muted(&pr, &[renovate_in_widgets()]));
    }

    #[test]
    fn repo_scoped_rule_without_labels_mutes_on_author_and_repo() {
        let rule = MuteRule::new("renovate", [], ["acme/widgets"]);
        assert!(is_muted(
            &with_labels(partial(&[])),
            std::slice::from_ref(&rule)
        ));
        assert!(!is_muted(&in_repo("acme/gadgets"), &[rule]));
    }

    #[test]
    fn unscoped_rule_applies_everywhere() {
        assert!(is_muted(&in_repo("acme/gadgets"), &[renovate()]));
    }

    #[test]
    fn rules_scoped_to_different_repos_are_independent() {
        let rules = [
            MuteRule::new("renovate", [], ["acme/widgets"]),
            MuteRule::new("renovate", ["S-Python"], ["acme/gadgets"]),
        ];
        let python = |repo: &str| PullRequest {
            labels: complete(&["S-Python"]),
            ..in_repo(repo)
        };
        let helm = |repo: &str| PullRequest {
            labels: complete(&["S-Helm"]),
            ..in_repo(repo)
        };
        assert!(is_muted(&python("acme/widgets"), &rules));
        assert!(!is_muted(&python("acme/gadgets"), &rules));
        assert!(is_muted(&helm("acme/gadgets"), &rules));
        assert!(!is_muted(&in_repo("acme/tools"), &rules));
    }

    #[test]
    fn split_partitions_preserving_order() {
        let numbered = |number, author: &str| PullRequest {
            number,
            ..by(author)
        };
        let exempt = PullRequest {
            number: 4,
            labels: complete(&["S-Python"]),
            ..bump()
        };
        let prs = vec![
            numbered(1, "octocat"),
            numbered(2, "renovate"),
            numbered(3, "octocat"),
            exempt,
        ];
        let (visible, muted) = split_muted(prs, &[renovate_unless_python()]);
        let numbers = |prs: &[PullRequest]| prs.iter().map(|p| p.number).collect::<Vec<_>>();
        assert_eq!(numbers(&visible), [1, 3, 4]);
        assert_eq!(numbers(&muted), [2]);
    }

    #[test]
    fn split_without_rules_keeps_everything_visible() {
        let (visible, muted) = split_muted(vec![bump(), bump()], &[]);
        assert_eq!((visible.len(), muted.len()), (2, 0));
    }
}
