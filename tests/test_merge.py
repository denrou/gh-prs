"""Merge policy tests: what blocks a merge, and when to approve first."""

import pytest

from gh_prs.gh import PullRequest
from gh_prs.merge import merge_blockers, should_approve


def _pr(**overrides) -> PullRequest:
    """A PR that passes preflight outright unless an override says otherwise."""
    defaults = dict(
        number=1,
        repo="acme/widgets",
        title="Fix parser",
        author="octocat",
        url="https://github.com/acme/widgets/pull/1",
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
        state="OPEN",
        review_decision="APPROVED",
        mergeable="MERGEABLE",
        checks_state="SUCCESS",
        head_ref_oid="cafe",
        stacked=False,
    )
    return PullRequest(**(defaults | overrides))


class TestShouldApprove:
    def test_someone_elses_unreviewed_pr(self):
        assert should_approve(_pr()) is True

    def test_own_pr_is_never_approved(self):
        # GitHub rejects self-approval; attempting it would fail the batch.
        assert should_approve(_pr(roles={"author"})) is False

    def test_standing_approval_is_not_repeated(self):
        assert should_approve(_pr(my_review_state="APPROVED")) is False

    @pytest.mark.parametrize("state", ["DISMISSED", "COMMENTED", "CHANGES_REQUESTED"])
    def test_non_approving_review_is_superseded(self, state):
        assert should_approve(_pr(my_review_state=state)) is True


class TestMergeBlockers:
    def test_clean_pr_has_none(self):
        assert merge_blockers(_pr()) == []

    def test_no_checks_configured_is_fine(self):
        assert merge_blockers(_pr(checks_state="")) == []

    def test_no_review_requirement_is_fine(self):
        assert merge_blockers(_pr(review_decision="")) == []

    # --- always blocking, whatever the flags ---

    @pytest.mark.parametrize("flags", [{}, {"admin": True}, {"auto": True}])
    @pytest.mark.parametrize(
        "override, phrase",
        [
            ({"state": "MERGED"}, "is merged"),
            ({"state": "CLOSED"}, "is closed"),
            ({"state": ""}, "unknown state"),
            ({"is_draft": True}, "is a draft"),
            ({"mergeable": "CONFLICTING"}, "merge conflicts"),
            ({"mergeable": "UNKNOWN"}, "unknown mergeability"),
            ({"mergeable": ""}, "unknown mergeability"),
            ({"head_ref_oid": ""}, "no known head commit"),
            ({"stacked": True}, "stacked on another open PR"),
        ],
    )
    def test_always_blocking(self, flags, override, phrase):
        blockers = merge_blockers(_pr(**override), **flags)
        assert any(phrase in b for b in blockers), blockers

    def test_blockers_accumulate(self):
        blockers = merge_blockers(_pr(is_draft=True, mergeable="CONFLICTING"))
        assert len(blockers) == 2

    # --- checks ---

    def test_failing_checks_block(self):
        assert merge_blockers(_pr(checks_state="FAILURE")) == ["has failing checks"]

    def test_pending_checks_block_and_point_at_auto(self):
        (blocker,) = merge_blockers(_pr(checks_state="PENDING"))
        assert "still running" in blocker
        assert "--auto" in blocker

    @pytest.mark.parametrize("checks", ["FAILURE", "PENDING"])
    def test_auto_leaves_checks_to_github(self, checks):
        assert merge_blockers(_pr(checks_state=checks), auto=True) == []

    @pytest.mark.parametrize("checks", ["FAILURE", "PENDING"])
    def test_admin_bypasses_checks(self, checks):
        assert merge_blockers(_pr(checks_state=checks), admin=True) == []

    # --- review decision ---

    def test_changes_requested_blocks(self):
        assert merge_blockers(_pr(review_decision="CHANGES_REQUESTED")) == [
            "has changes requested"
        ]

    def test_changes_requested_still_blocks_with_auto(self):
        # A human objected; auto-merge must not be armed over their head.
        assert merge_blockers(_pr(review_decision="CHANGES_REQUESTED"), auto=True) == [
            "has changes requested"
        ]

    def test_admin_bypasses_changes_requested(self):
        assert (
            merge_blockers(_pr(review_decision="CHANGES_REQUESTED"), admin=True) == []
        )

    def test_review_required_passes_when_the_viewer_will_approve(self):
        # Not the author, no standing approval: the approve step may satisfy it.
        assert merge_blockers(_pr(review_decision="REVIEW_REQUIRED")) == []

    def test_review_required_blocks_own_pr(self):
        assert merge_blockers(
            _pr(review_decision="REVIEW_REQUIRED", roles={"author"})
        ) == ["is still awaiting review"]

    def test_review_required_blocks_when_already_approved_by_viewer(self):
        # The viewer's approval is already counted and it wasn't enough.
        assert merge_blockers(
            _pr(review_decision="REVIEW_REQUIRED", my_review_state="APPROVED")
        ) == ["is still awaiting review"]

    def test_auto_waits_for_review(self):
        assert (
            merge_blockers(
                _pr(review_decision="REVIEW_REQUIRED", roles={"author"}), auto=True
            )
            == []
        )

    def test_admin_bypasses_review_requirement(self):
        assert (
            merge_blockers(
                _pr(review_decision="REVIEW_REQUIRED", roles={"author"}), admin=True
            )
            == []
        )
