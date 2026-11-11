//! A GraphQL `Issue` node into an [`Issue`].
//!
//! Two layers, as in gh-prs. Serde reads the node into types that mirror
//! the GraphQL shape: every nullable or omittable field is an `Option` and
//! every connection's nodes an `Option<Vec<Option<_>>>`, so nulls survive
//! as unknowns, while a value of the wrong JSON type is an error. GitHub's
//! schema is typed, so a type change means the contract with `gh` changed,
//! and we want to hear about it. Then [`to_issue`] reads the unknowns.

use std::collections::BTreeSet;

use gh_common::ui::strip_control_characters;
use serde::Deserialize;

use crate::issue::{Issue, LastComment};

#[derive(Deserialize)]
struct Connection<T> {
    nodes: Option<Vec<Option<T>>>,
}

fn nodes<T>(connection: Option<&Connection<T>>) -> impl Iterator<Item = &T> {
    connection
        .and_then(|connection| connection.nodes.as_deref())
        .unwrap_or_default()
        .iter()
        .flatten()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawIssue {
    pub(super) id: Option<String>,
    number: u64,
    title: String,
    url: Option<String>,
    state: Option<String>,
    updated_at: Option<String>,
    created_at: Option<String>,
    /// An identity field (it keys de-duplication), so required.
    repository: Repository,
    author: Option<Actor>,
    labels: Option<Connection<Label>>,
    assignees: Option<Connection<Actor>>,
    comments: Option<Comments>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Actor {
    login: Option<String>,
}

#[derive(Deserialize)]
struct Label {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comments {
    total_count: Option<u64>,
    #[serde(flatten)]
    last: Connection<Comment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    author: Option<Actor>,
    created_at: Option<String>,
}

fn login(actor: Option<&Actor>) -> Option<String> {
    actor
        .and_then(|actor| actor.login.as_deref())
        .filter(|login| !login.is_empty())
        .map(str::to_owned)
}

impl RawIssue {
    /// Positively closed: the searches only match open issues, so this is
    /// one closed between the search and the lookup. An unknown state
    /// stays (unknown must never read as "nothing to show").
    pub(super) fn is_closed(&self) -> bool {
        self.state.as_deref() == Some("CLOSED")
    }
}

/// The issue a node describes, without its roles (the caller knows which
/// searches matched it).
pub(super) fn to_issue(raw: &RawIssue) -> Issue {
    let comments = raw.comments.as_ref();
    Issue {
        number: raw.number,
        repo: raw.repository.name_with_owner.clone(),
        title: strip_control_characters(&raw.title),
        author: login(raw.author.as_ref()),
        url: raw.url.clone().unwrap_or_default(),
        updated_at: raw.updated_at.clone().unwrap_or_default(),
        created_at: raw.created_at.clone().unwrap_or_default(),
        labels: nodes(raw.labels.as_ref())
            .filter_map(|label| label.name.as_deref())
            .map(strip_control_characters)
            .collect(),
        assignees: nodes(raw.assignees.as_ref())
            .filter_map(|assignee| login(Some(assignee)))
            .collect(),
        comments: comments.and_then(|comments| comments.total_count),
        last_comment: comments
            .and_then(|comments| nodes(Some(&comments.last)).last())
            .map(|comment| LastComment {
                author: login(comment.author.as_ref()),
                created_at: comment.created_at.clone().unwrap_or_default(),
            }),
        roles: BTreeSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn raw(overrides: Value) -> RawIssue {
        let mut node = json!({
            "id": "I_1",
            "number": 7,
            "title": "Crash on start",
            "repository": {"nameWithOwner": "acme/widgets"},
        });
        for (key, value) in overrides.as_object().unwrap() {
            node[key] = value.clone();
        }
        serde_json::from_value(node).unwrap()
    }

    fn with(overrides: Value) -> Issue {
        to_issue(&raw(overrides))
    }

    #[test]
    fn a_full_node_is_read() {
        let issue = with(json!({
            "url": "https://github.com/acme/widgets/issues/7",
            "state": "OPEN",
            "updatedAt": "2026-11-02T10:00:00Z",
            "createdAt": "2026-11-01T10:00:00Z",
            "author": {"login": "octocat"},
            "labels": {"nodes": [{"name": "bug"}, {"name": "deferred"}]},
            "assignees": {"nodes": [{"login": "me"}]},
            "comments": {
                "totalCount": 3,
                "nodes": [{"author": {"login": "hubot"}, "createdAt": "2026-11-02T10:00:00Z"}]
            },
        }));
        assert_eq!(issue.id(), "acme/widgets#7");
        assert_eq!(issue.title, "Crash on start");
        assert_eq!(issue.author.as_deref(), Some("octocat"));
        assert_eq!(issue.url, "https://github.com/acme/widgets/issues/7");
        assert_eq!(issue.updated_at, "2026-11-02T10:00:00Z");
        assert_eq!(issue.created_at, "2026-11-01T10:00:00Z");
        assert_eq!(issue.labels, ["bug", "deferred"]);
        assert_eq!(issue.assignees, ["me"]);
        assert_eq!(issue.comments, Some(3));
        assert_eq!(
            issue.last_comment,
            Some(LastComment {
                author: Some("hubot".to_owned()),
                created_at: "2026-11-02T10:00:00Z".to_owned(),
            })
        );
        assert!(issue.roles.is_empty());
    }

    #[test]
    fn missing_and_null_fields_are_unknowns() {
        for overrides in [
            json!({}),
            json!({
                "url": null, "state": null, "updatedAt": null, "createdAt": null,
                "author": null, "labels": null, "assignees": null, "comments": null,
            }),
            json!({
                "author": {"login": null},
                "labels": {"nodes": null},
                "assignees": {"nodes": [null, {"login": null}]},
                "comments": {"totalCount": null, "nodes": [null]},
            }),
        ] {
            let issue = with(overrides);
            assert_eq!(issue.author, None);
            assert_eq!(issue.url, "");
            assert!(issue.labels.is_empty());
            assert!(issue.assignees.is_empty());
            assert_eq!(issue.comments, None);
            assert_eq!(issue.last_comment, None);
        }
    }

    #[test]
    fn a_comment_by_a_deleted_account_is_still_a_comment() {
        let issue = with(json!({
            "comments": {"totalCount": 1, "nodes": [{"author": null, "createdAt": "2026-11-02T10:00:00Z"}]}
        }));
        assert_eq!(
            issue.last_comment,
            Some(LastComment {
                author: None,
                created_at: "2026-11-02T10:00:00Z".to_owned(),
            })
        );
    }

    #[test]
    fn no_comment_is_zero_not_unknown() {
        let issue = with(json!({"comments": {"totalCount": 0, "nodes": []}}));
        assert_eq!(issue.comments, Some(0));
        assert_eq!(issue.last_comment, None);
    }

    #[test]
    fn control_characters_stripped_from_title_and_labels() {
        let issue = with(json!({
            "title": "safe\u{1b}]0;evil\u{7} title\u{9b}31m",
            "labels": {"nodes": [{"name": "bug\u{1b}[31m"}]},
        }));
        assert_eq!(issue.title, "safe]0;evil title31m");
        assert_eq!(issue.labels, ["bug[31m"]);
    }

    #[test]
    fn only_a_known_closed_state_is_closed() {
        assert!(raw(json!({"state": "CLOSED"})).is_closed());
        for state in [json!("OPEN"), json!("SOMETHING_NEW"), Value::Null] {
            assert!(!raw(json!({"state": state})).is_closed());
        }
    }

    #[test]
    fn a_wrong_type_is_an_error() {
        let node = json!({
            "number": "7",
            "title": "x",
            "repository": {"nameWithOwner": "acme/widgets"},
        });
        assert!(serde_json::from_value::<RawIssue>(node).is_err());
    }

    #[test]
    fn identity_fields_are_required() {
        let node = json!({"number": 7, "title": "x"});
        assert!(serde_json::from_value::<RawIssue>(node).is_err());
    }
}
