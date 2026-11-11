//! Which issues need the viewer, and why. Pure: no `gh` calls.
//!
//! The question is whose move it is. Unknowns read as "someone else's
//! word, yours to answer": an issue wrongly shown costs a glance, one
//! wrongly withheld is a person left without an answer.

use std::collections::BTreeSet;

use crate::issue::{Issue, Reason, Role};

/// Why `issue` needs `viewer` (the authenticated login), if it does.
/// At most one reason today: they exclude each other.
#[must_use]
pub fn attention_reasons(issue: &Issue, viewer: &str) -> BTreeSet<Reason> {
    let is_viewer = |login: &String| login.eq_ignore_ascii_case(viewer);
    let has = |role| issue.roles.contains(&role);
    // Read from the issue as well as from the searches: past the 1000
    // matches GitHub serves, a search no longer grants its role.
    let authored = has(Role::Author) || issue.author.as_ref().is_some_and(is_viewer);
    let assigned = has(Role::Assignee) || issue.assignees.iter().any(is_viewer);
    // The viewer has a say in the thread: they opened it, spoke in it, or
    // it was handed to them.
    let engaged = authored || assigned || has(Role::Commenter);

    // Someone else spoke last. A commenter whose account is gone is still
    // someone else.
    let answered_by_someone_else = issue
        .last_comment
        .as_ref()
        .is_some_and(|comment| !comment.author.as_ref().is_some_and(is_viewer));
    let reason = if engaged && answered_by_someone_else {
        Some(Reason::Reply)
    } else if has(Role::Owner) && !engaged && issue.assignees.is_empty() {
        // Opened by someone else in a repository of the viewer's, and
        // nobody has taken it: the viewer has not said a word, and it is
        // assigned to no one. A label is no sign of triage: an issue
        // template sets one on its own.
        Some(Reason::Triage)
    } else if assigned && !authored {
        // Someone else's issue handed to the viewer: standing work, even
        // after the viewer's last word. One the viewer opened and took is
        // their own backlog.
        Some(Reason::Assigned)
    } else {
        None
    };
    reason.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::LastComment;

    fn assigned_to(mut issue: Issue, logins: &[&str]) -> Issue {
        issue.assignees = logins.iter().map(|login| (*login).to_owned()).collect();
        issue
    }

    fn issue(author: Option<&str>, roles: &[Role], last_commenter: Option<Option<&str>>) -> Issue {
        Issue {
            number: 1,
            repo: "acme/widgets".to_owned(),
            title: "Title".to_owned(),
            author: author.map(str::to_owned),
            url: String::new(),
            updated_at: String::new(),
            created_at: String::new(),
            labels: Vec::new(),
            assignees: Vec::new(),
            comments: None,
            last_comment: last_commenter.map(|author| LastComment {
                author: author.map(str::to_owned),
                created_at: String::new(),
            }),
            roles: roles.iter().copied().collect(),
            attention: BTreeSet::new(),
        }
    }

    fn reasons(issue: &Issue) -> Vec<&'static str> {
        attention_reasons(issue, "me")
            .into_iter()
            .map(Reason::name)
            .collect()
    }

    #[test]
    fn my_own_backlog_issue_needs_nothing() {
        let quiet = issue(Some("me"), &[Role::Author, Role::Owner], None);
        assert!(reasons(&quiet).is_empty());
    }

    #[test]
    fn someone_answering_my_issue_calls_for_a_reply() {
        let answered = issue(Some("me"), &[Role::Author], Some(Some("maintainer")));
        assert_eq!(reasons(&answered), ["reply"]);
    }

    #[test]
    fn my_own_last_word_calls_for_nothing() {
        let waiting = issue(Some("me"), &[Role::Author], Some(Some("Me")));
        assert!(reasons(&waiting).is_empty());
    }

    #[test]
    fn a_last_commenter_who_is_gone_is_someone_else() {
        let answered = issue(Some("me"), &[Role::Author], Some(None));
        assert_eq!(reasons(&answered), ["reply"]);
    }

    #[test]
    fn a_thread_i_spoke_in_calls_for_a_reply_when_someone_follows() {
        let followed = issue(Some("octocat"), &[Role::Commenter], Some(Some("octocat")));
        assert_eq!(reasons(&followed), ["reply"]);
    }

    #[test]
    fn someone_elses_issue_in_my_repository_is_to_triage() {
        let new = issue(Some("octocat"), &[Role::Owner], None);
        assert_eq!(reasons(&new), ["triage"]);
        // Their own follow-ups don't make it a reply: I never spoke.
        let insisted = issue(Some("octocat"), &[Role::Owner], Some(Some("octocat")));
        assert_eq!(reasons(&insisted), ["triage"]);
        // An author who is gone is still someone else.
        let orphan = issue(None, &[Role::Owner], None);
        assert_eq!(reasons(&orphan), ["triage"]);
    }

    #[test]
    fn an_issue_i_answered_in_my_repository_is_triaged() {
        let answered = issue(
            Some("octocat"),
            &[Role::Owner, Role::Commenter],
            Some(Some("me")),
        );
        assert!(reasons(&answered).is_empty());
        let back = issue(
            Some("octocat"),
            &[Role::Owner, Role::Commenter],
            Some(Some("octocat")),
        );
        assert_eq!(reasons(&back), ["reply"]);
    }

    #[test]
    fn an_issue_assigned_to_me_is_standing_work() {
        let assigned = issue(Some("octocat"), &[Role::Assignee], None);
        assert_eq!(reasons(&assigned), ["assigned"]);
        // Taking it in my own repository is the triage.
        let taken = issue(Some("octocat"), &[Role::Owner, Role::Assignee], None);
        assert_eq!(reasons(&taken), ["assigned"]);
    }

    #[test]
    fn an_assigned_issue_stays_standing_work_after_my_last_word() {
        let working = issue(
            Some("lead"),
            &[Role::Assignee, Role::Commenter],
            Some(Some("me")),
        );
        assert_eq!(reasons(&working), ["assigned"]);
    }

    #[test]
    fn an_issue_i_opened_and_took_is_my_own_backlog() {
        let mine = issue(
            Some("me"),
            &[Role::Author, Role::Owner, Role::Assignee],
            None,
        );
        assert!(reasons(&mine).is_empty());
        let spoken = issue(
            Some("me"),
            &[Role::Author, Role::Assignee],
            Some(Some("me")),
        );
        assert!(reasons(&spoken).is_empty());
    }

    #[test]
    fn an_issue_taken_by_someone_is_triaged() {
        let delegated = assigned_to(issue(Some("octocat"), &[Role::Owner], None), &["teammate"]);
        assert!(reasons(&delegated).is_empty());
    }

    #[test]
    fn the_assignee_search_missing_does_not_hide_my_assignment() {
        let mine = assigned_to(issue(Some("octocat"), &[Role::Owner], None), &["Me"]);
        assert_eq!(reasons(&mine), ["assigned"]);
    }

    #[test]
    fn a_reply_owed_says_it_instead_of_assigned() {
        let asked = issue(Some("octocat"), &[Role::Assignee], Some(Some("octocat")));
        assert_eq!(reasons(&asked), ["reply"]);
    }

    #[test]
    fn the_author_search_missing_does_not_hide_my_authorship() {
        // Past the 1000 matches GitHub serves, the author role can be
        // missing from an issue that is mine.
        let mine = issue(Some("me"), &[Role::Owner], Some(Some("octocat")));
        assert_eq!(reasons(&mine), ["reply"]);
    }
}
