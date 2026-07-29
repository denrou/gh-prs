"""Tests for gh_prs.gh: attention reasons, GraphQL parsing, and fetch orchestration."""

import json
import subprocess
from datetime import UTC, datetime, timedelta

import pytest

from gh_prs import gh
from gh_prs.gh import (
    GhError,
    PullRequest,
    _attention_reasons,
    _is_stale,
    fetch_prs,
)

_NOW = datetime(2026, 7, 27, 12, 0, 0, tzinfo=UTC)
_STALE_AFTER = timedelta(days=3)


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
    return PullRequest(**(defaults | overrides))


def _node(**overrides) -> dict:
    """A GraphQL PullRequest node as returned by a qualifier's search query."""
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
        "latestOpinionatedReviews": {"nodes": []},
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

    def test_draft_never_needs_review(self):
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


class TestNewCommitsReason:
    """The head moved after my review, without a re-request."""

    _REVIEWED = dict(
        roles={"reviewed-by"},
        my_review_state="APPROVED",
        my_review_commit="aaa111",
        head_ref_oid="bbb222",
    )

    def test_head_moved_after_my_approval_flags(self):
        pr = _pr(**self._REVIEWED)
        assert _attention_reasons(pr) == {"new-commits"}

    @pytest.mark.parametrize("state", ["CHANGES_REQUESTED", "COMMENTED"])
    def test_head_moved_after_any_submitted_review_flags(self, state):
        pr = _pr(**self._REVIEWED | {"my_review_state": state})
        assert _attention_reasons(pr) == {"new-commits"}

    def test_unchanged_head_does_not_flag(self):
        pr = _pr(**self._REVIEWED | {"head_ref_oid": "aaa111"})
        assert _attention_reasons(pr) == set()

    @pytest.mark.parametrize("field", ["my_review_commit", "head_ref_oid"])
    def test_one_missing_oid_still_flags(self, field):
        # An absent oid (drift, or the reviewed commit force-pushed away)
        # counts as "moved": unknown must never read as "nothing to do".
        pr = _pr(**self._REVIEWED | {field: ""})
        assert _attention_reasons(pr) == {"new-commits"}

    def test_both_oids_missing_stays_quiet(self):
        # With no oid at all there is nothing to compare.
        pr = _pr(**self._REVIEWED | {"my_review_commit": "", "head_ref_oid": ""})
        assert _attention_reasons(pr) == set()

    def test_conflicting_pr_does_not_flag(self):
        # More commits are coming anyway; reviewing now is premature.
        pr = _pr(**self._REVIEWED | {"mergeable": "CONFLICTING"})
        assert _attention_reasons(pr) == set()

    def test_draft_does_not_flag(self):
        pr = _pr(**self._REVIEWED | {"is_draft": True})
        assert _attention_reasons(pr) == set()

    def test_comment_review_on_my_own_pr_does_not_self_flag(self):
        pr = _pr(**self._REVIEWED | {"roles": {"author", "reviewed-by"}})
        assert _attention_reasons(pr) == set()

    def test_changes_requested_decision_does_not_hide(self):
        # Deliberately unlike "review": commits landing while the overall
        # decision is CHANGES_REQUESTED are plausibly the rework to look at.
        pr = _pr(**self._REVIEWED | {"review_decision": "CHANGES_REQUESTED"})
        assert _attention_reasons(pr) == {"new-commits"}

    def test_re_requested_after_comment_yields_review_not_both(self):
        # An explicit re-request wins: the PR belongs in "review", not twice.
        pr = _pr(
            **self._REVIEWED
            | {
                "roles": {"reviewed-by", "review-requested"},
                "my_review_state": "COMMENTED",
            }
        )
        assert _attention_reasons(pr) == {"review"}

    def test_re_requested_after_approval_still_flags_new_commits(self):
        # An active approval suppresses "review", but the head moving since
        # that approval is exactly what this reason must surface.
        pr = _pr(**self._REVIEWED | {"roles": {"reviewed-by", "review-requested"}})
        assert _attention_reasons(pr) == {"new-commits"}

    def test_dismissed_with_visible_review_reason_wins(self):
        # A dismissal normally re-triggers "review"; no double listing.
        pr = _pr(**self._REVIEWED | {"my_review_state": "DISMISSED"})
        assert _attention_reasons(pr) == {"review"}

    def test_dismissed_hidden_by_approved_decision_flags_new_commits(self):
        # Auto-dismiss-on-push repos: my approval was dismissed by the push,
        # a colleague approved, I'm not personally re-requested — "review"
        # hides (mergeable without me), but the head still moved on me.
        pr = _pr(
            **self._REVIEWED
            | {
                "my_review_state": "DISMISSED",
                "review_decision": "APPROVED",
            }
        )
        assert _attention_reasons(pr) == {"new-commits"}


