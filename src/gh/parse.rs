//! A GraphQL `PullRequest` node into a [`PullRequest`].
//!
//! Two layers. Serde reads the node into types that mirror the GraphQL
//! shape: every nullable or omittable field is an `Option` and every
//! connection's nodes an `Option<Vec<Option<_>>>`, so nulls survive as
//! unknowns, while a value of the wrong JSON type is an error. GitHub's
//! schema is typed, so a type change means the contract with `gh` changed,
//! and we want to hear about it (docs/rust-port.md, Parsing). Then
//! [`to_pull_request`] applies the fail-safe rules.
//!
//! An enum value this version doesn't know is kept (as
//! [`Reported::Unrecognised`]) and also reported, so a contract change is
//! loud without being fatal.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::fetch::{LABEL_PAGE_LIMIT, REVIEW_PAGE_LIMIT};
use crate::pr::{
    ChangesRequested, Checks, Labels, Mergeable, MyReview, Oid, PrState, PullRequest, Reported,
    ReviewDecision, ReviewState,
};

#[derive(Deserialize)]
pub(super) struct Connection<T> {
    nodes: Option<Vec<Option<T>>>,
}

impl<T> Connection<T> {
    fn nodes(&self) -> &[Option<T>] {
        self.nodes.as_deref().unwrap_or_default()
    }
}

