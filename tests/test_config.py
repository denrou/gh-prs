"""Tests for gh_prs.config: loading user settings and their fail-safe defaults."""

from datetime import timedelta

import pytest

from gh_prs.config import Config, ConfigError, config_path, load_config, week_days
from gh_prs.gh import DEFAULT_STALE_AFTER
from gh_prs.mute import MuteRule


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


class TestMuteRules:
    def _load(self, tmp_path, body: str):
        path = tmp_path / "config.json"
        path.write_text(f'{{"mute": {body}}}', encoding="utf-8")
        return load_config(path)

    def test_absent_key_means_no_rules(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text("{}", encoding="utf-8")
        assert load_config(path).mute == ()

    def test_empty_list_means_no_rules(self, tmp_path):
        assert self._load(tmp_path, "[]").mute == ()

    def test_author_only_rule(self, tmp_path):
        rules = self._load(tmp_path, '[{"author": "renovate"}]').mute
        assert rules == (MuteRule(author="renovate"),)
        assert rules[0].unless_labels == frozenset()

    def test_rule_with_exempting_labels_keeps_file_order(self, tmp_path):
        body = (
            '[{"author": "renovate", "unless_labels": ["S-Python", "S-Rust"]},'
            ' {"author": "dependabot"}]'
        )
        assert self._load(tmp_path, body).mute == (
            MuteRule(
                author="renovate", unless_labels=frozenset({"S-Python", "S-Rust"})
            ),
            MuteRule(author="dependabot"),
        )

    def test_author_is_stripped(self, tmp_path):
        assert (
            self._load(tmp_path, '[{"author": " renovate "}]').mute[0].author
            == "renovate"
        )

    def test_rules_coexist_with_the_staleness_settings(self, tmp_path):
        path = tmp_path / "config.json"
        path.write_text(
            '{"stale_after": "1w", "skip_weekends": true, "mute": [{"author": "bot"}]}',
            encoding="utf-8",
        )
        config = load_config(path)
        assert config.stale_after == timedelta(days=5)
        assert config.mute == (MuteRule(author="bot"),)

    @pytest.mark.parametrize(
        "body", ['{"author": "renovate"}', '"renovate"', "null", "1"]
    )
    def test_non_list_raises(self, tmp_path, body):
        with pytest.raises(ConfigError, match="'mute' must be a list"):
            self._load(tmp_path, body)

    @pytest.mark.parametrize("body", ['["renovate"]', "[null]", "[[]]"])
    def test_non_object_rule_raises(self, tmp_path, body):
        with pytest.raises(ConfigError, match=r"'mute\[0\]' must be an object"):
            self._load(tmp_path, body)

    @pytest.mark.parametrize(
        "body",
        [
            "[{}]",
            '[{"author": ""}]',
            '[{"author": "  "}]',
            '[{"author": 1}]',
            '[{"author": null}]',
        ],
    )
    def test_missing_or_blank_author_raises(self, tmp_path, body):
        with pytest.raises(ConfigError, match="needs a non-empty 'author'"):
            self._load(tmp_path, body)

    @pytest.mark.parametrize(
        "labels", ['"S-Python"', "null", "[1]", '[""]', '["S-Python", null]']
    )
    def test_malformed_unless_labels_raises(self, tmp_path, labels):
        body = f'[{{"author": "renovate", "unless_labels": {labels}}}]'
        with pytest.raises(ConfigError, match="'unless_labels' must be a list"):
            self._load(tmp_path, body)

    def test_unknown_key_raises(self, tmp_path):
        # A misspelt "unless_label" would otherwise silently turn an
        # exempting rule into an unconditional one.
        body = '[{"author": "renovate", "unless_label": ["S-Python"]}]'
        with pytest.raises(
            ConfigError, match=r"'mute\[0\]' has unknown key\(s\): unless_label"
        ):
            self._load(tmp_path, body)

    def test_error_names_the_offending_rule(self, tmp_path):
        body = '[{"author": "renovate"}, {"author": ""}]'
        with pytest.raises(ConfigError, match=r"'mute\[1\]'"):
            self._load(tmp_path, body)


class TestWeekDays:
    """A 'w' is five days in working time, seven on the calendar."""

    def test_calendar_week(self):
        assert week_days(False) == 7

    def test_working_week(self):
        assert week_days(True) == 5