class TestAuthorReasons:
    def test_approved_green_ci_is_ready(self):
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="SUCCESS",
            mergeable="MERGEABLE",
        )
        assert _attention_reasons(pr) == {"ready"}

    def test_approved_without_checks_is_ready(self):
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="",
            mergeable="MERGEABLE",
        )
        assert _attention_reasons(pr) == {"ready"}

    def test_approved_with_pending_checks_is_not_ready(self):
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="PENDING",
            mergeable="MERGEABLE",
        )
        assert _attention_reasons(pr) == set()

    def test_unknown_mergeability_is_not_ready(self):
        # GitHub returns UNKNOWN while recomputing mergeability (e.g. right
        # after a push); "don't know" must never read as "no conflict".
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="SUCCESS",
            mergeable="UNKNOWN",
        )
        assert _attention_reasons(pr) == set()

    def test_unknown_mergeability_still_shows_review(self):
        # ...but a review request shouldn't vanish while GitHub churns.
        pr = _pr(
            roles={"review-requested"},
            review_decision="REVIEW_REQUIRED",
            mergeable="UNKNOWN",
        )
        assert _attention_reasons(pr) == {"review"}

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


class TestStaleReason:
    """Authored PRs still awaiting review that have gone quiet too long."""

    def _stale_pr(self, **overrides) -> PullRequest:
        # Authored, review still pending, nothing actionable, last touched
        # well before _NOW - _STALE_AFTER.
        base = dict(
            roles={"author"},
            review_decision="REVIEW_REQUIRED",
            updated_at="2026-07-01T12:00:00Z",
        )
        return _pr(**(base | overrides))

    def _reasons(self, pr, now=_NOW, stale_after=_STALE_AFTER):
        return _attention_reasons(pr, now=now, stale_after=stale_after)

    def test_old_waiting_pr_is_stale(self):
        assert self._reasons(self._stale_pr()) == {"stale"}

    def test_no_review_decision_still_stale(self):
        # A PR with no reviewers requested yet, but no approval, still counts
        # as awaiting review.
        assert self._reasons(self._stale_pr(review_decision="")) == {"stale"}

    def test_recently_active_pr_is_not_stale(self):
        pr = self._stale_pr(updated_at="2026-07-26T12:00:00Z")
        assert self._reasons(pr) == set()

    def test_exactly_at_threshold_is_stale(self):
        # now - updated == stale_after: the boundary counts as stale (>=).
        pr = self._stale_pr(updated_at="2026-07-24T12:00:00Z")
        assert self._reasons(pr) == {"stale"}

    def test_approved_pr_is_not_stale(self):
        # Approved-but-not-mergeable (UNKNOWN) isn't awaiting review — it's
        # waiting to merge, not a reviewer nudge.
        pr = self._stale_pr(review_decision="APPROVED", mergeable="UNKNOWN")
        assert self._reasons(pr) == set()

    def test_changes_requested_pr_is_not_stale(self):
        # The author is reworking it; pinging reviewers would be premature.
        pr = self._stale_pr(review_decision="CHANGES_REQUESTED")
        assert self._reasons(pr) == set()

    def _answered_pr(self, **overrides) -> PullRequest:
        # Changes were requested against an older commit, the author has since
        # pushed, and a reviewer is on the hook again. GitHub still reports
        # CHANGES_REQUESTED (it only clears on a new review), but the PR is
        # genuinely waiting on reviewers.
        base = dict(
            review_decision="CHANGES_REQUESTED",
            changes_requested_commits=("old111",),
            head_ref_oid="new222",
            has_pending_review_request=True,
        )
        return self._stale_pr(**(base | overrides))

    def test_answered_changes_requested_is_stale_again(self):
        assert self._reasons(self._answered_pr()) == {"stale"}

    def test_answered_but_never_re_requested_is_not_stale(self):
        # No pending request: by GitHub convention the author hasn't handed it
        # back yet, so this is still their own unfinished business.
        pr = self._answered_pr(has_pending_review_request=False)
        assert self._reasons(pr) == set()

    def test_re_requested_without_pushing_is_not_stale(self):
        # The review still describes the current head — nothing was answered.
        pr = self._answered_pr(changes_requested_commits=("new222",))
        assert self._reasons(pr) == set()

    def test_one_unanswered_review_blocks_the_nudge(self):
        # Two reviewers stand on changes-requested and only one was answered;
        # the other still describes the head.
        pr = self._answered_pr(changes_requested_commits=("old111", "new222"))
        assert self._reasons(pr) == set()

    def test_unlinked_review_commit_is_not_stale(self):
        # GitHub dropped the reviewed commit (e.g. force-pushed away): unknown
        # must not be read as answered, or the nudge is fabricated.
        pr = self._answered_pr(changes_requested_commits=("",))
        assert self._reasons(pr) == set()

    def test_missing_head_oid_is_not_stale(self):
        pr = self._answered_pr(head_ref_oid="")
        assert self._reasons(pr) == set()

    def test_hidden_changes_requested_review_is_not_stale(self):
        # No standing review in hand — either nobody stands on
        # changes-requested (yet the decision says otherwise), or the
        # latestOpinionatedReviews 50-node cap hid them all. Both stay quiet.
        pr = self._answered_pr(changes_requested_commits=())
        assert self._reasons(pr) == set()

    def test_cap_marker_blocks_the_nudge(self):
        # from_graphql appends an unknown ("") slot when the connection is at
        # the cap: a review hidden by truncation may still be against the head,
        # so the visible ones must not be read as the whole set.
        pr = self._answered_pr(changes_requested_commits=("old111", ""))
        assert self._reasons(pr) == set()

    def test_approved_is_never_stale_even_when_answered(self):
        # The APPROVED short-circuit must not be reachable around: approved is
        # waiting to merge, whatever the review history looks like. Guards the
        # nesting in _awaiting_review — flattening it to a single `or` still
        # passes every other test in this class.
        pr = self._answered_pr(review_decision="APPROVED", mergeable="UNKNOWN")
        assert self._reasons(pr) == set()

    def test_answered_changes_requested_still_yields_to_real_actions(self):
        # The nudge is last in line: a conflict or red CI is mine to fix.
        assert self._reasons(self._answered_pr(mergeable="CONFLICTING")) == {"conflict"}
        assert self._reasons(self._answered_pr(checks_state="FAILURE")) == {"ci-failed"}

    def test_answered_changes_requested_respects_the_threshold(self):
        pr = self._answered_pr(updated_at="2026-07-26T12:00:00Z")
        assert self._reasons(pr) == set()

    def test_failing_ci_takes_precedence_over_stale(self):
        pr = self._stale_pr(checks_state="FAILURE")
        assert self._reasons(pr) == {"ci-failed"}

    def test_conflict_takes_precedence_over_stale(self):
        pr = self._stale_pr(mergeable="CONFLICTING")
        assert self._reasons(pr) == {"conflict"}

    def test_ready_takes_precedence_over_stale(self):
        pr = self._stale_pr(
            review_decision="APPROVED", checks_state="SUCCESS", mergeable="MERGEABLE"
        )
        assert self._reasons(pr) == {"ready"}

    def test_reviewer_not_author_is_never_stale(self):
        # 'stale' is only for PRs I authored; an old PR I'm asked to review
        # surfaces as 'review', never 'stale'.
        pr = self._stale_pr(roles={"review-requested"})
        assert "stale" not in self._reasons(pr)

    def test_draft_is_stale_draft_not_stale(self):
        # A quiet authored draft gets its own nudge — 'stale' is strictly for
        # PRs waiting on reviewers.
        assert self._reasons(self._stale_pr(is_draft=True)) == {"stale-draft"}

    def test_disabled_when_no_threshold(self):
        # Omitting now/stale_after (or stale_after=None) turns the nudge off.
        assert _attention_reasons(self._stale_pr()) == set()
        assert _attention_reasons(self._stale_pr(), now=_NOW, stale_after=None) == set()

    def test_unparseable_updated_at_is_not_stale(self):
        # Fail direction here is the opposite of the rest of the module: an
        # unknown age must not fabricate a nudge on a possibly-fresh PR.
        assert self._reasons(self._stale_pr(updated_at="")) == set()
        assert self._reasons(self._stale_pr(updated_at="not-a-date")) == set()


