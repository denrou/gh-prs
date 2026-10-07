"""CLI tests: qualifier selection, --count semantics, failure surfacing, snoozing, escaping."""

import json
import sys
from datetime import UTC, date, datetime, time, timedelta

import pytest
from rich.console import Console

from gh_prs import cli
from gh_prs.duration import Duration, add_days
from gh_prs.gh import DEFAULT_STALE_AFTER, GhError, PullRequest
from gh_prs.hide import hide_path, load_hidden, save_hidden
from gh_prs.hide import make_entry as make_hide_entry
from gh_prs.snooze import load_snoozes, make_entry, save_snoozes, snooze_path


def _pr(number: int, **overrides) -> PullRequest:
    defaults = dict(
        repo="acme/widgets",
        title=f"PR {number}",
        author="octocat",
        url="",
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
    )
    return PullRequest(number=number, **(defaults | overrides))


def _local_midnight(day: date) -> datetime:
    """The local midnight opening ``day``, as a day-based snooze stores it."""
    return datetime.combine(day, time()).astimezone()


@pytest.fixture(autouse=True)
def isolated_config(tmp_path, monkeypatch):
    """Point the snooze store at a temp dir so tests never read the user's."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


@pytest.fixture
def fake_backend(monkeypatch):
    """Stub out fetch_prs; records the qualifiers requested."""
    calls: dict = {
        "qualifiers": None,
        "prs": [],
        "stale_after": "unset",
        "skip_weekends": "unset",
    }

    def fake_fetch(
        qualifiers=None, on_warning=None, stale_after="unset", skip_weekends="unset"
    ):
        calls["qualifiers"] = qualifiers
        calls["stale_after"] = stale_after
        calls["skip_weekends"] = skip_weekends
        return calls["prs"]

    monkeypatch.setattr(cli, "fetch_prs", fake_fetch)
    return calls


class TestQualifierSelection:
    def test_default_view_searches_author_review_requested_reviewed_by(
        self, fake_backend
    ):
        cli.main([])
        assert fake_backend["qualifiers"] == [
            "author",
            "review-requested",
            "reviewed-by",
        ]

    @pytest.mark.parametrize("flag", ["-c", "--created", "--me"])
    def test_created_view_searches_author_only(self, fake_backend, flag):
        cli.main([flag])
        assert fake_backend["qualifiers"] == ["author"]

    def test_review_view_searches_review_requested_only(self, fake_backend):
        cli.main(["-r"])
        assert fake_backend["qualifiers"] == ["review-requested"]

    def test_all_view_searches_all_qualifiers(self, fake_backend):
        cli.main(["-a"])
        assert fake_backend["qualifiers"] == [
            "author",
            "review-requested",
            "reviewed-by",
            "assignee",
            "involves",
        ]


class TestCountSemantics:
    def test_default_count_only_counts_attention(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, attention_reasons={"review"}),
            _pr(2),
            _pr(3, attention_reasons={"ready", "conflict"}),
        ]
        assert cli.main(["--count"]) == 0
        assert capsys.readouterr().out.strip() == "2"

    def test_single_qualifier_count_uses_fast_path(
        self, monkeypatch, fake_backend, capsys
    ):
        # -c/-r with --count skip node hydration entirely via count_prs.
        counted: list[str] = []

        def fake_count(qualifier):
            counted.append(qualifier)
            return 3

        monkeypatch.setattr(cli, "count_prs", fake_count)
        assert cli.main(["-c", "--count"]) == 0
        assert capsys.readouterr().out.strip() == "3"
        assert counted == ["author"]
        assert fake_backend["qualifiers"] is None  # fetch_prs never called

    def test_all_view_count_still_deduplicates_via_fetch(self, fake_backend, capsys):
        # -a spans several searches whose union must be de-duplicated, so it
        # keeps the full fetch path.
        fake_backend["prs"] = [_pr(1), _pr(2), _pr(3)]
        assert cli.main(["-a", "--count"]) == 0
        assert capsys.readouterr().out.strip() == "3"
        assert fake_backend["qualifiers"] is not None

    def test_fast_count_error_exits_nonzero(self, monkeypatch, fake_backend, capsys):
        def boom(qualifier):
            raise GhError("rate limited")

        monkeypatch.setattr(cli, "count_prs", boom)
        assert cli.main(["-r", "--count"]) == 1
        assert "rate limited" in capsys.readouterr().err


class TestFailureSurfacing:
    def test_fetch_error_prints_error_and_exits_nonzero(self, monkeypatch, capsys):
        def boom(
            qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False
        ):
            raise GhError("token expired")

        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main([]) == 1
        assert "token expired" in capsys.readouterr().err


class TestJsonOutput:
    def test_json_field_contract(self, fake_backend, capsys):
        # --json is a scripting interface; its key names are a contract.
        fake_backend["prs"] = [
            _pr(
                7,
                review_decision="APPROVED",
                my_review_state="DISMISSED",
                review_requested_explicitly=True,
                roles={"review-requested", "author"},
                attention_reasons={"review"},
            )
        ]
        assert cli.main(["--json", "--no-color"]) == 0
        out = capsys.readouterr().out
        for key in (
            '"repo"',
            '"number"',
            '"title"',
            '"author"',
            '"url"',
            '"isDraft"',
            '"reviewDecision"',
            '"checksState"',
            '"mergeable"',
            '"myReviewState"',
            '"myReviewCommit"',
            '"headRefOid"',
            '"reviewRequestedExplicitly"',
            '"changesRequestedCommits"',
            '"hasPendingReviewRequest"',
            '"labels"',
            '"roles"',
            '"attentionReasons"',
            '"updatedAt"',
            '"createdAt"',
        ):
            assert key in out, key
        # Sets are serialized sorted for stable output.
        assert out.index('"author"') < out.index('"review-requested"')

    def test_json_to_a_pipe_is_plain_and_parseable(self, fake_backend, capsys):
        # capsys is not a terminal: no escape codes, whatever the environment
        # (FORCE_COLOR included) says.
        fake_backend["prs"] = [
            _pr(7, labels=("S-Python",), attention_reasons={"review"})
        ]
        assert cli.main(["--json"]) == 0
        out = capsys.readouterr().out
        assert "\x1b[" not in out
        assert json.loads(out) == [cli._to_dict(fake_backend["prs"][0])]

    def test_json_with_no_color_is_plain_even_on_a_terminal(
        self, fake_backend, capsys, monkeypatch
    ):
        # rich's no_color strips colors but keeps bold; --json must mean
        # raw JSON, so the flag routes around rich entirely.
        monkeypatch.setattr(sys.stdout, "isatty", lambda: True)
        fake_backend["prs"] = [_pr(7)]
        assert cli.main(["--json", "--no-color"]) == 0
        out = capsys.readouterr().out
        assert "\x1b[" not in out
        assert json.loads(out)[0]["number"] == 7


class TestStaleThreshold:
    """--stale-after and config.json resolve the 'stale' nudge threshold."""

    def _write_config(self, tmp_path, body):
        path = tmp_path / "gh-prs" / "config.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(body, encoding="utf-8")

    def test_default_is_config_default(self, fake_backend):
        assert cli.main([]) == 0
        assert fake_backend["stale_after"] == DEFAULT_STALE_AFTER

    def test_flag_overrides_default(self, fake_backend):
        assert cli.main(["--stale-after", "5d"]) == 0
        assert fake_backend["stale_after"] == Duration(5, "d")

    def test_config_file_value_used(self, fake_backend, isolated_config):
        self._write_config(isolated_config, '{"stale_after": "1w"}')
        assert cli.main([]) == 0
        assert fake_backend["stale_after"] == Duration(7, "d")

    def test_flag_beats_config_file(self, fake_backend, isolated_config):
        self._write_config(isolated_config, '{"stale_after": "1w"}')
        assert cli.main(["--stale-after", "2d"]) == 0
        assert fake_backend["stale_after"] == Duration(2, "d")

    def test_config_null_disables_nudge(self, fake_backend, isolated_config):
        self._write_config(isolated_config, '{"stale_after": null}')
        assert cli.main([]) == 0
        assert fake_backend["stale_after"] is None

    def test_weekends_count_by_default(self, fake_backend):
        assert cli.main([]) == 0
        assert fake_backend["skip_weekends"] is False

    def test_config_skip_weekends_reaches_the_view(self, fake_backend, isolated_config):
        self._write_config(isolated_config, '{"skip_weekends": true}')
        assert cli.main([]) == 0
        assert fake_backend["skip_weekends"] is True
        assert fake_backend["stale_after"] == DEFAULT_STALE_AFTER

    def test_config_week_is_five_days_when_weekends_are_skipped(
        self, fake_backend, isolated_config
    ):
        self._write_config(
            isolated_config, '{"stale_after": "1w", "skip_weekends": true}'
        )
        assert cli.main([]) == 0
        assert fake_backend["stale_after"] == Duration(5, "d")

    def test_flag_week_follows_the_configured_weekend_policy(
        self, fake_backend, isolated_config
    ):
        # The flag overrides the threshold, not how a 'w' is read: both come
        # out of the same working-week rule.
        self._write_config(isolated_config, '{"skip_weekends": true}')
        assert cli.main(["--stale-after", "1w"]) == 0
        assert fake_backend["stale_after"] == Duration(5, "d")
        assert fake_backend["skip_weekends"] is True

    def test_corrupt_config_keeps_calendar_time(
        self, fake_backend, isolated_config, capsys
    ):
        self._write_config(isolated_config, '{"skip_weekends": "oui"}')
        assert cli.main([]) == 0
        assert fake_backend["skip_weekends"] is False
        assert "ignoring config" in capsys.readouterr().err

    def test_bad_flag_is_a_hard_error(self, fake_backend, capsys):
        assert cli.main(["--stale-after", "soon"]) == 1
        assert "invalid duration" in capsys.readouterr().err
        # It failed before fetching.
        assert fake_backend["stale_after"] == "unset"

    def test_corrupt_config_warns_and_uses_default(
        self, fake_backend, isolated_config, capsys
    ):
        self._write_config(isolated_config, "{not json")
        assert cli.main([]) == 0
        assert fake_backend["stale_after"] == DEFAULT_STALE_AFTER
        assert "ignoring config" in capsys.readouterr().err

    def test_overflowing_flag_is_a_hard_error(self, fake_backend, capsys):
        # An enormous --stale-after must fail cleanly (exit 1) before fetching,
        # not crash with an uncaught OverflowError.
        assert cli.main(["--stale-after", "9" * 100 + "d"]) == 1
        assert "invalid duration" in capsys.readouterr().err
        assert fake_backend["stale_after"] == "unset"

    def test_fast_count_never_reads_config(
        self, monkeypatch, fake_backend, isolated_config, capsys
    ):
        # The -c/-r --count status-bar path must stay exact and quiet: a
        # corrupt config.json must not even be consulted there, so no warning
        # can ever leak into a status bar.
        self._write_config(isolated_config, "{not json")
        loaded: list[int] = []
        real_load = cli.load_config
        monkeypatch.setattr(
            cli,
            "load_config",
            lambda *a, **k: (loaded.append(1), real_load(*a, **k))[1],
        )
        monkeypatch.setattr(cli, "count_prs", lambda q: 0)
        assert cli.main(["-c", "--count"]) == 0
        assert loaded == []  # config never consulted on the fast path
        # No config warning leaks (the spinner's own cursor codes aside).
        assert "config" not in capsys.readouterr().err


class TestAttentionRendering:
    def test_every_attention_reason_has_a_section(self):
        # A reason without a section would count toward --count yet never
        # render — the PR would be invisible while "needing attention".
        emittable = {
            "review",
            "new-commits",
            "ready",
            "ci-failed",
            "conflict",
            "unresolved",
            "stale",
            "stale-draft",
        }
        assert emittable == {reason for reason, _, _ in cli._SECTIONS}

    def test_new_commits_section_renders_with_author(self):
        pr = _pr(1, attention_reasons={"new-commits"}, author="octocat")
        console = Console(no_color=True, force_terminal=False, width=200)
        with console.capture() as capture:
            cli._render_attention(console, [pr])
        out = capture.get()
        assert "New commits since your review" in out
        assert "octocat" in out

    def test_stale_section_renders_without_author(self):
        # 'stale' PRs are my own, so the section omits the author column.
        pr = _pr(1, attention_reasons={"stale"}, author="octocat")
        console = Console(no_color=True, force_terminal=False, width=200)
        with console.capture() as capture:
            cli._render_attention(console, [pr])
        out = capture.get()
        assert "Waiting on review — time to nudge" in out
        assert "octocat" not in out

    def test_stale_draft_section_renders_without_author(self):
        # 'stale-draft' PRs are my own drafts: no author column, and this is
        # the one attention section where the (draft) title prefix shows.
        pr = _pr(1, attention_reasons={"stale-draft"}, author="octocat", is_draft=True)
        console = Console(no_color=True, force_terminal=False, width=200)
        with console.capture() as capture:
            cli._render_attention(console, [pr])
        out = capture.get()
        assert "Drafts gone quiet — finish or mark ready" in out
        assert "(draft)" in out
        assert "octocat" not in out


class TestEscaping:
    def test_title_markup_is_escaped(self):
        pr = _pr(1, title="[link=https://evil.example]click[/link]")
        cell = cli._title_cell(pr)
        # Renders as literal text: escape() backslash-escapes the brackets.
        assert cell.startswith("\\[link=")

    def test_unmatched_closing_tag_does_not_crash_render(self):
        pr = _pr(1, title="broken [/bold] title", attention_reasons={"review"})
        console = Console(no_color=True, force_terminal=False)
        cli._render_attention(console, [pr])  # must not raise MarkupError


_SNOOZE_URL = "https://github.com/acme/widgets/pull/1"


def _entry(oid: str = "cafe", hours: float = 24) -> dict[str, str]:
    """A store entry expiring ``hours`` from now."""
    return make_entry(oid, datetime.now(UTC) + timedelta(hours=hours))


class TestSnoozeFiltering:
    def test_snoozed_pr_hidden_from_attention_view(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "1 snoozed PR(s) hidden" in captured.err

    def test_reason_change_resurfaces_warns_and_prunes(self, fake_backend, capsys):
        # Snoozed while waiting for review; now approved and ready to merge —
        # the head never moved, so only the reason change can resurface it.
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"ready"})
        ]
        save_snoozes(
            {
                _SNOOZE_URL: make_entry(
                    "cafe", datetime.now(UTC) + timedelta(hours=24), ["review"]
                )
            }
        )
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        # The warning line wraps at the console width, so match a word that
        # only the reason-change path emits rather than the whole phrase.
        assert "changed" in captured.err
        assert load_snoozes() == {}

    def test_open_ended_snooze_hides_and_is_not_pruned(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        entry = make_entry("cafe", None, ["review"])
        save_snoozes({_SNOOZE_URL: entry})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "1 snoozed PR(s) hidden" in captured.err
        assert load_snoozes() == {_SNOOZE_URL: entry}

    def test_open_ended_snooze_lifts_on_reason_change(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"ready"})
        ]
        save_snoozes({_SNOOZE_URL: make_entry("cafe", None, ["review"])})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "changed" in captured.err  # wraps at the console width
        assert load_snoozes() == {}

    def test_unchanged_reasons_stay_hidden(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes(
            {
                _SNOOZE_URL: make_entry(
                    "cafe", datetime.now(UTC) + timedelta(hours=24), ["review"]
                )
            }
        )
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "1 snoozed PR(s) hidden" in captured.err

    def test_legacy_entry_without_reasons_ignores_reason_change(
        self, fake_backend, capsys
    ):
        # An entry written before reason-tracking has no "reasons" key; it must
        # keep working on head+window alone and never resurface on a reason change.
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"ready"})
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        assert "PR 1" not in capsys.readouterr().out

    def test_moved_head_resurfaces_warns_and_prunes(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="beef", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "head moved" in captured.err
        assert load_snoozes() == {}

    def test_elapsed_window_resurfaces_warns_and_prunes(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe", hours=-1)})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "window elapsed" in captured.err
        assert load_snoozes() == {}

    def test_elapsed_entry_for_absent_pr_prunes_quietly(self, fake_backend, capsys):
        fake_backend["prs"] = []
        save_snoozes({_SNOOZE_URL: _entry("cafe", hours=-1)})
        assert cli.main(["--no-color"]) == 0
        assert "snooze expired" not in capsys.readouterr().err
        assert load_snoozes() == {}

    def test_snoozed_pr_without_attention_reasons_not_counted_hidden(
        self, fake_backend, capsys
    ):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, head_ref_oid="cafe")]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        assert "snoozed PR(s) hidden" not in capsys.readouterr().err

    def test_attention_count_respects_snooze_and_notes_it(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"}),
            _pr(2, attention_reasons={"ready"}),
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--count"]) == 0
        captured = capsys.readouterr()
        # stdout stays pure for status bars; the hiding is reported on stderr.
        assert captured.out.strip() == "1"
        assert "1 snoozed PR(s) hidden" in captured.err

    def test_review_view_ignores_snoozes(self, fake_backend, capsys):
        # Explicit views must stay exact: the PR factually awaits review.
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, head_ref_oid="cafe")]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["-r", "--no-color"]) == 0
        assert "PR 1" in capsys.readouterr().out

    def test_json_ignores_snoozes(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--json", "--no-color"]) == 0
        assert _SNOOZE_URL in capsys.readouterr().out

    @pytest.mark.parametrize(
        "raw", [b"{not json", b'\xff\xfe{"a": 1}']
    )  # invalid JSON / invalid UTF-8
    def test_corrupt_store_warns_and_shows_everything(self, fake_backend, capsys, raw):
        # Fail-safe direction: a broken store may only ever show more PRs.
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        path = snooze_path()
        path.parent.mkdir(parents=True)
        path.write_bytes(raw)
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "ignoring snoozes" in captured.err


def _mute_config(tmp_path, rules: str) -> None:
    """Write a config.json with the given mute rules into the isolated XDG home."""
    path = tmp_path / "gh-prs" / "config.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'{{"mute": {rules}}}', encoding="utf-8")


def _bot_pr(number: int, **overrides) -> PullRequest:
    defaults = dict(
        author="renovate",
        labels=("C-Deps", "S-Helm"),
        labels_complete=True,
        roles={"review-requested"},
        attention_reasons={"review"},
    )
    return _pr(number, **(defaults | overrides))


class TestMuteFiltering:
    def test_muted_pr_hidden_from_attention_view_and_noted(
        self, fake_backend, isolated_config, capsys
    ):
        _mute_config(
            isolated_config, '[{"author": "renovate", "unless_labels": ["S-Python"]}]'
        )
        fake_backend["prs"] = [
            _bot_pr(1),
            _bot_pr(2, labels=("C-Deps", "S-Python")),
            _pr(3, roles={"review-requested"}, attention_reasons={"review"}),
        ]
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "PR 2" in captured.out
        assert "PR 3" in captured.out
        assert "1 PR(s) muted by config" in captured.err

    def test_attention_count_excludes_muted(
        self, fake_backend, isolated_config, capsys
    ):
        _mute_config(isolated_config, '[{"author": "renovate"}]')
        fake_backend["prs"] = [
            _bot_pr(1),
            _pr(2, roles={"review-requested"}, attention_reasons={"review"}),
        ]
        assert cli.main(["--count", "--no-color"]) == 0
        captured = capsys.readouterr()
        assert captured.out.strip() == "1"
        assert "1 PR(s) muted by config" in captured.err

    def test_muted_pr_without_attention_reasons_is_not_counted(
        self, fake_backend, isolated_config, capsys
    ):
        _mute_config(isolated_config, '[{"author": "renovate"}]')
        fake_backend["prs"] = [_bot_pr(1, attention_reasons=set())]
        assert cli.main(["--no-color"]) == 0
        assert "muted by config" not in capsys.readouterr().err

    def test_review_view_ignores_mute_rules(
        self, fake_backend, isolated_config, capsys
    ):
        _mute_config(isolated_config, '[{"author": "renovate"}]')
        fake_backend["prs"] = [_bot_pr(1)]
        assert cli.main(["-r", "--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "muted" not in captured.err

    def test_json_ignores_mute_rules(self, fake_backend, isolated_config, capsys):
        _mute_config(isolated_config, '[{"author": "renovate"}]')
        fake_backend["prs"] = [_bot_pr(1)]
        assert cli.main(["--json", "--no-color"]) == 0
        captured = capsys.readouterr()
        assert json.loads(captured.out)[0]["number"] == 1
        assert "muted" not in captured.err

    def test_authored_pr_is_never_muted(self, fake_backend, isolated_config, capsys):
        _mute_config(isolated_config, '[{"author": "octocat"}]')
        fake_backend["prs"] = [
            _pr(1, author="octocat", roles={"author"}, attention_reasons={"ready"})
        ]
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "muted" not in captured.err

    def test_incomplete_labels_keep_the_pr_visible(
        self, fake_backend, isolated_config, capsys
    ):
        _mute_config(
            isolated_config, '[{"author": "renovate", "unless_labels": ["S-Python"]}]'
        )
        fake_backend["prs"] = [_bot_pr(1, labels_complete=False)]
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "muted" not in captured.err

    def test_muted_pr_does_not_count_as_snoozed_hidden(
        self, fake_backend, isolated_config, capsys
    ):
        # A lingering snooze on a now-muted PR must not inflate the snooze
        # line: muting is applied first.
        _mute_config(isolated_config, '[{"author": "renovate"}]')
        fake_backend["prs"] = [_bot_pr(1, url=_SNOOZE_URL, head_ref_oid="cafe")]
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "1 PR(s) muted by config" in captured.err
        assert "snoozed PR(s) hidden" not in captured.err

    def test_broken_mute_config_warns_and_mutes_nothing(
        self, fake_backend, isolated_config, capsys
    ):
        # Fail-safe: a bad rule set shows everything rather than guessing
        # which rules the user meant.
        _mute_config(
            isolated_config, '[{"author": "renovate", "unless_label": ["S-Python"]}]'
        )
        fake_backend["prs"] = [_bot_pr(1)]
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "ignoring config" in captured.err
        assert "muted" not in captured.err


_OTHER_SNOOZE_URL = "https://github.com/acme/widgets/pull/2"


class TestSnoozeActions:
    @pytest.fixture(autouse=True)
    def stub_attention_fetch(self, monkeypatch):
        """Snoozing fetches the attention view to capture reasons; stub it so
        these tests neither hit the network nor need a live gh. The two PRs
        the tests snooze are in the view (an open-ended snooze requires it);
        individual tests override cli.fetch_prs to exercise reason capture.
        """
        monkeypatch.setattr(
            cli,
            "fetch_prs",
            lambda qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False: [
                _pr(1, url=_SNOOZE_URL, attention_reasons={"review"}),
                _pr(2, url=_OTHER_SNOOZE_URL, attention_reasons={"review"}),
            ],
        )

    def test_snooze_normalizes_url_and_records_head_oid(self, monkeypatch, capsys):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["snooze", f"{_SNOOZE_URL}/files?diff=split"]) == 0
        entry = load_snoozes()[_SNOOZE_URL]
        assert entry["oid"] == "cafe123"
        assert "Snoozed" in capsys.readouterr().out

    def test_snooze_defaults_to_open_ended(self, monkeypatch, capsys):
        # No --for: no window, lifted only by the head or the reasons.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["snooze", _SNOOZE_URL, "--no-color"]) == 0
        entry = load_snoozes()[_SNOOZE_URL]
        assert entry["until"] is None
        assert entry["reasons"] == ["review"]
        # The line wraps at the console width; match words only this path emits.
        out = capsys.readouterr().out
        assert "Snoozed" in out and "until its head moves" in out

    def test_open_ended_snooze_needs_the_pr_in_the_attention_view(
        self, monkeypatch, capsys
    ):
        # With no window to fall back on, an entry without reasons would hide
        # the PR on its head alone for good — refused per ref, like a bad ref.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(
            cli,
            "fetch_prs",
            lambda qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False: [
                _pr(2, url=_OTHER_SNOOZE_URL, attention_reasons={"review"})
            ],
        )
        assert cli.main(["snooze", _SNOOZE_URL, _OTHER_SNOOZE_URL]) == 1
        assert set(load_snoozes()) == {_OTHER_SNOOZE_URL}
        err = capsys.readouterr().err
        assert "not in your attention view" in err
        assert "pass --for" in err

    def test_open_ended_snooze_aborts_when_attention_state_is_unreadable(
        self, monkeypatch, capsys
    ):
        def boom(
            qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False
        ):
            raise GhError("token expired")

        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main(["snooze", _SNOOZE_URL]) == 1
        assert load_snoozes() == {}
        err = capsys.readouterr().err
        assert "token expired" in err
        assert "pass --for" in err

    def test_windowed_snooze_survives_unreadable_attention_state(
        self, monkeypatch, capsys
    ):
        # The window is a fallback, so a windowed snooze degrades with a
        # warning instead of aborting.
        def boom(
            qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False
        ):
            raise GhError("token expired")

        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "1d"]) == 0
        entry = load_snoozes()[_SNOOZE_URL]
        assert entry["until"] is not None
        assert "reasons" not in entry
        assert "snoozing without it" in capsys.readouterr().err

    def test_snooze_for_one_day_ends_the_next_morning(self, monkeypatch):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "1d"]) == 0
        until = datetime.fromisoformat(load_snoozes()[_SNOOZE_URL]["until"])
        assert until == _local_midnight(add_days(date.today(), 1))

    def test_snooze_for_custom_duration(self, monkeypatch):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "3d"]) == 0
        until = datetime.fromisoformat(load_snoozes()[_SNOOZE_URL]["until"])
        assert until == _local_midnight(add_days(date.today(), 3))

    def test_snooze_for_hours_is_an_exact_span(self, monkeypatch):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "24h"]) == 0
        until = datetime.fromisoformat(load_snoozes()[_SNOOZE_URL]["until"])
        remaining = until - datetime.now(UTC)
        assert timedelta(hours=23) < remaining <= timedelta(hours=24)

    def test_snooze_follows_the_configured_weekend_policy(
        self, monkeypatch, isolated_config
    ):
        # Under skip_weekends a day counts working days and a 'w' is five.
        path = isolated_config / "gh-prs" / "config.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('{"skip_weekends": true}', encoding="utf-8")
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        for text, days in (("1d", 1), ("1w", 5)):
            assert cli.main(["snooze", _SNOOZE_URL, "--for", text]) == 0
            until = datetime.fromisoformat(load_snoozes()[_SNOOZE_URL]["until"])
            expected = add_days(date.today(), days, skip_weekends=True)
            assert until == _local_midnight(expected)

    def test_snooze_invalid_duration_errors_before_lookup(self, monkeypatch, capsys):
        def boom(url):
            raise AssertionError("fetch_pr_head must not run")

        monkeypatch.setattr(cli, "fetch_pr_head", boom)
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "soon"]) == 1
        assert "invalid duration" in capsys.readouterr().err
        assert load_snoozes() == {}

    @pytest.mark.parametrize("shorthand", ["acme/widgets/1", "acme/widgets#1"])
    def test_snooze_rejects_removed_shorthand(self, monkeypatch, shorthand, capsys):
        # owner/repo/123 and owner/repo#123 are gone: use a number + --repo.
        def no_fetch(url):
            raise AssertionError("a rejected shorthand must not be looked up")

        monkeypatch.setattr(cli, "fetch_pr_head", no_fetch)
        assert cli.main(["snooze", shorthand]) == 1
        assert load_snoozes() == {}
        assert "not a pull request URL" in capsys.readouterr().err

    def test_snooze_lookup_failure_stores_nothing(self, monkeypatch, capsys):
        def boom(url):
            raise GhError("no such PR")

        monkeypatch.setattr(cli, "fetch_pr_head", boom)
        assert cli.main(["snooze", _SNOOZE_URL]) == 1
        assert load_snoozes() == {}
        assert "no such PR" in capsys.readouterr().err

    def test_snooze_rejects_non_pr_url(self, capsys):
        assert cli.main(["snooze", "https://github.com/acme/widgets"]) == 1
        assert "not a pull request URL" in capsys.readouterr().err

    @pytest.mark.parametrize("command", ["snooze", "unsnooze"])
    def test_empty_url_is_a_clean_error_not_a_fetch(self, monkeypatch, command, capsys):
        # An empty (falsy) URL must still reach URL validation and fail it —
        # for snooze in particular, not be mistaken for the bare listing form.
        def boom(qualifiers=None, on_warning=None):
            raise AssertionError("fetch_prs must not run")

        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main([command, ""]) == 1
        assert "not a pull request URL" in capsys.readouterr().err

    def test_snooze_bare_number_resolves_via_gh(self, monkeypatch):
        calls: list[tuple] = []

        def fake_resolve(ref, repo):
            calls.append((ref, repo))
            return _SNOOZE_URL, "cafe123"

        def no_fetch(url):
            raise AssertionError("a bare number must not use fetch_pr_head")

        monkeypatch.setattr(cli, "resolve_pr", fake_resolve)
        monkeypatch.setattr(cli, "fetch_pr_head", no_fetch)
        assert cli.main(["snooze", "1"]) == 0
        # No --repo given → resolved against the current directory.
        assert calls == [("1", None)]
        assert load_snoozes()[_SNOOZE_URL]["oid"] == "cafe123"

    def test_snooze_bare_number_honors_repo_flag(self, monkeypatch):
        calls: list[tuple] = []

        def fake_resolve(ref, repo):
            calls.append((ref, repo))
            return _SNOOZE_URL, "cafe123"

        monkeypatch.setattr(cli, "resolve_pr", fake_resolve)
        assert cli.main(["snooze", "7", "-R", "acme/widgets"]) == 0
        assert calls == [("7", "acme/widgets")]

    def test_snooze_multiple_refs(self, monkeypatch):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe")
        assert cli.main(["snooze", _SNOOZE_URL, _OTHER_SNOOZE_URL]) == 0
        assert set(load_snoozes()) == {_SNOOZE_URL, _OTHER_SNOOZE_URL}

    def test_snooze_partial_failure_records_good_refs(self, monkeypatch, capsys):
        # A bad ref is reported and skipped; the good one is still snoozed.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe")
        assert cli.main(["snooze", _SNOOZE_URL, "https://github.com/acme/widgets"]) == 1
        assert set(load_snoozes()) == {_SNOOZE_URL}
        assert "not a pull request URL" in capsys.readouterr().err

    def test_snooze_captures_attention_reasons(self, monkeypatch):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(
            cli,
            "fetch_prs",
            lambda qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False: [
                _pr(
                    1,
                    url=_SNOOZE_URL,
                    head_ref_oid="cafe123",
                    attention_reasons={"review"},
                )
            ],
        )
        assert cli.main(["snooze", _SNOOZE_URL]) == 0
        assert load_snoozes()[_SNOOZE_URL]["reasons"] == ["review"]

    def test_snooze_capture_follows_the_configured_weekend_policy(
        self, monkeypatch, isolated_config
    ):
        # A reason captured under a different clock than later views use
        # would differ on the next run and defeat the snooze.
        path = isolated_config / "gh-prs" / "config.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('{"skip_weekends": true}', encoding="utf-8")
        seen: dict = {}

        def fake_fetch(
            qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False
        ):
            seen["skip_weekends"] = skip_weekends
            return [_pr(1, url=_SNOOZE_URL, attention_reasons={"review"})]

        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(cli, "fetch_prs", fake_fetch)
        assert cli.main(["snooze", _SNOOZE_URL]) == 0
        assert seen["skip_weekends"] is True

    def test_windowed_snooze_of_absent_pr_records_no_reasons(self, monkeypatch):
        # Not in the attention view, so there are no reasons to attach; a
        # windowed entry falls back to the head-and-window rule alone.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        monkeypatch.setattr(
            cli,
            "fetch_prs",
            lambda qualifiers=None, on_warning=None, stale_after=None, skip_weekends=False: [],
        )
        assert cli.main(["snooze", _SNOOZE_URL, "--for", "1d"]) == 0
        assert "reasons" not in load_snoozes()[_SNOOZE_URL]

    def test_unsnooze_removes_entry(self, capsys):
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["unsnooze", _SNOOZE_URL]) == 0
        assert load_snoozes() == {}
        assert "Unsnoozed" in capsys.readouterr().out

    def test_unsnooze_missing_entry_errors(self, capsys):
        assert cli.main(["unsnooze", _SNOOZE_URL]) == 1
        assert "is not snoozed" in capsys.readouterr().err

    def test_unsnooze_bare_number_resolves_via_gh(self, monkeypatch):
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        monkeypatch.setattr(cli, "resolve_pr", lambda ref, repo: (_SNOOZE_URL, "cafe"))
        assert cli.main(["unsnooze", "1"]) == 0
        assert load_snoozes() == {}

    def test_unsnooze_url_needs_no_gh_lookup(self, monkeypatch):
        # A full URL is canonicalized offline, so removal works without network.
        def boom(ref, repo=None):
            raise AssertionError("unsnoozing a URL must not call gh")

        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        monkeypatch.setattr(cli, "resolve_pr", boom)
        assert cli.main(["unsnooze", _SNOOZE_URL]) == 0
        assert load_snoozes() == {}

    def test_unsnooze_multiple_refs(self, capsys):
        other = "https://github.com/acme/widgets/pull/2"
        save_snoozes({_SNOOZE_URL: _entry("cafe"), other: _entry("beef")})
        assert cli.main(["unsnooze", _SNOOZE_URL, other]) == 0
        assert load_snoozes() == {}

    def test_bare_snooze_lists_entries(self, capsys):
        save_snoozes({_SNOOZE_URL: _entry("cafe123deadbeef")})
        assert cli.main(["snooze", "--no-color"]) == 0
        out = capsys.readouterr().out
        assert _SNOOZE_URL in out
        assert "until" in out
        assert "cafe123deadbe" not in out  # oid shown truncated to 12 chars
        assert "cafe123deadb" in out

    def test_bare_snooze_marks_expired_entries(self, capsys):
        save_snoozes({_SNOOZE_URL: _entry("cafe", hours=-1)})
        assert cli.main(["snooze", "--no-color"]) == 0
        assert "expired" in capsys.readouterr().out

    def test_bare_snooze_describes_open_ended_entries(self, capsys):
        save_snoozes({_SNOOZE_URL: make_entry("cafe123deadbeef", None, ["review"])})
        assert cli.main(["snooze", "--no-color"]) == 0
        out = capsys.readouterr().out
        # Wrapped at the console width and dim-styled, so match the pieces.
        assert "until head moving off cafe123deadb" in out
        assert "status changing" in out
        assert "expired" not in out

    def test_snooze_prune_drops_closed_and_keeps_open(self, monkeypatch, capsys):
        save_snoozes(
            {
                _SNOOZE_URL: make_entry("cafe", None, ["review"]),
                _OTHER_SNOOZE_URL: make_entry("beef", None, ["review"]),
            }
        )
        states = {_SNOOZE_URL: "OPEN", _OTHER_SNOOZE_URL: "MERGED"}
        monkeypatch.setattr(
            cli, "fetch_pr", lambda url: _pr(1, url=url, state=states[url])
        )
        assert cli.main(["snooze", "--prune", "--no-color"]) == 0
        assert set(load_snoozes()) == {_SNOOZE_URL}
        out = capsys.readouterr().out
        assert _OTHER_SNOOZE_URL in out and "merged" in out

    def test_snooze_prune_keeps_entries_it_cannot_inspect(self, monkeypatch, capsys):
        save_snoozes(
            {
                _SNOOZE_URL: make_entry("cafe", None, ["review"]),
                _OTHER_SNOOZE_URL: make_entry("beef", None, ["review"]),
            }
        )

        def fake_fetch_pr(url):
            if url == _OTHER_SNOOZE_URL:
                return _pr(2, url=url, state="")
            raise GhError("boom")

        monkeypatch.setattr(cli, "fetch_pr", fake_fetch_pr)
        assert cli.main(["snooze", "--prune", "--no-color"]) == 1
        assert set(load_snoozes()) == {_SNOOZE_URL, _OTHER_SNOOZE_URL}
        captured = capsys.readouterr()
        assert "Nothing to prune" in captured.out
        assert "boom" in captured.err

    def test_snooze_prune_rejects_refs(self, capsys):
        assert cli.main(["snooze", "--prune", _SNOOZE_URL]) == 2
        assert "--prune takes no PR arguments" in capsys.readouterr().err
        assert load_snoozes() == {}

    def test_bare_snooze_empty_store_says_so(self, capsys):
        assert cli.main(["snooze", "--no-color"]) == 0
        assert "No snoozed PRs" in capsys.readouterr().out

    def test_bare_snooze_rejects_dangling_for(self, capsys):
        # "--for without refs" means a forgotten PR ref, not a listing request.
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["snooze", "--for", "3d"]) == 2
        captured = capsys.readouterr()
        assert "--for requires at least one PR" in captured.err
        assert _SNOOZE_URL not in captured.out

    @pytest.mark.parametrize(
        "flag", ["--snooze", "--snooze=1", "--unsnooze", "--snoozed"]
    )
    def test_removed_flags_hint_at_subcommands(self, monkeypatch, flag, capsys):
        def boom(qualifiers=None, on_warning=None):
            raise AssertionError("fetch_prs must not run")

        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main([flag, "1"]) == 2
        assert "replaced by a subcommand" in capsys.readouterr().err

    @pytest.mark.parametrize(
        "flags",
        [["-c"], ["-r"], ["-a"], ["--count"], ["--json"], ["--stale-after", "5d"]],
    )
    def test_view_flags_rejected_with_subcommand(self, flags, capsys):
        assert cli.main([*flags, "snooze", _SNOOZE_URL]) == 2
        assert "do not apply" in capsys.readouterr().err
        assert load_snoozes() == {}

    def test_corrupt_store_is_fatal_for_write_actions(self, monkeypatch, capsys):
        # Writing through a corrupt store would clobber it.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        path = snooze_path()
        path.parent.mkdir(parents=True)
        path.write_text("{not json")
        assert cli.main(["snooze", _SNOOZE_URL]) == 1
        assert "Error:" in capsys.readouterr().err
        assert path.read_text() == "{not json"


_MERGE_URL = "https://github.com/acme/widgets/pull/7"
_MERGE_URL_2 = "https://github.com/acme/widgets/pull/8"


def _mergeable_pr(number: int, url: str, **overrides) -> PullRequest:
    """A PR that passes the merge preflight; someone else's unless overridden."""
    return _pr(
        number,
        **(
            dict(
                url=url,
                state="OPEN",
                review_decision="APPROVED",
                mergeable="MERGEABLE",
                checks_state="SUCCESS",
                head_ref_oid=f"head{number}",
            )
            | overrides
        ),
    )


