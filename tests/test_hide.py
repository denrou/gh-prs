"""Tests for gh_prs.hide: store I/O and partitioning."""

from datetime import UTC, datetime

import pytest

from gh_prs.gh import PullRequest
from gh_prs.hide import (
    HideError,
    hide_path,
    load_hidden,
    make_entry,
    save_hidden,
    split_hidden,
)

_URL = "https://github.com/acme/widgets/pull/42"
_NOW = datetime(2026, 10, 6, 12, 0, 0, tzinfo=UTC)


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


class TestStore:
    def test_missing_file_is_empty_store(self, tmp_path):
        assert load_hidden(tmp_path / "hidden.json") == {}

    def test_save_load_roundtrip_creates_directories(self, tmp_path):
        path = tmp_path / "nested" / "hidden.json"
        save_hidden({_URL: make_entry(_NOW)}, path)
        assert load_hidden(path) == {_URL: {"since": "2026-10-06T12:00:00+00:00"}}

    def test_invalid_json_raises(self, tmp_path):
        path = tmp_path / "hidden.json"
        path.write_text("{not json", encoding="utf-8")
        with pytest.raises(HideError, match="not valid JSON"):
            load_hidden(path)

    def test_invalid_utf8_raises_hide_error(self, tmp_path):
        path = tmp_path / "hidden.json"
        path.write_bytes(b'\xff\xfe{"a": 1}')
        with pytest.raises(HideError, match="not valid UTF-8"):
            load_hidden(path)

    @pytest.mark.parametrize(
        "raw",
        [
            "[]",
            '{"url": "2026-10-06"}',
            '{"url": {}}',
            '{"url": {"since": 1}}',
        ],
    )
    def test_wrong_shape_raises(self, tmp_path, raw):
        path = tmp_path / "hidden.json"
        path.write_text(raw, encoding="utf-8")
        with pytest.raises(HideError, match="unexpected shape"):
            load_hidden(path)

    def test_default_path_honors_xdg_config_home(self, tmp_path, monkeypatch):
        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
        assert hide_path() == tmp_path / "gh-prs" / "hidden.json"


class TestSplitHidden:
    def test_hidden_url_is_withheld_whatever_its_state(self):
        # No head oid, no reasons: a hide is a standing decision, so nothing
        # about the PR itself can lift it.
        pr = _pr(head_ref_oid="beef", attention_reasons={"review", "new-commits"})
        visible, hidden = split_hidden([pr], {_URL: make_entry(_NOW)})
        assert visible == []
        assert hidden == [pr]

    def test_other_prs_stay_visible(self):
        other = _pr("https://github.com/acme/widgets/pull/7")
        visible, hidden = split_hidden([other], {_URL: make_entry(_NOW)})
        assert visible == [other]
        assert hidden == []

    def test_empty_store_withholds_nothing(self):
        pr = _pr()
        assert split_hidden([pr], {}) == ([pr], [])