class TestDraftReasons:
    """Authored drafts: only 'conflict' and the 'stale-draft' nudge can fire."""

    def _draft(self, **overrides) -> PullRequest:
        # Authored draft, last touched well before _NOW - _STALE_AFTER.
        base = dict(
            roles={"author"},
            is_draft=True,
            updated_at="2026-07-01T12:00:00Z",
        )
        return _pr(**(base | overrides))

    _FRESH = "2026-07-26T12:00:00Z"

    def _reasons(self, pr, now=_NOW, stale_after=_STALE_AFTER):
        return _attention_reasons(pr, now=now, stale_after=stale_after)

    def test_conflicting_draft_flags_conflict(self):
        # The base moved underneath the draft; resolving early is cheaper.
        pr = self._draft(mergeable="CONFLICTING", updated_at=self._FRESH)
        assert self._reasons(pr) == {"conflict"}

    def test_quiet_draft_flags_stale_draft(self):
        assert self._reasons(self._draft()) == {"stale-draft"}

    def test_fresh_draft_stays_quiet(self):
        assert self._reasons(self._draft(updated_at=self._FRESH)) == set()

    def test_conflict_takes_precedence_over_stale_draft(self):
        pr = self._draft(mergeable="CONFLICTING")
        assert self._reasons(pr) == {"conflict"}

    def test_failing_ci_on_draft_stays_quiet(self):
        # Red CI is expected while iterating on a draft.
        pr = self._draft(checks_state="FAILURE", updated_at=self._FRESH)
        assert self._reasons(pr) == set()

    def test_failing_ci_does_not_gate_stale_draft(self):
        # Red CI neither flags nor suppresses the nudge on a quiet draft.
        pr = self._draft(checks_state="FAILURE")
        assert self._reasons(pr) == {"stale-draft"}

    def test_unknown_mergeability_still_flags_stale_draft(self):
        # UNKNOWN (GitHub recomputing mergeability) is not a conflict, and it
        # must not swallow the nudge on a quiet draft either.
        pr = self._draft(mergeable="UNKNOWN")
        assert self._reasons(pr) == {"stale-draft"}

    def test_approved_green_draft_is_not_ready(self):
        pr = self._draft(
            review_decision="APPROVED",
            checks_state="SUCCESS",
            mergeable="MERGEABLE",
            updated_at=self._FRESH,
        )
        assert self._reasons(pr) == set()

    def test_review_decision_does_not_gate_stale_draft(self):
        # Unlike 'stale', the nudge is about the parked draft itself, not
        # about waiting on reviewers — a quiet draft flags regardless of any
        # review decision on it.
        pr = self._draft(review_decision="CHANGES_REQUESTED")
        assert self._reasons(pr) == {"stale-draft"}

    def test_someone_elses_draft_never_flags(self):
        pr = self._draft(roles={"review-requested"}, mergeable="CONFLICTING")
        assert self._reasons(pr) == set()

    def test_authored_draft_with_review_requested_role_still_flags(self):
        # Being asked to review my own draft doesn't strip authorship.
        pr = self._draft(roles={"author", "review-requested"}, mergeable="CONFLICTING")
        assert self._reasons(pr) == {"conflict"}

    def test_disabled_when_no_threshold(self):
        assert _attention_reasons(self._draft()) == set()
        assert _attention_reasons(self._draft(), now=_NOW, stale_after=None) == set()

    def test_unparseable_updated_at_is_not_stale_draft(self):
        # Same quiet fail direction as 'stale': an unknown age must not
        # fabricate a nudge on a possibly-fresh draft.
        assert self._reasons(self._draft(updated_at="")) == set()
        assert self._reasons(self._draft(updated_at="not-a-date")) == set()


