"""Tests for gh_prs.snooze: normalization, durations, store I/O, and partitioning."""

from datetime import UTC, datetime, timedelta

import pytest

from gh_prs.gh import PullRequest
from gh_prs.snooze import (
    SnoozeError,
    is_expired,
    load_snoozes,
    make_entry,
    normalize_pr_url,
    parse_duration,
    save_snoozes,
    snooze_path,
    split_snoozed,
)

_URL = "https://github.com/acme/widgets/pull/42"
_NOW = datetime(2026, 7, 22, 12, 0, 0, tzinfo=UTC)


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


def _entry(oid: str = "cafe123", hours: float = 24) -> dict[str, str]:
    """A store entry expiring ``hours`` after the fixed test clock."""
    return make_entry(oid, _NOW, timedelta(hours=hours))


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

    @pytest.mark.parametrize("shorthand", ["acme/widgets/42", "acme/widgets#42"])
    def test_shorthand_expands_to_github_url(self, shorthand):
        assert normalize_pr_url(shorthand) == _URL

    @pytest.mark.parametrize(
        "bad",
        [
            "",
            "acme/widgets",
            "acme/widgets/1a",
            "acme/widgets/pull/42",  # four segments: neither shorthand nor URL
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets/issues/42",
            "http://github.com/acme/widgets/pull/42",  # only https is canonical
            # Garbage fused to the number is a typo, not navigation state;
            # truncating it would snooze the wrong PR.
            "https://github.com/acme/widgets/pull/42abc",
        ],
    )
    def test_non_pr_reference_rejected(self, bad):
        with pytest.raises(SnoozeError):
            normalize_pr_url(bad)

    def test_multi_digit_number_not_truncated(self):
        url = "https://github.com/acme/widgets/pull/423"
        assert normalize_pr_url(url + "/files") == url


class TestParseDuration:
    @pytest.mark.parametrize(
        ("text", "expected"),
        [
            ("12h", timedelta(hours=12)),
            ("24h", timedelta(hours=24)),
            ("3d", timedelta(days=3)),
            ("1w", timedelta(weeks=1)),
            (" 2D ", timedelta(days=2)),  # case- and whitespace-insensitive
        ],
    )
    def test_valid_durations(self, text, expected):
        assert parse_duration(text) == expected

    @pytest.mark.parametrize(
        "bad", ["", "h", "12", "12m", "1.5d", "-1d", "0h", "forever"]
    )
    def test_invalid_durations_rejected(self, bad):
        with pytest.raises(SnoozeError):
            parse_duration(bad)


class TestStore:
    def test_missing_file_is_empty_store(self, tmp_path):
        assert load_snoozes(tmp_path / "snooze.json") == {}

    def test_save_load_roundtrip_creates_directories(self, tmp_path):
        path = tmp_path / "deep" / "snooze.json"
        save_snoozes({_URL: _entry()}, path)
        assert load_snoozes(path) == {_URL: _entry()}

    def test_invalid_json_raises(self, tmp_path):
        path = tmp_path / "snooze.json"
        path.write_text("{not json")
        with pytest.raises(SnoozeError):
            load_snoozes(path)

    def test_invalid_utf8_raises_snooze_error(self, tmp_path):
        # UnicodeDecodeError is a ValueError, not an OSError — it must still
        # surface as SnoozeError so callers can degrade instead of crashing.
        path = tmp_path / "snooze.json"
        path.write_bytes(b'\xff\xfe{"a": 1}')
        with pytest.raises(SnoozeError):
            load_snoozes(path)

    @pytest.mark.parametrize(
        "raw",
        [
            "[]",
            '"oid"',
            '{"url": 1}',
            '{"url": "bare-oid"}',  # the pre-expiry flat format
            '{"url": {"oid": "cafe"}}',  # missing until
            '{"url": {"oid": "cafe", "until": 5}}',
        ],
    )
    def test_wrong_shape_raises(self, tmp_path, raw):
        path = tmp_path / "snooze.json"
        path.write_text(raw)
        with pytest.raises(SnoozeError):
            load_snoozes(path)

    def test_default_path_honors_xdg_config_home(self, tmp_path, monkeypatch):
        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
        assert snooze_path() == tmp_path / "gh-prs" / "snooze.json"


