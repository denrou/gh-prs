"""Tests for the pure logic in gh_prs.gh: attention reasons and GraphQL parsing."""

from gh_prs.gh import PullRequest, _attention_reasons


def _pr(**overrides) -> PullRequest:
    defaults = dict(
        number=1,
        repo="acme/widgets",
        title="Add feature",
        author="octocat",
        url="https://github.com/acme/widgets/pull/1",
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
    )
    defaults.update(overrides)
    return PullRequest(**defaults)


def _node(**overrides) -> dict:
    """A GraphQL PullRequest node as returned by the aliased search blocks."""
    node = {
        "number": 42,
        "title": "Fix parser",
        "url": "https://github.com/acme/widgets/pull/42",
        "updatedAt": "2026-07-15T12:00:00Z",
        "createdAt": "2026-07-01T09:30:00Z",
        "isDraft": False,
        "reviewDecision": "REVIEW_REQUIRED",
        "mergeable": "MERGEABLE",
        "repository": {"nameWithOwner": "acme/widgets"},
        "author": {"login": "octocat"},
        "reviewRequests": {"nodes": []},
        "latestReviews": {"nodes": []},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": None}}]},
    }
    node.update(overrides)
    return node


class TestReviewReason:
    def test_requested_and_pending_needs_review(self):
        pr = _pr(roles={"review-requested"}, review_decision="REVIEW_REQUIRED")
        assert _attention_reasons(pr) == {"review"}

    def test_no_review_decision_still_needs_review(self):
        pr = _pr(roles={"review-requested"}, review_decision="")
        assert _attention_reasons(pr) == {"review"}

    def test_draft_never_needs_attention(self):
        pr = _pr(is_draft=True, roles={"review-requested"})
        assert _attention_reasons(pr) == set()

    def test_my_active_approval_suppresses_review(self):
        pr = _pr(
            roles={"review-requested"},
            review_decision="REVIEW_REQUIRED",
            my_review_state="APPROVED",
        )
        assert _attention_reasons(pr) == set()

    def test_my_changes_requested_suppresses_review(self):
        pr = _pr(
            roles={"review-requested"},
            review_decision="REVIEW_REQUIRED",
            my_review_state="CHANGES_REQUESTED",
        )
        assert _attention_reasons(pr) == set()

    def test_dismissed_review_needs_re_review(self):
        pr = _pr(roles=set(), review_decision="", my_review_state="DISMISSED")
        assert _attention_reasons(pr) == {"review"}

    def test_conflicting_pr_excluded_from_review(self):
        pr = _pr(
            roles={"review-requested"},
            review_decision="REVIEW_REQUIRED",
            mergeable="CONFLICTING",
        )
        assert _attention_reasons(pr) == set()

    # --- The review gate: hide once the PR is mergeable without me ---

    def test_changes_requested_by_others_hides_review(self):
        # The author is reworking the PR; a review now would be premature.
        pr = _pr(roles={"review-requested"}, review_decision="CHANGES_REQUESTED")
        assert _attention_reasons(pr) == set()

    def test_approved_via_team_request_hides_review(self):
        # Approved = mergeable without me; I'm only requested through a team.
        pr = _pr(
            roles={"review-requested"},
            review_decision="APPROVED",
            review_requested_explicitly=False,
        )
        assert _attention_reasons(pr) == set()

    def test_approved_with_explicit_request_still_shows_review(self):
        # I'm personally on the requested-reviewers list — my review is wanted
        # even though the PR could merge without it.
        pr = _pr(
            roles={"review-requested"},
            review_decision="APPROVED",
            review_requested_explicitly=True,
        )
        assert _attention_reasons(pr) == {"review"}


