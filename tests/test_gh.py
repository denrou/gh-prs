"""Tests for the pure logic in gh_prs.gh: rollup, attention reasons, parsing."""

from gh_prs.gh import PullRequest, _attention_reasons, _rollup_state


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


def _review(login: str, state: str, submitted_at: str) -> dict:
    return {"author": {"login": login}, "state": state, "submittedAt": submitted_at}


class TestRollupState:
    def test_empty_rollup_means_no_checks(self):
        assert _rollup_state([]) == ""

    def test_status_context_failure(self):
        assert _rollup_state([{"state": "FAILURE"}]) == "FAILURE"

    def test_status_context_success(self):
        assert _rollup_state([{"state": "SUCCESS"}]) == "SUCCESS"

    def test_check_run_in_progress_is_pending(self):
        assert _rollup_state([{"status": "IN_PROGRESS"}]) == "PENDING"

    def test_check_run_failing_conclusions(self):
        for conclusion in ("TIMED_OUT", "CANCELLED", "ACTION_REQUIRED"):
            rollup = [{"status": "COMPLETED", "conclusion": conclusion}]
            assert _rollup_state(rollup) == "FAILURE", conclusion

    def test_completed_without_conclusion_is_pending(self):
        assert _rollup_state([{"status": "COMPLETED", "conclusion": ""}]) == "PENDING"

    def test_failure_wins_over_pending(self):
        rollup = [{"status": "IN_PROGRESS"}, {"state": "FAILURE"}]
        assert _rollup_state(rollup) == "FAILURE"

    def test_only_skipped_and_neutral_counts_as_success(self):
        # Deliberate behavior: skipped/neutral checks do not block "ready".
        rollup = [
            {"status": "COMPLETED", "conclusion": "SKIPPED"},
            {"status": "COMPLETED", "conclusion": "NEUTRAL"},
        ]
        assert _rollup_state(rollup) == "SUCCESS"


class TestReviewReason:
    def test_requested_and_pending_needs_review(self):
        pr = _pr(roles={"review-requested"}, review_decision="REVIEW_REQUIRED")
        assert _attention_reasons(pr, [], "me") == {"review"}

    def test_no_review_decision_still_needs_review(self):
        pr = _pr(roles={"review-requested"}, review_decision="")
        assert _attention_reasons(pr, [], "me") == {"review"}

    def test_draft_never_needs_attention(self):
        pr = _pr(is_draft=True, roles={"review-requested"})
        assert _attention_reasons(pr, [], "me") == set()

    def test_latest_review_wins_approved_after_dismissed(self):
        reviews = [
            _review("me", "DISMISSED", "2026-07-01T00:00:00Z"),
            _review("me", "APPROVED", "2026-07-02T00:00:00Z"),
        ]
        pr = _pr(roles={"review-requested"}, review_decision="REVIEW_REQUIRED")
        assert _attention_reasons(pr, reviews, "me") == set()

    def test_latest_review_wins_dismissed_after_approved(self):
        reviews = [
            _review("me", "APPROVED", "2026-07-01T00:00:00Z"),
            _review("me", "DISMISSED", "2026-07-02T00:00:00Z"),
        ]
        pr = _pr(roles={"review-requested"}, review_decision="APPROVED")
        assert _attention_reasons(pr, reviews, "me") == {"review"}

    def test_changes_requested_by_me_suppresses_review(self):
        reviews = [_review("me", "CHANGES_REQUESTED", "2026-07-01T00:00:00Z")]
        pr = _pr(roles={"review-requested"}, review_decision="REVIEW_REQUIRED")
        assert _attention_reasons(pr, reviews, "me") == set()

    def test_conflicting_pr_excluded_from_review(self):
        pr = _pr(
            roles={"review-requested"},
            review_decision="REVIEW_REQUIRED",
            mergeable="CONFLICTING",
        )
        assert _attention_reasons(pr, [], "me") == set()

    def test_other_users_reviews_are_ignored(self):
        reviews = [_review("someone-else", "APPROVED", "2026-07-01T00:00:00Z")]
        pr = _pr(roles={"review-requested"}, review_decision="REVIEW_REQUIRED")
        assert _attention_reasons(pr, reviews, "me") == {"review"}


class TestAuthorReasons:
    def test_approved_green_ci_is_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="SUCCESS")
        assert _attention_reasons(pr, [], "me") == {"ready"}

    def test_approved_without_checks_is_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="")
        assert _attention_reasons(pr, [], "me") == {"ready"}

    def test_approved_with_pending_checks_is_not_ready(self):
        pr = _pr(roles={"author"}, review_decision="APPROVED", checks_state="PENDING")
        assert _attention_reasons(pr, [], "me") == set()

    def test_failing_ci_flagged(self):
        pr = _pr(roles={"author"}, checks_state="FAILURE")
        assert _attention_reasons(pr, [], "me") == {"ci-failed"}

    def test_conflict_flagged(self):
        pr = _pr(roles={"author"}, mergeable="CONFLICTING", checks_state="SUCCESS")
        assert _attention_reasons(pr, [], "me") == {"conflict"}

    def test_conflict_and_ci_failed_are_independent(self):
        pr = _pr(roles={"author"}, mergeable="CONFLICTING", checks_state="FAILURE")
        assert _attention_reasons(pr, [], "me") == {"conflict", "ci-failed"}

    def test_conflicting_approved_green_is_not_ready(self):
        pr = _pr(
            roles={"author"},
            review_decision="APPROVED",
            checks_state="SUCCESS",
            mergeable="CONFLICTING",
        )
        assert _attention_reasons(pr, [], "me") == {"conflict"}


class TestFromJson:
    def test_full_payload(self):
        pr = PullRequest.from_json(
            {
                "number": 42,
                "title": "Fix parser",
                "repository": {"nameWithOwner": "acme/widgets"},
                "author": {"login": "octocat"},
                "url": "https://github.com/acme/widgets/pull/42",
                "updatedAt": "2026-07-15T12:00:00Z",
                "createdAt": "2026-07-01T09:30:00Z",
                "isDraft": True,
            }
        )
        assert pr.number == 42
        assert pr.repo == "acme/widgets"
        assert pr.repo_short == "widgets"
        assert pr.author == "octocat"
        assert pr.is_draft is True
        assert pr.id == "acme/widgets#42"
        assert pr.updated_date == "2026-07-15"

    def test_repository_and_author_as_strings(self):
        pr = PullRequest.from_json(
            {"number": 1, "title": "t", "repository": "acme/widgets", "author": "bob"}
        )
        assert pr.repo == "acme/widgets"
        assert pr.author == "bob"

    def test_missing_optional_fields_default(self):
        pr = PullRequest.from_json({"number": 1, "title": "t"})
        assert pr.url == ""
        assert pr.is_draft is False

    def test_control_characters_stripped_from_title(self):
        pr = PullRequest.from_json(
            {"number": 1, "title": "safe\x1b]0;evil\x07 title\x00"}
        )
        assert pr.title == "safe]0;evil title"