class TestIsExpired:
    def test_future_timestamp_is_live(self):
        assert not is_expired(_entry(hours=1), _NOW)

    def test_past_timestamp_is_expired(self):
        assert is_expired(_entry(hours=-1), _NOW)

    @pytest.mark.parametrize(
        "until",
        ["", "not-a-date", "2026-07-23T12:00:00"],  # last one is naive
    )
    def test_uncomparable_timestamp_counts_as_expired(self, until):
        # Fail-safe: a timestamp we can't trust must show the PR.
        assert is_expired({"oid": "cafe", "until": until}, _NOW)

    def test_naive_now_is_a_caller_bug_and_raises(self):
        # A naive `now` would otherwise silently expire every entry in the
        # store; it must raise loudly instead.
        with pytest.raises(TypeError):
            is_expired(_entry(hours=1), datetime(2026, 7, 22, 12, 0, 0))


class TestSplitSnoozed:
    def test_matching_head_within_window_is_hidden(self):
        pr = _pr(head_ref_oid="cafe123")
        visible, hidden, dead = split_snoozed([pr], {_URL: _entry()}, _NOW)
        assert (visible, hidden, dead) == ([], [pr], {})

    def test_moved_head_resurfaces_as_dead(self):
        pr = _pr(head_ref_oid="beef456")
        visible, hidden, dead = split_snoozed([pr], {_URL: _entry()}, _NOW)
        assert (visible, hidden) == ([pr], [])
        assert dead == {_URL: "head moved since you snoozed it"}

    def test_elapsed_window_resurfaces_as_dead(self):
        pr = _pr(head_ref_oid="cafe123")
        visible, hidden, dead = split_snoozed([pr], {_URL: _entry(hours=-1)}, _NOW)
        assert (visible, hidden) == ([pr], [])
        assert dead == {_URL: "snooze window elapsed"}

    def test_unknown_head_resurfaces_as_dead(self):
        # Unknown must never read as "nothing to do": no oid, no hiding.
        pr = _pr(head_ref_oid="")
        visible, hidden, dead = split_snoozed([pr], {_URL: _entry()}, _NOW)
        assert (visible, hidden) == ([pr], [])
        assert _URL in dead

    def test_empty_stored_oid_resurfaces_as_dead(self):
        pr = _pr(head_ref_oid="")
        visible, hidden, dead = split_snoozed([pr], {_URL: _entry(oid="")}, _NOW)
        assert (visible, hidden) == ([pr], [])
        assert _URL in dead

    def test_unsnoozed_pr_stays_visible(self):
        pr = _pr(head_ref_oid="cafe123")
        other = {"https://github.com/x/y/pull/1": _entry()}
        visible, hidden, dead = split_snoozed([pr], other, _NOW)
        assert (visible, hidden, dead) == ([pr], [], {})

    def test_live_entry_for_absent_pr_is_kept(self):
        # The PR may be closed or beyond the search cap; its snooze survives
        # while the window is open.
        snoozes = {"https://github.com/x/y/pull/1": _entry()}
        visible, hidden, dead = split_snoozed([], snoozes, _NOW)
        assert (visible, hidden, dead) == ([], [], {})
        assert snoozes == {"https://github.com/x/y/pull/1": _entry()}

    def test_elapsed_entry_for_absent_pr_is_dead(self):
        # A time-expired entry hides nothing; pruning it caps store growth.
        snoozes = {"https://github.com/x/y/pull/1": _entry(hours=-1)}
        visible, hidden, dead = split_snoozed([], snoozes, _NOW)
        assert (visible, hidden) == ([], [])
        assert dead == {"https://github.com/x/y/pull/1": "snooze window elapsed"}