class TestIsStale:
    def test_old_timestamp_is_stale(self):
        assert _is_stale("2026-07-01T12:00:00Z", _NOW, _STALE_AFTER) is True

    def test_recent_timestamp_is_not_stale(self):
        assert _is_stale("2026-07-26T12:00:00Z", _NOW, _STALE_AFTER) is False

    def test_boundary_is_stale(self):
        assert _is_stale("2026-07-24T12:00:00Z", _NOW, _STALE_AFTER) is True

    @pytest.mark.parametrize("bad", ["", "not-a-date", "2026-13-99"])
    def test_unparseable_is_not_stale(self, bad):
        assert _is_stale(bad, _NOW, _STALE_AFTER) is False

    def test_non_string_is_not_stale(self):
        # A non-str makes fromisoformat raise TypeError, not ValueError; the
        # catch covers it so a shape-drifted None can't crash the nudge.
        assert _is_stale(None, _NOW, _STALE_AFTER) is False

    def test_naive_timestamp_is_not_stale(self):
        # A timestamp without a timezone can't be compared safely → no nudge.
        assert _is_stale("2026-07-01T12:00:00", _NOW, _STALE_AFTER) is False


class TestPrFragment:
    def test_review_connections_match_the_page_limit_constant(self):
        # _REVIEW_PAGE_LIMIT drives the at-cap unknown marker in from_graphql,
        # but the fragment is a plain literal, so the two can drift apart
        # silently. All three review connections must use the constant.
        assert gh._PR_FRAGMENT.count(f"first: {gh._REVIEW_PAGE_LIMIT}") == 3, (
            "reviewRequests / latestReviews / latestOpinionatedReviews"
        )

    def test_fragment_requests_every_field_the_parser_reads(self):
        for field in (
            "headRefOid",
            "reviewRequests",
            "latestReviews",
            "latestOpinionatedReviews",
            "statusCheckRollup",
        ):
            assert field in gh._PR_FRAGMENT, field


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

    @pytest.mark.parametrize(
        ("graphql_state", "expected"),
        [
            ("SUCCESS", "SUCCESS"),
            ("FAILURE", "FAILURE"),
            ("ERROR", "FAILURE"),
            ("PENDING", "PENDING"),
            ("EXPECTED", "PENDING"),
        ],
    )
    def test_rollup_states_normalized(self, graphql_state, expected):
        node = _node(
            commits={
                "nodes": [{"commit": {"statusCheckRollup": {"state": graphql_state}}}]
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

    def test_my_latest_review_state_and_commit_extracted(self):
        node = _node(
            latestReviews={
                "nodes": [
                    {"author": {"login": "someone-else"}, "state": "APPROVED"},
                    {
                        "author": {"login": "me"},
                        "state": "DISMISSED",
                        "commit": {"oid": "aaa111"},
                    },
                ]
            }
        )
        pr = PullRequest.from_graphql(node, "me")
        assert pr.my_review_state == "DISMISSED"
        assert pr.my_review_commit == "aaa111"

    def test_no_review_means_empty_review_commit(self):
        pr = PullRequest.from_graphql(_node(), "me")
        assert pr.my_review_state == ""
        assert pr.my_review_commit == ""

    def test_null_review_commit_defaults_to_empty(self):
        # GitHub returns a null commit when the reviewed commit is gone
        # (e.g. force-pushed away); "" then reads as "moved" in
        # _attention_reasons (checked in TestNewCommitsReason).
        node = _node(
            latestReviews={
                "nodes": [
                    {"author": {"login": "me"}, "state": "APPROVED", "commit": None}
                ]
            }
        )
        assert PullRequest.from_graphql(node, "me").my_review_commit == ""

    def test_head_ref_oid_extracted(self):
        assert (
            PullRequest.from_graphql(_node(headRefOid="bbb222"), "me").head_ref_oid
            == "bbb222"
        )

    def test_missing_head_ref_oid_defaults_to_empty(self):
        assert PullRequest.from_graphql(_node(), "me").head_ref_oid == ""

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

    def test_team_review_request_still_counts_as_pending(self):
        # Not *my* request, but the PR is waiting on someone.
        node = _node(
            reviewRequests={"nodes": [{"requestedReviewer": {"__typename": "Team"}}]}
        )
        assert PullRequest.from_graphql(node, "me").has_pending_review_request

    def test_no_review_request_is_not_pending(self):
        assert not PullRequest.from_graphql(_node(), "me").has_pending_review_request

    def test_changes_requested_commits_collected_from_opinionated_reviews(self):
        # Only reviewers standing on CHANGES_REQUESTED contribute; approvals
        # are skipped.
        node = _node(
            latestOpinionatedReviews={
                "nodes": [
                    {"state": "CHANGES_REQUESTED", "commit": {"oid": "aaa111"}},
                    {"state": "APPROVED", "commit": {"oid": "bbb222"}},
                    {"state": "CHANGES_REQUESTED", "commit": {"oid": "ccc333"}},
                ]
            }
        )
        pr = PullRequest.from_graphql(node, "me")
        assert pr.changes_requested_commits == ("aaa111", "ccc333")

    def test_changes_requested_read_from_opinionated_not_latest_reviews(self):
        # The case the carve-out exists for: the author answered and
        # re-requested, so GitHub emptied latestReviews while
        # latestOpinionatedReviews still carries the standing rejection.
        node = _node(
            latestReviews={"nodes": []},
            latestOpinionatedReviews={
                "nodes": [
                    {
                        "author": {"login": "me"},
                        "state": "CHANGES_REQUESTED",
                        "commit": {"oid": "aaa111"},
                    }
                ]
            },
        )
        pr = PullRequest.from_graphql(node, "me")
        assert pr.changes_requested_commits == ("aaa111",)
        # latestReviews stays the sole source of the viewer's own review: the
        # opinionated entry is the viewer's here, and must not leak into it.
        assert (pr.my_review_state, pr.my_review_commit) == ("", "")

    def test_at_cap_appends_unknown_slot(self):
        # A full page means a standing review may be hidden beyond it.
        node = _node(
            latestOpinionatedReviews={
                "nodes": [
                    {"state": "CHANGES_REQUESTED", "commit": {"oid": f"old{i}"}}
                    for i in range(gh._REVIEW_PAGE_LIMIT)
                ]
            }
        )
        commits = PullRequest.from_graphql(node, "me").changes_requested_commits
        assert len(commits) == gh._REVIEW_PAGE_LIMIT + 1
        assert commits[-1] == ""

    def test_at_cap_with_no_standing_review_stays_empty(self):
        # Empty already reads as unknown; no marker needed.
        node = _node(
            latestOpinionatedReviews={
                "nodes": [
                    {"state": "APPROVED", "commit": {"oid": f"ok{i}"}}
                    for i in range(gh._REVIEW_PAGE_LIMIT)
                ]
            }
        )
        assert PullRequest.from_graphql(node, "me").changes_requested_commits == ()

    def test_below_cap_appends_nothing(self):
        node = _node(
            latestOpinionatedReviews={
                "nodes": [{"state": "CHANGES_REQUESTED", "commit": {"oid": "aaa111"}}]
            }
        )
        assert PullRequest.from_graphql(node, "me").changes_requested_commits == (
            "aaa111",
        )

    def test_null_opinionated_node_kept_as_unknown(self):
        # Shape drift must not be silently dropped: a null node could have been
        # a standing review, so it becomes an unknown slot rather than nothing.
        node = _node(
            latestOpinionatedReviews={
                "nodes": [
                    None,
                    {"state": "CHANGES_REQUESTED", "commit": {"oid": "aaa111"}},
                ]
            }
        )
        assert PullRequest.from_graphql(node, "me").changes_requested_commits == (
            "",
            "aaa111",
        )

    def test_changes_requested_without_commit_keeps_an_empty_slot(self):
        # A dropped commit link must stay visible as "unknown" rather than be
        # filtered out — _changes_requested_addressed reads it as not answered.
        node = _node(
            latestOpinionatedReviews={
                "nodes": [{"state": "CHANGES_REQUESTED", "commit": None}]
            }
        )
        assert PullRequest.from_graphql(node, "me").changes_requested_commits == ("",)

    def test_no_changes_requested_review_means_empty_commits(self):
        assert PullRequest.from_graphql(_node(), "me").changes_requested_commits == ()

    def test_missing_opinionated_reviews_block_means_empty_commits(self):
        # Shape drift stays quiet: "unknown" reads as not answered, which
        # suppresses the nudge rather than fabricating it.
        node = _node(latestOpinionatedReviews=None)
        assert PullRequest.from_graphql(node, "me").changes_requested_commits == ()

    def test_null_author_defaults_to_empty(self):
        pr = PullRequest.from_graphql(_node(author=None), "me")
        assert pr.author == ""

    def test_missing_commits_block_is_pending_not_no_checks(self):
        # Shape drift ("we don't know") must not read as "no checks
        # configured" (which counts toward ready).
        node = _node()
        del node["commits"]
        assert PullRequest.from_graphql(node, "me").checks_state == "PENDING"

    def test_null_repository_raises(self):
        # repository keys de-duplication; a missing one must fail parsing,
        # not silently merge distinct PRs under the same id.
        with pytest.raises((KeyError, TypeError)):
            PullRequest.from_graphql(_node(repository=None), "me")

    def test_control_characters_stripped_from_title(self):
        # Includes ESC (C0), DEL, and a C1 control (U+009B, one-char CSI).
        node = _node(title="safe\x1b]0;evil\x07 title\x00\x9b31m")
        assert PullRequest.from_graphql(node, "me").title == "safe]0;evil title31m"


def _completed(
    stdout: str, returncode: int = 0, stderr: str = ""
) -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(
        args=["gh"], returncode=returncode, stdout=stdout, stderr=stderr
    )


def _search_payload(
    nodes: list, viewer: str = "me", issue_count: int | None = None
) -> str:
    return json.dumps(
        {
            "data": {
                "viewer": {"login": viewer},
                "results": {
                    "issueCount": len(nodes) if issue_count is None else issue_count,
                    "nodes": nodes,
                },
            }
        }
    )


class TestSearch:
    """_search / _graphql response-envelope validation (mocked _run_gh)."""

    def test_happy_path(self, monkeypatch):
        monkeypatch.setattr(
            gh, "_run_gh", lambda *a: _completed(_search_payload([_node()]))
        )
        viewer, nodes, count = gh._search("author")
        assert viewer == "me"
        assert len(nodes) == 1
        assert count == 1

    def test_nonzero_exit_raises_with_context(self, monkeypatch):
        monkeypatch.setattr(
            gh, "_run_gh", lambda *a: _completed("", returncode=1, stderr="401")
        )
        with pytest.raises(GhError, match="Search 'author'.*401"):
            gh._search("author")

    def test_invalid_json_raises(self, monkeypatch):
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed("not json"))
        with pytest.raises(GhError, match="invalid JSON"):
            gh._search("author")

    def test_non_object_payload_raises(self, monkeypatch):
        # json.loads guarantees valid JSON, not a JSON object.
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed("null"))
        with pytest.raises(GhError, match="not a JSON object"):
            gh._search("author")

    def test_graphql_errors_array_raises(self, monkeypatch):
        payload = json.dumps({"data": None, "errors": [{"message": "rate limited"}]})
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="rate limited"):
            gh._search("author")

    def test_missing_data_block_raises(self, monkeypatch):
        # An envelope without data must be an error, not "zero PRs".
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed('{"data": null}'))
        with pytest.raises(GhError, match="no data block"):
            gh._search("author")

    def test_missing_results_block_raises(self, monkeypatch):
        payload = json.dumps({"data": {"viewer": {"login": "me"}}})
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="no results block"):
            gh._search("author")