class TestMergeCommand:
    @pytest.fixture
    def backend(self, monkeypatch):
        """Stub the gh layer: PRs served by URL, actions recorded in order."""
        state: dict = {"prs": {}, "actions": [], "fail": {}}

        def fake_fetch_pr(url):
            try:
                return state["prs"][url]
            except KeyError:
                raise GhError(f"Lookup of {url}: no such pull request") from None

        def fake_approve(url):
            if url in state["fail"].get("approve", ()):
                raise GhError(f"Approving {url} failed: nope")
            state["actions"].append(("approve", url))

        def fake_merge(url, head_oid, *, admin=False, auto=False):
            if url in state["fail"].get("merge", ()):
                raise GhError(f"Merging {url} failed: head commit mismatch")
            state["actions"].append(("merge", url, head_oid, admin, auto))

        def no_resolve(ref, repo=None):
            raise AssertionError("a URL must not be resolved through gh")

        monkeypatch.setattr(cli, "fetch_pr", fake_fetch_pr)
        monkeypatch.setattr(cli, "approve_pr", fake_approve)
        monkeypatch.setattr(cli, "merge_pr", fake_merge)
        monkeypatch.setattr(cli, "resolve_pr", no_resolve)
        return state

    def test_approves_then_merges_someone_elses_pr(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL)
        assert cli.main(["merge", _MERGE_URL, "--no-color"]) == 0
        assert backend["actions"] == [
            ("approve", _MERGE_URL),
            ("merge", _MERGE_URL, "head7", False, False),
        ]
        out = capsys.readouterr().out
        assert "acme/widgets#7: approved" in out
        assert "acme/widgets#7: merged" in out

    def test_own_pr_is_merged_without_approving(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", _MERGE_URL, "--no-color"]) == 0
        assert backend["actions"] == [("merge", _MERGE_URL, "head7", False, False)]
        assert "approved" not in capsys.readouterr().out

    def test_standing_approval_is_not_repeated(self, backend):
        backend["prs"][_MERGE_URL] = _mergeable_pr(
            7, _MERGE_URL, my_review_state="APPROVED"
        )
        assert cli.main(["merge", _MERGE_URL]) == 0
        assert [a[0] for a in backend["actions"]] == ["merge"]

    def test_url_is_canonicalized_offline(self, backend):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", f"{_MERGE_URL}/files?diff=split"]) == 0
        assert backend["actions"] == [("merge", _MERGE_URL, "head7", False, False)]

    def test_bare_number_resolves_via_gh(self, backend, monkeypatch):
        calls: list[tuple] = []

        def fake_resolve(ref, repo):
            calls.append((ref, repo))
            return _MERGE_URL, "head7"

        monkeypatch.setattr(cli, "resolve_pr", fake_resolve)
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", "7", "-R", "acme/widgets"]) == 0
        assert calls == [("7", "acme/widgets")]
        assert [a[0] for a in backend["actions"]] == ["merge"]

    def test_batch_merges_in_order(self, backend):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        backend["prs"][_MERGE_URL_2] = _mergeable_pr(8, _MERGE_URL_2, roles={"author"})
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2]) == 0
        assert [a[1] for a in backend["actions"]] == [_MERGE_URL, _MERGE_URL_2]

    def test_duplicate_refs_merge_once(self, backend):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", _MERGE_URL, f"{_MERGE_URL}/files"]) == 0
        assert len(backend["actions"]) == 1

    # --- preflight: a single blocked ref aborts the batch before any write ---

    def test_blocked_pr_aborts_the_whole_batch(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        backend["prs"][_MERGE_URL_2] = _mergeable_pr(8, _MERGE_URL_2, is_draft=True)
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2, "--no-color"]) == 1
        assert backend["actions"] == []
        err = capsys.readouterr().err
        assert "acme/widgets#8: is a draft" in err
        assert "Nothing was merged" in err

    def test_all_blockers_are_reported_at_once(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(
            7, _MERGE_URL, mergeable="CONFLICTING"
        )
        backend["prs"][_MERGE_URL_2] = _mergeable_pr(8, _MERGE_URL_2, stacked=True)
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2, "--no-color"]) == 1
        err = capsys.readouterr().err
        assert "#7: has merge conflicts" in err
        assert "#8: is stacked" in err

    def test_unknown_pr_aborts_the_batch(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2, "--no-color"]) == 1
        assert backend["actions"] == []
        assert "no such pull request" in capsys.readouterr().err

    def test_non_pr_url_aborts_the_batch(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main(["merge", _MERGE_URL, "https://github.com/acme/widgets"]) == 1
        assert backend["actions"] == []
        assert "not a pull request URL" in capsys.readouterr().err

    def test_bare_merge_needs_a_ref(self, backend, capsys):
        with pytest.raises(SystemExit) as exc:
            cli.main(["merge"])
        assert exc.value.code == 2
        assert backend["actions"] == []

    # --- acting: the first failure stops the batch ---

    def test_merge_failure_stops_the_batch(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        backend["prs"][_MERGE_URL_2] = _mergeable_pr(8, _MERGE_URL_2, roles={"author"})
        backend["fail"]["merge"] = {_MERGE_URL}
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2, "--no-color"]) == 1
        assert backend["actions"] == []
        err = " ".join(capsys.readouterr().err.split())  # rich wraps long lines
        assert "head commit mismatch" in err
        assert "not attempted: acme/widgets#8" in err

    def test_approval_failure_skips_the_merge(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL)
        backend["fail"]["approve"] = {_MERGE_URL}
        assert cli.main(["merge", _MERGE_URL, "--no-color"]) == 1
        assert backend["actions"] == []
        assert "Approving" in capsys.readouterr().err

    def test_second_failure_after_first_success(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        backend["prs"][_MERGE_URL_2] = _mergeable_pr(8, _MERGE_URL_2, roles={"author"})
        backend["fail"]["merge"] = {_MERGE_URL_2}
        assert cli.main(["merge", _MERGE_URL, _MERGE_URL_2, "--no-color"]) == 1
        assert [a[1] for a in backend["actions"]] == [_MERGE_URL]
        captured = capsys.readouterr()
        assert "acme/widgets#7: merged" in captured.out
        assert "not attempted" not in captured.err  # nothing was left over

    # --- flags ---

    def test_admin_passes_through_and_relaxes_preflight(self, backend):
        backend["prs"][_MERGE_URL] = _mergeable_pr(
            7, _MERGE_URL, roles={"author"}, checks_state="FAILURE"
        )
        assert cli.main(["merge", _MERGE_URL, "--admin"]) == 0
        assert backend["actions"] == [("merge", _MERGE_URL, "head7", True, False)]

    def test_auto_passes_through_and_reports_it(self, backend, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(
            7, _MERGE_URL, roles={"author"}, checks_state="PENDING"
        )
        assert cli.main(["merge", _MERGE_URL, "--auto", "--no-color"]) == 0
        assert backend["actions"] == [("merge", _MERGE_URL, "head7", False, True)]
        assert "auto-merge enabled" in capsys.readouterr().out

    def test_admin_and_auto_are_exclusive(self, backend):
        with pytest.raises(SystemExit) as exc:
            cli.main(["merge", _MERGE_URL, "--admin", "--auto"])
        assert exc.value.code == 2
        assert backend["actions"] == []

    @pytest.mark.parametrize(
        "flags",
        [["-c"], ["-r"], ["-a"], ["--count"], ["--json"], ["--stale-after", "5d"]],
    )
    def test_view_flags_rejected(self, backend, flags, capsys):
        backend["prs"][_MERGE_URL] = _mergeable_pr(7, _MERGE_URL, roles={"author"})
        assert cli.main([*flags, "merge", _MERGE_URL]) == 2
        assert "do not apply" in capsys.readouterr().err
        assert backend["actions"] == []


def _hidden_entry() -> dict[str, str]:
    return make_hide_entry(datetime.now(UTC))


class TestHideFiltering:
    def test_hidden_pr_withheld_from_attention_view_and_noted(
        self, fake_backend, capsys
    ):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "1 hidden PR(s) withheld" in captured.err

    def test_hide_survives_new_commits_and_status_changes(self, fake_backend, capsys):
        # The whole point: a snooze would resurface on either; a hide must not.
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        for oid, reasons in [("beef", {"review"}), ("f00d", {"new-commits"})]:
            fake_backend["prs"] = [
                _pr(1, url=_SNOOZE_URL, head_ref_oid=oid, attention_reasons=reasons)
            ]
            assert cli.main(["--no-color"]) == 0
            captured = capsys.readouterr()
            assert "PR 1" not in captured.out
            assert "withheld" in captured.err
        assert _SNOOZE_URL in load_hidden()

    def test_hidden_pr_without_attention_reasons_is_not_counted(
        self, fake_backend, capsys
    ):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL)]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["--no-color"]) == 0
        assert "withheld" not in capsys.readouterr().err

    def test_attention_count_excludes_hidden_and_notes_it(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, attention_reasons={"review"}),
            _pr(2, attention_reasons={"ready"}),
        ]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["--count"]) == 0
        captured = capsys.readouterr()
        assert captured.out.strip() == "1"
        assert "1 hidden PR(s) withheld" in captured.err

    def test_review_view_ignores_hides(self, fake_backend, capsys):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL)]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["-r", "--no-color"]) == 0
        assert "PR 1" in capsys.readouterr().out

    def test_json_ignores_hides(self, fake_backend, capsys):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, attention_reasons={"review"})]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["--json", "--no-color"]) == 0
        assert _SNOOZE_URL in capsys.readouterr().out

    def test_hidden_pr_does_not_count_as_snoozed_hidden(self, fake_backend, capsys):
        # A lingering snooze on a hidden PR must not inflate the snooze line,
        # and the view must not prune it either (no store write).
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        save_snoozes({_SNOOZE_URL: _entry("cafe")})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "hidden PR(s) withheld" in captured.err
        assert "snoozed PR(s) hidden" not in captured.err

    def test_entry_for_absent_pr_is_kept(self, fake_backend, capsys):
        # Absent may mean closed — or merely beyond the search cap.
        fake_backend["prs"] = []
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["--no-color"]) == 0
        assert "withheld" not in capsys.readouterr().err
        assert _SNOOZE_URL in load_hidden()

    @pytest.mark.parametrize("raw", [b"{not json", b'\xff\xfe{"a": 1}'])
    def test_corrupt_store_warns_and_shows_everything(self, fake_backend, capsys, raw):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, attention_reasons={"review"})]
        path = hide_path()
        path.parent.mkdir(parents=True)
        path.write_bytes(raw)
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "ignoring hidden PRs" in captured.err


