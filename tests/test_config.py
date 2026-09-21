"""Tests for gh_prs.config: loading user settings and their fail-safe defaults."""

from datetime import timedelta

import pytest

from gh_prs.config import Config, ConfigError, config_path, load_config, week_days
from gh_prs.gh import DEFAULT_STALE_AFTER


class TestConfigPath:
    def test_uses_xdg_config_home(self, monkeypatch, tmp_path):
        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
        assert config_path() == tmp_path / "gh-prs" / "config.json"

    def test_falls_back_to_home_config(self, monkeypatch, tmp_path):
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
        monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
        assert config_path() == tmp_path / ".config" / "gh-prs" / "config.json"


class TestLoadConfig:
    def test_missing_file_is_all_defaults(self, tmp_path):
        assert load_config(tmp_path / "nope.json") == Config()
        assert load_config(tmp_path / "nope.json").stale_after == DEFAULT_STALE_AFTER

    def test_absent_key_keeps_default(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text("{}", encoding="utf-8")
        assert load_config(path).stale_after == DEFAULT_STALE_AFTER

    def test_valid_duration_is_parsed(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": "1w"}', encoding="utf-8")
        assert load_config(path).stale_after == timedelta(weeks=1)

    def test_null_disables_the_nudge(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": null}', encoding="utf-8")
        assert load_config(path).stale_after is None

    def test_weekends_count_by_default(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": "3d"}', encoding="utf-8")
        assert load_config(path).skip_weekends is False

    def test_skip_weekends_is_read(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"skip_weekends": true}', encoding="utf-8")
        assert load_config(path).skip_weekends is True

    def test_skipping_weekends_makes_a_week_five_days(self, tmp_path):
        # "1w" stays a same-weekday anniversary rather than stretching to
        # seven working days (nine calendar ones).
        path = tmp_path / "config.json"
        path.write_text(
            '{"stale_after": "1w", "skip_weekends": true}', encoding="utf-8"
        )
        assert load_config(path).stale_after == timedelta(days=5)

    @pytest.mark.parametrize("value", ['"yes"', "1", "null", "[]"])
    def test_non_boolean_skip_weekends_raises(self, tmp_path, value):
        # 1 in particular: bool subclasses int, so a bare isinstance check
        # would have accepted it and hidden the user's mistake.
        path = tmp_path / "config.json"
        path.write_text(f'{{"skip_weekends": {value}}}', encoding="utf-8")
        with pytest.raises(ConfigError, match="'skip_weekends' must be true or false"):
            load_config(path)

    def test_invalid_duration_raises(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": "soon"}', encoding="utf-8")
        with pytest.raises(ConfigError, match="invalid 'stale_after'"):
            load_config(path)

    def test_zero_duration_raises(self, tmp_path):
        # parse_duration rejects a zero window; that must surface as ConfigError.
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": "0d"}', encoding="utf-8")
        with pytest.raises(ConfigError, match="invalid 'stale_after'"):
            load_config(path)

    def test_non_string_duration_raises(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text('{"stale_after": 3}', encoding="utf-8")
        with pytest.raises(ConfigError, match="must be a duration string"):
            load_config(path)

    def test_overflowing_duration_raises_not_crashes(self, tmp_path):
        # An enormous but syntactically valid duration must surface as
        # ConfigError (via parse_duration), never escape as OverflowError.
        path = tmp_path / "config.json"
        path.write_text(f'{{"stale_after": "{"9" * 100}d"}}', encoding="utf-8")
        with pytest.raises(ConfigError, match="invalid 'stale_after'"):
            load_config(path)

    def test_unreadable_path_raises(self, tmp_path):
        # A non-FileNotFoundError OSError (here IsADirectoryError) degrades to
        # ConfigError rather than propagating.
        with pytest.raises(ConfigError, match="cannot read"):
            load_config(tmp_path)  # a directory, not a file

    def test_invalid_json_raises(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text("{not json", encoding="utf-8")
        with pytest.raises(ConfigError, match="not valid JSON"):
            load_config(path)

    def test_non_object_json_raises(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text("[1, 2, 3]", encoding="utf-8")
        with pytest.raises(ConfigError, match="unexpected shape"):
            load_config(path)

    def test_non_utf8_raises(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_bytes(b"\xff\xfe{}")
        with pytest.raises(ConfigError, match="not valid UTF-8"):
            load_config(path)


class TestWeekDays:
    """A 'w' is five days in working time, seven on the calendar."""

    def test_calendar_week(self):
        assert week_days(False) == 7

    def test_working_week(self):
        assert week_days(True) == 5