class TestCountPrs:
    def test_returns_issue_count(self, monkeypatch):
        payload = json.dumps({"data": {"results": {"issueCount": 143}}})
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        assert gh.count_prs("author") == 143

    def test_missing_issue_count_raises(self, monkeypatch):
        payload = json.dumps({"data": {"results": {}}})
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="no issueCount"):
            gh.count_prs("author")


class TestFetchPrHead:
    """fetch_pr_head response validation (mocked _run_gh)."""

    def test_happy_path_passes_url_through(self, monkeypatch):
        calls: list[tuple] = []

        def fake_run(*args):
            calls.append(args)
            return _completed('{"headRefOid": "cafe123"}')

        monkeypatch.setattr(gh, "_run_gh", fake_run)
        url = "https://github.com/acme/widgets/pull/42"
        assert gh.fetch_pr_head(url) == "cafe123"
        assert calls == [("pr", "view", url, "--json", "headRefOid")]

    def test_nonzero_exit_raises_with_detail(self, monkeypatch):
        monkeypatch.setattr(
            gh, "_run_gh", lambda *a: _completed("", returncode=1, stderr="no such PR")
        )
        with pytest.raises(GhError, match="no such PR"):
            gh.fetch_pr_head("url")

    def test_invalid_json_raises(self, monkeypatch):
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed("not json"))
        with pytest.raises(GhError, match="invalid JSON"):
            gh.fetch_pr_head("url")

    def test_non_dict_payload_raises(self, monkeypatch):
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed("null"))
        with pytest.raises(GhError, match="no headRefOid"):
            gh.fetch_pr_head("url")

    @pytest.mark.parametrize(
        "payload",
        ["{}", '{"headRefOid": ""}', '{"headRefOid": null}', '{"headRefOid": 5}'],
    )
    def test_missing_or_invalid_oid_raises(self, monkeypatch, payload):
        # A snooze recorded against an unknown head could hide newer work.
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="no headRefOid"):
            gh.fetch_pr_head("url")