fn nodes<T>(connection: Option<&Connection<T>>) -> &[Option<T>] {
    connection.map(Connection::nodes).unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawPr {
    pub(super) id: Option<String>,
    number: u64,
    title: String,
    url: Option<String>,
    state: Option<String>,
    updated_at: Option<String>,
    created_at: Option<String>,
    is_draft: Option<bool>,
    review_decision: Option<String>,
    mergeable: Option<String>,
    head_ref_oid: Option<String>,
    base_ref: Option<BaseRef>,
    /// An identity field (it keys de-duplication), so required.
    repository: Repository,
    author: Option<Actor>,
    labels: Option<Connection<Label>>,
    review_requests: Option<Connection<ReviewRequest>>,
    latest_reviews: Option<Connection<Review>>,
    latest_opinionated_reviews: Option<Connection<Review>>,
    review_threads: Option<Connection<Thread>>,
    commits: Option<Connection<CommitNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseRef {
    associated_pull_requests: Option<Count>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: Option<u64>,
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
struct ReviewRequest {
    /// Only a User reviewer carries a login in the fragment; a Team doesn't.
    requested_reviewer: Option<Actor>,
}

#[derive(Deserialize)]
struct Review {
    author: Option<Actor>,
    state: Option<String>,
    commit: Option<CommitRef>,
}

#[derive(Deserialize)]
struct CommitRef {
    oid: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    is_resolved: Option<bool>,
    comments: Option<Connection<Comment>>,
}

#[derive(Deserialize)]
struct Comment {
    author: Option<Actor>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Option<Commit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: Option<String>,
}

/// A GitHub enum value this version doesn't know, for the warning.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Unrecognised {
    pub(super) field: &'static str,
    pub(super) value: String,
}

/// The node as a PR, with `viewer` the authenticated login. Roles and
/// attention reasons are left empty for the caller.
pub(super) fn to_pull_request(
    raw: &RawPr,
    viewer: &str,
    unknown: &mut Vec<Unrecognised>,
) -> PullRequest {
    let mut report = |field: &'static str, value: &str| {
        unknown.push(Unrecognised {
            field,
            value: value.to_owned(),
        });
    };
    let is_viewer = |actor: Option<&Actor>| !viewer.is_empty() && login(actor) == Some(viewer);

    // latestReviews already collapses to each reviewer's most recent review.
    let my_review = nodes(raw.latest_reviews.as_ref())
        .iter()
        .flatten()
        .find(|review| is_viewer(review.author.as_ref()))
        .map(|review| MyReview {
            state: reported(
                review.state.as_deref(),
                "review state",
                review_state,
                &mut report,
            ),
            commit: oid(review.commit.as_ref()),
        });

    // Standing changes-requested reviews come from latestOpinionatedReviews,
    // which, unlike latestReviews, survives a re-request. A null node is
    // shape drift, kept as an unknown slot rather than dropped, which would
    // let the survivors read as the whole set.
    let opinionated = nodes(raw.latest_opinionated_reviews.as_ref());
    let mut standing = Vec::new();
    for review in opinionated {
        match review {
            None => standing.push(None),
            Some(review) => {
                let state = reported(
                    review.state.as_deref(),
                    "review state",
                    review_state,
                    &mut report,
                );
                if state == Reported::Known(ReviewState::ChangesRequested) {
                    standing.push(oid(review.commit.as_ref()));
                }
            }
        }
    }
    // At the cap, a standing review may be hidden. Nothing standing already
    // reads as unknown, so the marker only matters when something stands.
    let changes_requested = ChangesRequested {
        truncated: opinionated.len() >= REVIEW_PAGE_LIMIT && !standing.is_empty(),
        commits: standing,
    };

    // A thread the viewer spoke last in is disregarded (the ball is back in
    // the reviewer's court); that carve-out needs positive evidence, so an
    // unknown last commenter or a missing isResolved still counts. Only a
    // missing threads block stays quiet: no evidence any thread exists.
    let threads = nodes(raw.review_threads.as_ref());
    let unresolved_feedback = threads.iter().any(|thread| match thread {
        None => true,
        Some(thread) => {
            let last = nodes(thread.comments.as_ref())
                .last()
                .and_then(Option::as_ref);
            thread.is_resolved != Some(true) && !is_viewer(last.and_then(|c| c.author.as_ref()))
        }
    });

    // Hiding needs positive evidence, so labels are complete only when the
    // block is there, every node named, and the page below its cap.
    let label_nodes = nodes(raw.labels.as_ref());
    let labels: Vec<String> = label_nodes
        .iter()
        .flatten()
        .filter_map(|label| label.name.clone())
        .collect();
    let complete = raw
        .labels
        .as_ref()
        .is_some_and(|block| block.nodes.is_some())
        && labels.len() == label_nodes.len()
        && labels.len() < LABEL_PAGE_LIMIT;

    let requests = nodes(raw.review_requests.as_ref());

    PullRequest {
        number: raw.number,
        repo: raw.repository.name_with_owner.clone(),
        title: strip_control_characters(&raw.title),
        author: login(raw.author.as_ref()).map(str::to_owned),
        url: raw.url.clone().unwrap_or_default(),
        state: reported(raw.state.as_deref(), "state", pr_state, &mut report),
        updated_at: raw.updated_at.clone().unwrap_or_default(),
        created_at: raw.created_at.clone().unwrap_or_default(),
        is_draft: raw.is_draft.unwrap_or(false),
        review_decision: reported(
            raw.review_decision.as_deref(),
            "reviewDecision",
            review_decision,
            &mut report,
        ),
        mergeable: reported(
            raw.mergeable.as_deref(),
            "mergeable",
            mergeable,
            &mut report,
        ),
        checks: checks(raw.commits.as_ref(), &mut report),
        my_review,
        head: raw.head_ref_oid.as_deref().and_then(Oid::new),
        review_requested_explicitly: requests
            .iter()
            .flatten()
            .any(|request| is_viewer(request.requested_reviewer.as_ref())),
        has_pending_review_request: !requests.is_empty(),
        changes_requested,
        unresolved_feedback,
        review_threads_truncated: threads.len() >= REVIEW_PAGE_LIMIT,
        // An open PR whose head is the base branch: this one is stacked on
        // it. associatedPullRequests matches only PRs whose *head* is the
        // ref, so ordinary PRs targeting a maintenance branch stay unstacked.
        // An unknown count reads as stacked: "don't know" never ships.
        stacked: raw
            .base_ref
            .as_ref()
            .and_then(|base| base.associated_pull_requests.as_ref())
            .and_then(|count| count.total_count)
            .is_none_or(|count| count > 0),
        labels: if complete {
            Labels::Complete(labels)
        } else {
            Labels::Partial(labels)
        },
        roles: BTreeSet::new(),
        attention: BTreeSet::new(),
    }
}

/// The CI rollup of the head commit. A missing commits block is drift and
/// reads as still running (never as "no checks", which counts toward
/// ready); a commit without a rollup is the legitimate "no checks
/// configured"; an unrecognised state never counts as passing.
fn checks(
    commits: Option<&Connection<CommitNode>>,
    report: &mut impl FnMut(&'static str, &str),
) -> Checks {
    let Some(commits) = commits else {
        return Checks::Pending;
    };
    let rollup = commits
        .nodes()
        .first()
        .and_then(Option::as_ref)
        .and_then(|node| node.commit.as_ref())
        .and_then(|commit| commit.status_check_rollup.as_ref());
    let Some(rollup) = rollup else {
        return Checks::NoneConfigured;
    };
    let state = rollup.state.as_deref().unwrap_or_default().to_uppercase();
    match state.as_str() {
        "SUCCESS" => Checks::Success,
        "FAILURE" | "ERROR" => Checks::Failure,
        "PENDING" | "EXPECTED" | "" => Checks::Pending,
        other => {
            report("statusCheckRollup state", other);
            Checks::Pending
        }
    }
}

fn login(actor: Option<&Actor>) -> Option<&str> {
    actor.and_then(|a| a.login.as_deref())
}

/// An enum field: `None` and `""` are missing, an unknown value is kept and
/// reported.
fn reported<T>(
    value: Option<&str>,
    field: &'static str,
    known: fn(&str) -> Option<T>,
    report: &mut impl FnMut(&'static str, &str),
) -> Reported<T> {
    match value.filter(|v| !v.is_empty()) {
        None => Reported::Missing,
        Some(value) => known(value).map_or_else(
            || {
                report(field, value);
                Reported::Unrecognised(value.to_owned())
            },
            Reported::Known,
        ),
    }
}

fn pr_state(value: &str) -> Option<PrState> {
    Some(match value {
        "OPEN" => PrState::Open,
        "CLOSED" => PrState::Closed,
        "MERGED" => PrState::Merged,
        _ => return None,
    })
}

fn mergeable(value: &str) -> Option<Mergeable> {
    Some(match value {
        "MERGEABLE" => Mergeable::Mergeable,
        "CONFLICTING" => Mergeable::Conflicting,
        "UNKNOWN" => Mergeable::Unknown,
        _ => return None,
    })
}

fn review_decision(value: &str) -> Option<ReviewDecision> {
    Some(match value {
        "APPROVED" => ReviewDecision::Approved,
        "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
        "REVIEW_REQUIRED" => ReviewDecision::ReviewRequired,
        _ => return None,
    })
}

fn review_state(value: &str) -> Option<ReviewState> {
    Some(match value {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        _ => return None,
    })
}

fn oid(commit: Option<&CommitRef>) -> Option<Oid> {
    commit.and_then(|c| c.oid.as_deref()).and_then(Oid::new)
}

/// PR titles are attacker-controlled. C0 controls, DEL and C1 controls go:
/// ESC and a one-character CSI (U+009B) could otherwise inject terminal
/// escape sequences.
fn strip_control_characters(title: &str) -> String {
    title
        .chars()
        .filter(|c| !matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// A GraphQL node as hydration returns it.
    fn node(overrides: Value) -> Value {
        let mut node = json!({
            "id": "PR_42",
            "number": 42,
            "title": "Fix parser",
            "url": "https://github.com/acme/widgets/pull/42",
            "updatedAt": "2026-07-15T12:00:00Z",
            "createdAt": "2026-07-01T09:30:00Z",
            "isDraft": false,
            "reviewDecision": "REVIEW_REQUIRED",
            "mergeable": "MERGEABLE",
            "repository": {"nameWithOwner": "acme/widgets"},
            "author": {"login": "octocat"},
            "reviewRequests": {"nodes": []},
            "latestReviews": {"nodes": []},
            "latestOpinionatedReviews": {"nodes": []},
            "reviewThreads": {"nodes": []},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]},
            "baseRef": {"associatedPullRequests": {"totalCount": 0}},
        });
        for (key, value) in overrides.as_object().unwrap() {
            node[key] = value.clone();
        }
        node
    }

    fn without(key: &str) -> Value {
        let mut node = node(json!({}));
        node.as_object_mut().unwrap().remove(key);
        node
    }

    fn parse_raw(node: Value) -> Result<(PullRequest, Vec<Unrecognised>), serde_json::Error> {
        let raw: RawPr = serde_json::from_value(node)?;
        let mut unknown = Vec::new();
        let pr = to_pull_request(&raw, "me", &mut unknown);
        Ok((pr, unknown))
    }

    fn parse(node: Value) -> PullRequest {
        parse_raw(node).unwrap().0
    }

    fn with(overrides: Value) -> PullRequest {
        parse(node(overrides))
    }

    fn rollup(state: &str) -> Value {
        json!({"nodes": [{"commit": {"statusCheckRollup": {"state": state}}}]})
    }

    fn opinionated(reviews: Value) -> PullRequest {
        with(json!({"latestOpinionatedReviews": {"nodes": reviews}}))
    }

    fn changes_requested(commit: &str) -> Value {
        json!({"state": "CHANGES_REQUESTED", "commit": {"oid": commit}})
    }

    fn oids(pr: &PullRequest) -> Vec<Option<&str>> {
        pr.changes_requested
            .commits
            .iter()
            .map(|c| c.as_ref().map(Oid::as_str))
            .collect()
    }

    fn thread(last_commenter: Option<&str>, resolved: bool) -> Value {
        let author = last_commenter.map(|login| json!({"login": login}));
        json!({"isResolved": resolved, "comments": {"nodes": [{"author": author}]}})
    }

    fn feedback(threads: Value) -> bool {
        with(json!({"reviewThreads": {"nodes": threads}})).unresolved_feedback
    }

    #[test]
    fn full_node() {
        let pr = with(json!({}));
        assert_eq!(
            (pr.number, pr.repo.as_str(), pr.author.as_deref()),
            (42, "acme/widgets", Some("octocat"))
        );
        assert_eq!(
            pr.review_decision,
            Reported::Known(ReviewDecision::ReviewRequired)
        );
        assert_eq!(pr.mergeable, Reported::Known(Mergeable::Mergeable));
        assert_eq!(pr.id(), "acme/widgets#42");
        assert_eq!(pr.updated_at, "2026-07-15T12:00:00Z");
    }

    #[test]
    fn state_is_parsed_and_missing_when_absent() {
        assert_eq!(
            with(json!({"state": "OPEN"})).state,
            Reported::Known(PrState::Open)
        );
        assert_eq!(with(json!({"state": null})).state, Reported::Missing);
        assert_eq!(with(json!({})).state, Reported::Missing);
    }

    #[test]
    fn no_rollup_or_no_commit_means_no_checks() {
        assert_eq!(with(json!({})).checks, Checks::NoneConfigured);
        assert_eq!(
            with(json!({"commits": {"nodes": []}})).checks,
            Checks::NoneConfigured
        );
    }

    #[test]
    fn rollup_states_normalized() {
        for (state, expected) in [
            ("SUCCESS", Checks::Success),
            ("FAILURE", Checks::Failure),
            ("ERROR", Checks::Failure),
            ("PENDING", Checks::Pending),
            ("EXPECTED", Checks::Pending),
            ("success", Checks::Success),
        ] {
            assert_eq!(
                with(json!({"commits": rollup(state)})).checks,
                expected,
                "{state}"
            );
        }
    }

    #[test]
    fn unknown_rollup_state_is_pending_and_reported() {
        // A future GitHub state must never silently count as green.
        let (pr, unknown) = parse_raw(node(json!({"commits": rollup("BRAND_NEW")}))).unwrap();
        assert_eq!(pr.checks, Checks::Pending);
        assert_eq!(
            unknown,
            [Unrecognised {
                field: "statusCheckRollup state",
                value: "BRAND_NEW".to_owned()
            }]
        );
    }

    #[test]
    fn missing_commits_block_is_pending_not_no_checks() {
        assert_eq!(parse(without("commits")).checks, Checks::Pending);
        assert_eq!(with(json!({"commits": null})).checks, Checks::Pending);
    }

    #[test]
    fn unrecognised_enum_values_are_kept_and_reported() {
        let (pr, unknown) = parse_raw(node(
            json!({"reviewDecision": "QUEUED", "mergeable": "MAYBE", "state": "LOCKED"}),
        ))
        .unwrap();
        assert_eq!(
            pr.review_decision,
            Reported::Unrecognised("QUEUED".to_owned())
        );
        assert_eq!(pr.mergeable, Reported::Unrecognised("MAYBE".to_owned()));
        assert_eq!(pr.state, Reported::Unrecognised("LOCKED".to_owned()));
        let fields: Vec<&str> = unknown.iter().map(|u| u.field).collect();
        assert_eq!(fields, ["state", "reviewDecision", "mergeable"]);
    }

    #[test]
    fn empty_enum_value_is_missing_not_unrecognised() {
        let (pr, unknown) = parse_raw(node(json!({"reviewDecision": ""}))).unwrap();
        assert_eq!(pr.review_decision, Reported::Missing);
        assert!(unknown.is_empty());
    }

    #[test]
    fn my_latest_review_state_and_commit_extracted() {
        let pr = with(json!({"latestReviews": {"nodes": [
            {"author": {"login": "someone-else"}, "state": "APPROVED"},
            {"author": {"login": "me"}, "state": "DISMISSED", "commit": {"oid": "aaa111"}},
        ]}}));
        let review = pr.my_review.unwrap();
        assert_eq!(review.state, Reported::Known(ReviewState::Dismissed));
        assert_eq!(review.commit, Oid::new("aaa111"));
    }

    #[test]
    fn no_review_means_no_review() {
        assert_eq!(with(json!({})).my_review, None);
    }

    #[test]
    fn null_review_commit_is_none() {
        // The reviewed commit is gone (e.g. force-pushed away).
        let pr = with(
            json!({"latestReviews": {"nodes": [{"author": {"login": "me"}, "state": "APPROVED", "commit": null}]}}),
        );
        assert_eq!(pr.my_review.unwrap().commit, None);
    }

    #[test]
    fn head_ref_oid_extracted_or_none() {
        assert_eq!(
            with(json!({"headRefOid": "bbb222"})).head,
            Oid::new("bbb222")
        );
        assert_eq!(with(json!({})).head, None);
        assert_eq!(with(json!({"headRefOid": ""})).head, None);
    }

    #[test]
    fn explicit_user_review_request_detected() {
        let pr = with(
            json!({"reviewRequests": {"nodes": [{"requestedReviewer": {"__typename": "User", "login": "me"}}]}}),
        );
        assert!(pr.review_requested_explicitly);
        assert!(pr.has_pending_review_request);
    }

    #[test]
    fn team_review_request_is_pending_but_not_explicit() {
        let pr = with(
            json!({"reviewRequests": {"nodes": [{"requestedReviewer": {"__typename": "Team"}}]}}),
        );
        assert!(!pr.review_requested_explicitly);
        assert!(pr.has_pending_review_request);
    }

    #[test]
    fn no_review_request_is_not_pending() {
        assert!(!with(json!({})).has_pending_review_request);
    }

    #[test]
    fn stacking_needs_a_known_zero_count() {
        assert!(!with(json!({})).stacked);
        assert!(with(json!({"baseRef": {"associatedPullRequests": {"totalCount": 1}}})).stacked);
        // Positive evidence required: an unknown base never ships.
        assert!(with(json!({"baseRef": null})).stacked);
        assert!(with(json!({"baseRef": {"associatedPullRequests": {}}})).stacked);
    }

    #[test]
    fn changes_requested_commits_collected_from_opinionated_reviews() {
        let pr = opinionated(json!([
            changes_requested("aaa111"),
            {"state": "APPROVED", "commit": {"oid": "bbb222"}},
            changes_requested("ccc333"),
        ]));
        assert_eq!(oids(&pr), [Some("aaa111"), Some("ccc333")]);
        assert!(!pr.changes_requested.truncated);
    }

    #[test]
    fn changes_requested_read_from_opinionated_not_latest_reviews() {
        // The author answered and re-requested: GitHub emptied latestReviews
        // while latestOpinionatedReviews still carries the rejection.
        let pr = with(json!({
            "latestReviews": {"nodes": []},
            "latestOpinionatedReviews": {"nodes": [
                {"author": {"login": "me"}, "state": "CHANGES_REQUESTED", "commit": {"oid": "aaa111"}}
            ]},
        }));
        assert_eq!(oids(&pr), [Some("aaa111")]);
        // latestReviews stays the only source of the viewer's own review.
        assert_eq!(pr.my_review, None);
    }

    #[test]
    fn at_cap_marks_the_standing_reviews_truncated() {
        let full: Vec<Value> = (0..REVIEW_PAGE_LIMIT)
            .map(|i| changes_requested(&format!("old{i}")))
            .collect();
        let pr = opinionated(json!(full));
        assert_eq!(pr.changes_requested.commits.len(), REVIEW_PAGE_LIMIT);
        assert!(pr.changes_requested.truncated);
        let below = opinionated(json!(full[1..]));
        assert!(!below.changes_requested.truncated);
    }

    #[test]
    fn at_cap_with_no_standing_review_stays_empty() {
        let approvals: Vec<Value> = (0..REVIEW_PAGE_LIMIT)
            .map(|i| json!({"state": "APPROVED", "commit": {"oid": format!("ok{i}")}}))
            .collect();
        assert_eq!(
            opinionated(json!(approvals)).changes_requested,
            ChangesRequested::default()
        );
    }

    #[test]
    fn null_opinionated_node_kept_as_unknown() {
        assert_eq!(
            oids(&opinionated(json!([null, changes_requested("aaa111")]))),
            [None, Some("aaa111")]
        );
    }

    #[test]
    fn changes_requested_without_commit_keeps_an_unknown_slot() {
        assert_eq!(
            oids(&opinionated(
                json!([{"state": "CHANGES_REQUESTED", "commit": null}])
            )),
            [None]
        );
    }

    #[test]
    fn no_or_missing_opinionated_reviews_means_none_in_hand() {
        assert_eq!(
            with(json!({})).changes_requested,
            ChangesRequested::default()
        );
        assert_eq!(
            with(json!({"latestOpinionatedReviews": null})).changes_requested,
            ChangesRequested::default()
        );
    }

    #[test]
    fn null_author_is_none() {
        assert_eq!(with(json!({"author": null})).author, None);
    }

    #[test]
    fn repository_is_required() {
        // It keys de-duplication; a missing one must fail parsing, not merge
        // distinct PRs under the same id.
        assert!(parse_raw(node(json!({"repository": null}))).is_err());
        assert!(parse_raw(without("repository")).is_err());
        assert!(parse_raw(without("number")).is_err());
        assert!(parse_raw(without("title")).is_err());
    }

    #[test]
    fn wrong_json_types_are_errors() {
        // Strict parsing: a type change is a contract change with gh.
        for overrides in [
            json!({"number": "42"}),
            json!({"isDraft": "no"}),
            json!({"labels": {"nodes": "C-Deps"}}),
            json!({"baseRef": {"associatedPullRequests": {"totalCount": "3"}}}),
            json!({"reviewThreads": [1]}),
        ] {
            assert!(parse_raw(node(overrides.clone())).is_err(), "{overrides}");
        }
    }

    #[test]
    fn control_characters_stripped_from_title() {
        // ESC (C0), DEL, NUL and a C1 control (U+009B, a one-char CSI).
        let pr = with(json!({"title": "safe\u{1b}]0;evil\u{7} title\u{0}\u{7f}\u{9b}31m"}));
        assert_eq!(pr.title, "safe]0;evil title31m");
    }

    #[test]
    fn unresolved_thread_by_someone_else_is_feedback() {
        assert!(feedback(json!([thread(Some("reviewer"), false)])));
    }

    #[test]
    fn unresolved_thread_i_answered_last_is_not_feedback() {
        assert!(!feedback(json!([thread(Some("me"), false)])));
    }

    #[test]
    fn resolved_thread_is_not_feedback() {
        assert!(!feedback(json!([thread(Some("reviewer"), true)])));
    }

    #[test]
    fn one_unanswered_thread_among_answered_ones_is_feedback() {
        assert!(feedback(json!([
            thread(Some("me"), false),
            thread(Some("reviewer"), true),
            thread(Some("reviewer"), false),
        ])));
    }

    #[test]
    fn unknowns_inside_an_unresolved_thread_are_feedback() {
        // The "I answered last" carve-out needs positive evidence.
        assert!(feedback(json!([thread(None, false)])));
        assert!(feedback(json!([{"isResolved": false, "comments": null}])));
        assert!(feedback(
            json!([{"isResolved": false, "comments": {"nodes": []}}])
        ));
        assert!(feedback(
            json!([{"comments": {"nodes": [{"author": {"login": "reviewer"}}]}}])
        ));
        assert!(feedback(json!([null])));
    }

    #[test]
    fn no_threads_or_no_threads_block_is_not_feedback() {
        assert!(!feedback(json!([])));
        // No evidence any thread exists: flagging would fabricate the reason.
        let pr = with(json!({"reviewThreads": null}));
        assert!(!pr.unresolved_feedback && !pr.review_threads_truncated);
        assert!(!parse(without("reviewThreads")).unresolved_feedback);
    }

    #[test]
    fn full_threads_page_sets_truncated() {
        let threads: Vec<Value> = (0..REVIEW_PAGE_LIMIT)
            .map(|_| thread(Some("me"), false))
            .collect();
        let pr = with(json!({"reviewThreads": {"nodes": threads}}));
        assert!(pr.review_threads_truncated);
        assert!(!pr.unresolved_feedback);
        assert!(
            !with(json!({"reviewThreads": {"nodes": [thread(Some("me"), false)]}}))
                .review_threads_truncated
        );
    }

    #[test]
    fn labels_are_parsed_in_order_and_marked_complete() {
        let pr = with(json!({"labels": {"nodes": [{"name": "C-Deps"}, {"name": "S-Helm"}]}}));
        assert_eq!(
            pr.labels,
            Labels::Complete(vec!["C-Deps".to_owned(), "S-Helm".to_owned()])
        );
        assert_eq!(
            with(json!({"labels": {"nodes": []}})).labels,
            Labels::Complete(vec![])
        );
    }

    #[test]
    fn missing_or_null_labels_are_partial() {
        assert_eq!(with(json!({})).labels, Labels::Partial(vec![]));
        assert_eq!(
            with(json!({"labels": null})).labels,
            Labels::Partial(vec![])
        );
        assert_eq!(
            with(json!({"labels": {"nodes": null}})).labels,
            Labels::Partial(vec![])
        );
    }

    #[test]
    fn null_label_node_is_dropped_and_marks_the_set_partial() {
        let pr = with(json!({"labels": {"nodes": [{"name": "C-Deps"}, null, {"name": null}]}}));
        assert_eq!(pr.labels, Labels::Partial(vec!["C-Deps".to_owned()]));
    }

    #[test]
    fn labels_at_the_cap_are_partial() {
        let names: Vec<Value> = (0..LABEL_PAGE_LIMIT)
            .map(|i| json!({"name": format!("L{i}")}))
            .collect();
        assert!(
            matches!(with(json!({"labels": {"nodes": names}})).labels, Labels::Partial(ref l) if l.len() == LABEL_PAGE_LIMIT)
        );
        assert!(matches!(
            with(json!({"labels": {"nodes": names[1..]}})).labels,
            Labels::Complete(_)
        ));
    }
}