class TestAuthorReasons:
    def test_approved_green_ci_is_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="SUCCESS")
        assert _attention_reasons(pr) == {"ready"}

    def test_approved_without_checks_is_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="")
        assert _attention_reasons(pr) == {"ready"}

    def test_approved_with_pending_checks_is_not_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="PENDING")
        assert _attention_reasons(pr) == set()

    def test_failing_ci_flagged(self):
        pr = _pr(roles={"author"}, checks_state="FAILURE")
        assert _attention_reasons(pr) == {"ci-failed"}

    def test_conflict_flagged(self):
        pr = _pr(roles={"author"}, mergeable="CONFLICTING", checks_state="SUCCESS")
        assert _attention_reasons(pr) == {"conflict"}

    def test_conflict_and_ci_failed_are_independent(self):
        pr = _pr(roles={"author"}, mergeable="CONFLICTING", checks_state="FAILURE")
        assert _attention_reasons(pr) == {"conflict", "ci-failed"}

    def test_conflicting_approved_green_is_not_ready(self):
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="SUCCESS",
            mergeable="CONFLICTING",
        )
        assert _attention_reasons(pr) == {"conflict"}


class TestFromGraphql:
    def test_full_node(self):
        pr = PullRequest.from_graphql(_node(), "me")
        assert pr.number == 42
        assert pr.repo == "acme/widgets"
        assert pr.repo_short == "widgets"
        assert pr.author == "octocat"
        assert pr.review_decision == "REVIEW_REQUIRED"
        assert pr.mergeable == "MERGEABLE"
        assert pr.id == "acme/widgets#42"
        assert pr.updated_date == "2026-07-15"

    def test_no_rollup_means_no_checks(self):
        pr = PullRequest.from_graphql(_node(), "me")
        assert pr.checks_state == ""

    def test_empty_commits_means_no_checks(self):
        pr = PullRequest.from_graphql(_node(commits={"nodes": []}), "me")
        assert pr.checks_state == ""

    def test_rollup_states_normalized(self):
        cases = {
            "SUCCESS": "SUCCESS",
            "FAILURE": "FAILURE",
            "ERROR": "FAILURE",
            "PENDING": "PENDING",
            "EXPECTED": "PENDING",
        }
        for graphql_state, expected in cases.items():
            node = _node(
                commits={
                    "nodes": [
                        {"commit": {"statusCheckRollup": {"state": graphql_state}}}
                    ]
                }
            )
            assert PullRequest.from_graphql(node, "me").checks_state == expected

    def test_unknown_rollup_state_is_pending_not_success(self):
        # A future GitHub state must never silently count as green.
        node = _node(
            commits={
                "nodes": [{"commit": {"statusCheckRollup": {"state": "BRAND_NEW"}}}]
            }
        )
        assert PullRequest.from_graphql(node, "me").checks_state == "PENDING"

    def test_my_latest_review_state_extracted(self):
        node = _node(
            latestReviews={
                "nodes": [
                    {"author": {"login": "someone-else"}, "state": "APPROVED"},
                    {"author": {"login": "me"}, "state": "DISMISSED"},
                ]
            }
        )
        pr = PullRequest.from_graphql(node, "me")
        assert pr.my_review_state == "DISMISSED"

    def test_explicit_user_review_request_detected(self):
        node = _node(
            reviewRequests={
                "nodes": [{"requestedReviewer": {"__typename": "User", "login": "me"}}]
            }
        )
        assert PullRequest.from_graphql(node, "me").review_requested_explicitly

    def test_team_review_request_is_not_explicit(self):
        node = _node(
            reviewRequests={"nodes": [{"requestedReviewer": {"__typename": "Team"}}]}
        )
        assert not PullRequest.from_graphql(node, "me").review_requested_explicitly

    def test_null_author_defaults_to_empty(self):
        pr = PullRequest.from_graphql(_node(author=None), "me")
        assert pr.author == ""

    def test_control_characters_stripped_from_title(self):
        node = _node(title="safe\x1b]0;evil\x07 title\x00")
        assert PullRequest.from_graphql(node, "me").title == "safe]0;evil title"