class TestResolvePr:
    """resolve_pr: bare-number resolution and response validation (mocked _run_gh)."""

    def test_bare_number_uses_current_repo(self, monkeypatch):
        calls: list[tuple] = []

        def fake_run(*args):
            calls.append(args)
            return _completed(
                '{"url": "https://github.com/acme/widgets/pull/42",'
                ' "headRefOid": "cafe123"}'
            )

        monkeypatch.setattr(gh, "_run_gh", fake_run)
        url, oid = gh.resolve_pr("42")
        assert url == "https://github.com/acme/widgets/pull/42"
        assert oid == "cafe123"
        # No --repo when none is given: gh resolves against the current dir.
        assert calls == [("pr", "view", "42", "--json", "url,headRefOid")]

    def test_repo_is_passed_through(self, monkeypatch):
        calls: list[tuple] = []

        def fake_run(*args):
            calls.append(args)
            return _completed('{"url": "u", "headRefOid": "cafe"}')

        monkeypatch.setattr(gh, "_run_gh", fake_run)
        gh.resolve_pr("42", "acme/widgets")
        assert calls == [
            ("pr", "view", "42", "--json", "url,headRefOid", "--repo", "acme/widgets")
        ]

    def test_nonzero_exit_names_the_repo(self, monkeypatch):
        monkeypatch.setattr(
            gh, "_run_gh", lambda *a: _completed("", returncode=1, stderr="not found")
        )
        with pytest.raises(GhError, match=r"PR 42 in acme/widgets.*not found"):
            gh.resolve_pr("42", "acme/widgets")

    def test_invalid_json_raises(self, monkeypatch):
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed("not json"))
        with pytest.raises(GhError, match="invalid JSON"):
            gh.resolve_pr("42")

    @pytest.mark.parametrize(
        "payload",
        ["{}", '{"url": "", "headRefOid": "cafe"}', '{"url": 5, "headRefOid": "cafe"}'],
    )
    def test_missing_url_raises(self, monkeypatch, payload):
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="no url"):
            gh.resolve_pr("42")

    @pytest.mark.parametrize(
        "payload",
        [
            '{"url": "u"}',
            '{"url": "u", "headRefOid": ""}',
            '{"url": "u", "headRefOid": 5}',
        ],
    )
    def test_missing_oid_raises(self, monkeypatch, payload):
        # A snooze recorded against an unknown head could hide newer work.
        monkeypatch.setattr(gh, "_run_gh", lambda *a: _completed(payload))
        with pytest.raises(GhError, match="no headRefOid"):
            gh.resolve_pr("42")


