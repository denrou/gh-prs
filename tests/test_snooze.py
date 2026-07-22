"""Tests for gh_prs.snooze: URL normalization, store I/O, and partitioning."""

import pytest

from gh_prs.gh import PullRequest
from gh_prs.snooze import (
    SnoozeError,
    load_snoozes,
    normalize_pr_url,
    save_snoozes,
    snooze_path,
    split_snoozed,
)

_URL = "https://github.com/acme/widgets/pull/42"


def _pr(url: str = _URL, **overrides) -> PullRequest:
    defaults = dict(
        number=42,
        repo="acme/widgets",
        title="Fix parser",
        author="octocat",
        url=url,
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
    )
    return PullRequest(**(defaults | overrides))


class TestNormalizePrUrl:
    def test_canonical_url_unchanged(self):
        assert normalize_pr_url(_URL) == _URL

    @pytest.mark.parametrize(
        "suffix",
        [
            "/",
            "/files",
            "/files?diff=split",
            "#discussion_r1",
            "?notification_referrer_id=x",
        ],
    )
    def test_browser_navigation_state_stripped(self, suffix):
        assert normalize_pr_url(_URL + suffix) == _URL

    def test_surrounding_whitespace_stripped(self):
        assert normalize_pr_url(f"  {_URL}\n") == _URL

    def test_enterprise_host_accepted(self):
        url = "https://ghe.example.com/acme/widgets/pull/7"
        assert normalize_pr_url(url) == url

    @pytest.mark.parametrize(
        "bad",
        [
            "",
            "acme/widgets#42",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets/issues/42",
            "http://github.com/acme/widgets/pull/42",  # only https is canonical
        ],
    )
    def test_non_pr_url_rejected(self, bad):
        with pytest.raises(SnoozeError):
            normalize_pr_url(bad)


class TestStore:
    def test_missing_file_is_empty_store(self, tmp_path):
        assert load_snoozes(tmp_path / "snooze.json") == {}

    def test_save_load_roundtrip_creates_directories(self, tmp_path):
        path = tmp_path / "deep" / "snooze.json"
        save_snoozes({_URL: "cafe123"}, path)
        assert load_snoozes(path) == {_URL: "cafe123"}

    def test_invalid_json_raises(self, tmp_path):
        path = tmp_path / "snooze.json"
        path.write_text("{not json")
        with pytest.raises(SnoozeError):
            load_snoozes(path)

    @pytest.mark.parametrize("raw", ["[]", '"oid"', '{"url": 1}', '{"url": null}'])
    def test_wrong_shape_raises(self, tmp_path, raw):
        path = tmp_path / "snooze.json"
        path.write_text(raw)
        with pytest.raises(SnoozeError):
            load_snoozes(path)

    def test_default_path_honors_xdg_config_home(self, tmp_path, monkeypatch):
        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
        assert snooze_path() == tmp_path / "gh-prs" / "snooze.json"


class TestSplitSnoozed:
    def test_matching_head_is_hidden(self):
        pr = _pr(head_ref_oid="cafe123")
        visible, hidden, spent = split_snoozed([pr], {_URL: "cafe123"})
        assert (visible, hidden, spent) == ([], [pr], [])

    def test_moved_head_resurfaces_as_spent(self):
        pr = _pr(head_ref_oid="beef456")
        visible, hidden, spent = split_snoozed([pr], {_URL: "cafe123"})
        assert (visible, hidden, spent) == ([pr], [], [_URL])

    def test_unknown_head_resurfaces_as_spent(self):
        # Unknown must never read as "nothing to do": no oid, no hiding.
        pr = _pr(head_ref_oid="")
        visible, hidden, spent = split_snoozed([pr], {_URL: "cafe123"})
        assert (visible, hidden, spent) == ([pr], [], [_URL])

    def test_empty_stored_oid_resurfaces_as_spent(self):
        pr = _pr(head_ref_oid="")
        visible, hidden, spent = split_snoozed([pr], {_URL: ""})
        assert (visible, hidden, spent) == ([pr], [], [_URL])

    def test_unsnoozed_pr_stays_visible(self):
        pr = _pr(head_ref_oid="cafe123")
        visible, hidden, spent = split_snoozed(
            [pr], {"https://github.com/x/y/pull/1": "cafe123"}
        )
        assert (visible, hidden, spent) == ([pr], [], [])

    def test_entry_for_absent_pr_is_not_spent(self):
        # The PR may be closed or beyond the search cap; its snooze survives.
        snoozes = {"https://github.com/x/y/pull/1": "cafe123"}
        visible, hidden, spent = split_snoozed([], snoozes)
        assert (visible, hidden, spent) == ([], [], [])
        assert snoozes == {"https://github.com/x/y/pull/1": "cafe123"}