class TestHideActions:
    def test_hide_url_needs_no_gh_lookup(self, monkeypatch, capsys):
        def boom(*args, **kwargs):
            raise AssertionError("gh must not be called for a full URL")

        monkeypatch.setattr(cli, "resolve_pr", boom)
        monkeypatch.setattr(cli, "fetch_pr_head", boom)
        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main(["hide", f"{_SNOOZE_URL}/files?diff=split"]) == 0
        entry = load_hidden()[_SNOOZE_URL]
        datetime.fromisoformat(entry["since"])  # a parseable timestamp
        assert "Hidden" in capsys.readouterr().out

    def test_hide_bare_number_resolves_via_gh(self, monkeypatch):
        seen: list[tuple[str, str | None]] = []

        def fake_resolve(ref, repo):
            seen.append((ref, repo))
            return _SNOOZE_URL, "cafe"

        monkeypatch.setattr(cli, "resolve_pr", fake_resolve)
        assert cli.main(["hide", "1", "-R", "acme/widgets"]) == 0
        assert seen == [("1", "acme/widgets")]
        assert _SNOOZE_URL in load_hidden()

    def test_hide_multiple_refs(self):
        other = "https://github.com/acme/widgets/pull/2"
        assert cli.main(["hide", _SNOOZE_URL, other]) == 0
        assert set(load_hidden()) == {_SNOOZE_URL, other}

    def test_hide_partial_failure_records_good_refs(self, monkeypatch, capsys):
        def boom(ref, repo):
            raise GhError("no such PR")

        monkeypatch.setattr(cli, "resolve_pr", boom)
        assert cli.main(["hide", _SNOOZE_URL, "999"]) == 1
        assert _SNOOZE_URL in load_hidden()
        assert "no such PR" in capsys.readouterr().err

    def test_hide_rejects_non_pr_url(self, capsys):
        assert cli.main(["hide", "https://github.com/acme/widgets/issues/1"]) == 1
        assert load_hidden() == {}
        assert "not a pull request URL" in capsys.readouterr().err

    def test_rehiding_keeps_the_original_entry(self, capsys):
        original = {"since": "2026-01-01T00:00:00+00:00"}
        save_hidden({_SNOOZE_URL: original})
        assert cli.main(["hide", _SNOOZE_URL, "--no-color"]) == 0
        assert load_hidden()[_SNOOZE_URL] == original
        assert "already hidden" in capsys.readouterr().out

    def test_unhide_removes_entry(self, capsys):
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        assert cli.main(["unhide", _SNOOZE_URL]) == 0
        assert load_hidden() == {}
        assert "Unhidden" in capsys.readouterr().out

    def test_unhide_missing_entry_errors(self, capsys):
        assert cli.main(["unhide", _SNOOZE_URL]) == 1
        assert "is not hidden" in capsys.readouterr().err

    def test_unhide_bare_number_resolves_via_gh(self, monkeypatch):
        save_hidden({_SNOOZE_URL: _hidden_entry()})
        monkeypatch.setattr(cli, "resolve_pr", lambda ref, repo: (_SNOOZE_URL, "cafe"))
        assert cli.main(["unhide", "1"]) == 0
        assert load_hidden() == {}

    def test_bare_hide_lists_entries(self, capsys):
        save_hidden({_SNOOZE_URL: {"since": "2026-10-06T12:00:00+00:00"}})
        assert cli.main(["hide", "--no-color"]) == 0
        out = capsys.readouterr().out
        assert _SNOOZE_URL in out
        assert "since 2026-10-06" in out

    def test_bare_hide_empty_store_says_so(self, capsys):
        assert cli.main(["hide", "--no-color"]) == 0
        assert "No hidden PRs" in capsys.readouterr().out

    def test_corrupt_store_is_fatal_for_the_subcommands(self, capsys):
        path = hide_path()
        path.parent.mkdir(parents=True)
        path.write_text("{not json", encoding="utf-8")
        assert cli.main(["hide", _SNOOZE_URL]) == 1
        assert cli.main(["hide"]) == 1
        assert path.read_text(encoding="utf-8") == "{not json"
        assert "not valid JSON" in capsys.readouterr().err

    def test_prune_drops_closed_and_keeps_open(self, monkeypatch, capsys):
        merged = "https://github.com/acme/widgets/pull/2"
        save_hidden({_SNOOZE_URL: _hidden_entry(), merged: _hidden_entry()})
        states = {_SNOOZE_URL: "OPEN", merged: "MERGED"}
        monkeypatch.setattr(
            cli, "fetch_pr", lambda url: _pr(1, url=url, state=states[url])
        )
        assert cli.main(["hide", "--prune", "--no-color"]) == 0
        assert set(load_hidden()) == {_SNOOZE_URL}
        out = capsys.readouterr().out
        assert merged in out and "merged" in out

    def test_prune_keeps_entries_it_cannot_inspect(self, monkeypatch, capsys):
        # Positive evidence only: an unknown state or a failed lookup keeps
        # the entry (the PR may be temporarily unreachable).
        unknown = "https://github.com/acme/widgets/pull/2"
        save_hidden({_SNOOZE_URL: _hidden_entry(), unknown: _hidden_entry()})

        def fake_fetch_pr(url):
            if url == unknown:
                return _pr(2, url=url, state="")
            raise GhError("boom")

        monkeypatch.setattr(cli, "fetch_pr", fake_fetch_pr)
        assert cli.main(["hide", "--prune", "--no-color"]) == 1
        assert set(load_hidden()) == {_SNOOZE_URL, unknown}
        captured = capsys.readouterr()
        assert "Nothing to prune" in captured.out
        assert "boom" in captured.err

    def test_prune_rejects_refs(self, capsys):
        assert cli.main(["hide", "--prune", _SNOOZE_URL]) == 2
        assert "--prune takes no PR arguments" in capsys.readouterr().err
        assert load_hidden() == {}

    @pytest.mark.parametrize("flag", ["-r", "--count", "--json", "--stale-after=1d"])
    def test_view_flags_rejected_with_hide(self, flag, capsys):
        assert cli.main([flag, "hide", _SNOOZE_URL]) == 2
        assert "do not apply to 'gh prs hide'" in capsys.readouterr().err
        assert load_hidden() == {}