class TestFetchPrs:
    """fetch_prs orchestration (mocked _search)."""

    @staticmethod
    def _fake_search(responses: dict):
        def fake(qualifier: str):
            result = responses[qualifier]
            if isinstance(result, Exception):
                raise result
            return result

        return fake

    def test_dedup_merges_roles_and_computes_attention(self, monkeypatch):
        shared = _node(number=1, reviewDecision="REVIEW_REQUIRED")
        authored = _node(number=2, updatedAt="2026-07-16T12:00:00Z")
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search(
                {
                    "author": ("me", [shared, authored], 2),
                    # The same PR also comes back from the second search,
                    # plus a null node that must be skipped.
                    "review-requested": ("me", [None, shared], 1),
                }
            ),
        )
        prs = fetch_prs(["author", "review-requested"])
        assert len(prs) == 2
        by_number = {pr.number: pr for pr in prs}
        assert by_number[1].roles == {"author", "review-requested"}
        assert by_number[2].roles == {"author"}
        # attention_reasons is computed (PR 1 is review-requested + pending
        # — but "author" role comes from the author search: you can't review
        # your own PR in reality; here it just proves the wiring runs).
        assert by_number[1].attention_reasons == {"review"}
        # Sorted newest-updated first: PR 2 (07-16) before PR 1 (07-15).
        assert [pr.number for pr in prs] == [2, 1]

    def test_viewer_propagates_from_any_search(self, monkeypatch):
        node = _node(
            latestReviews={"nodes": [{"author": {"login": "me"}, "state": "APPROVED"}]}
        )
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search(
                {"author": ("", [], 0), "review-requested": ("me", [node], 1)}
            ),
        )
        prs = fetch_prs(["author", "review-requested"])
        assert prs[0].my_review_state == "APPROVED"

    def test_one_failed_search_fails_the_whole_fetch(self, monkeypatch):
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search(
                {
                    "author": ("me", [_node()], 1),
                    "review-requested": GhError(
                        "Search 'review-requested' failed: 502"
                    ),
                }
            ),
        )
        with pytest.raises(GhError, match="(?s)incomplete.*review-requested"):
            fetch_prs(["author", "review-requested"])

    def test_multiple_failures_are_aggregated(self, monkeypatch):
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search(
                {
                    "author": GhError("Search 'author' failed: 502"),
                    "review-requested": GhError(
                        "Search 'review-requested' failed: 401"
                    ),
                }
            ),
        )
        with pytest.raises(GhError) as excinfo:
            fetch_prs(["author", "review-requested"])
        assert "author" in str(excinfo.value)
        assert "review-requested" in str(excinfo.value)

    def test_unexpected_exception_is_aggregated_not_raised_raw(self, monkeypatch):
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search({"author": AttributeError("shape drift")}),
        )
        with pytest.raises(GhError, match="crashed.*shape drift"):
            fetch_prs(["author"])

    def test_missing_viewer_raises(self, monkeypatch):
        # An empty login would silently disable review-state classification.
        monkeypatch.setattr(
            gh, "_search", self._fake_search({"author": ("", [_node()], 1)})
        )
        with pytest.raises(GhError, match="authenticated user"):
            fetch_prs(["author"])

    def test_reviewed_by_without_parsed_review_triggers_warning(self, monkeypatch):
        # The reviewed-by search asserts I reviewed the PR; my review being
        # absent from latestReviews (50-node cap) would silently disable
        # new-commit detection for it — surface the contradiction instead.
        monkeypatch.setattr(
            gh, "_search", self._fake_search({"reviewed-by": ("me", [_node()], 1)})
        )
        warnings: list[str] = []
        fetch_prs(["reviewed-by"], on_warning=warnings.append)
        assert warnings and "acme/widgets#42" in warnings[0]

    def test_reviewed_by_draft_does_not_warn(self, monkeypatch):
        # Drafts never flag, so the missed review changes nothing.
        monkeypatch.setattr(
            gh,
            "_search",
            self._fake_search({"reviewed-by": ("me", [_node(isDraft=True)], 1)}),
        )
        warnings: list[str] = []
        fetch_prs(["reviewed-by"], on_warning=warnings.append)
        assert warnings == []

    def test_truncation_triggers_warning(self, monkeypatch):
        monkeypatch.setattr(
            gh, "_search", self._fake_search({"author": ("me", [_node()], 250)})
        )
        warnings: list[str] = []
        fetch_prs(["author"], on_warning=warnings.append)
        assert warnings and "250" in warnings[0]

    def test_malformed_node_raises_gherror(self, monkeypatch):
        bad = _node()
        del bad["number"]
        monkeypatch.setattr(
            gh, "_search", self._fake_search({"author": ("me", [bad], 1)})
        )
        with pytest.raises(GhError, match="Failed to parse PR data"):
            fetch_prs(["author"])
