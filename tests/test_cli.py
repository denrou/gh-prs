"""CLI tests: qualifier selection, --count semantics, failure surfacing, snoozing, escaping."""

import pytest
from rich.console import Console

from gh_prs import cli
from gh_prs.gh import GhError, PullRequest
from gh_prs.snooze import load_snoozes, save_snoozes, snooze_path


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


@pytest.fixture(autouse=True)
def isolated_config(tmp_path, monkeypatch):
    """Point the snooze store at a temp dir so tests never read the user's."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return tmp_path


@pytest.fixture
def fake_backend(monkeypatch):
    """Stub out fetch_prs; records the qualifiers requested."""
    calls: dict = {"qualifiers": None, "prs": []}

    def fake_fetch(qualifiers=None, on_warning=None):
        calls["qualifiers"] = qualifiers
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

    def test_created_view_searches_author_only(self, fake_backend):
        cli.main(["-c"])
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
        def boom(qualifiers=None, on_warning=None):
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
            '"roles"',
            '"attentionReasons"',
            '"updatedAt"',
            '"createdAt"',
        ):
            assert key in out, key
        # Sets are serialized sorted for stable output.
        assert out.index('"author"') < out.index('"review-requested"')


class TestAttentionRendering:
    def test_every_attention_reason_has_a_section(self):
        # A reason without a section would count toward --count yet never
        # render — the PR would be invisible while "needing attention".
        emittable = {"review", "new-commits", "ready", "ci-failed", "conflict"}
        assert emittable == {reason for reason, _, _ in cli._SECTIONS}

    def test_new_commits_section_renders_with_author(self):
        pr = _pr(1, attention_reasons={"new-commits"}, author="octocat")
        console = Console(no_color=True, force_terminal=False, width=200)
        with console.capture() as capture:
            cli._render_attention(console, [pr])
        out = capture.get()
        assert "New commits since your review" in out
        assert "octocat" in out


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


class TestSnoozeFiltering:
    def test_snoozed_pr_hidden_from_attention_view(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" not in captured.out
        assert "1 snoozed PR(s) hidden" in captured.err

    def test_expired_snooze_resurfaces_warns_and_prunes(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="beef", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "snooze expired" in captured.err
        assert load_snoozes() == {}

    def test_snoozed_pr_without_attention_reasons_not_counted_hidden(
        self, fake_backend, capsys
    ):
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, head_ref_oid="cafe")]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--no-color"]) == 0
        assert "snoozed PR(s) hidden" not in capsys.readouterr().err

    def test_attention_count_respects_snooze(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"}),
            _pr(2, attention_reasons={"ready"}),
        ]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--count"]) == 0
        assert capsys.readouterr().out.strip() == "1"

    def test_review_view_ignores_snoozes(self, fake_backend, capsys):
        # Explicit views must stay exact: the PR factually awaits review.
        fake_backend["prs"] = [_pr(1, url=_SNOOZE_URL, head_ref_oid="cafe")]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["-r", "--no-color"]) == 0
        assert "PR 1" in capsys.readouterr().out

    def test_json_ignores_snoozes(self, fake_backend, capsys):
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--json", "--no-color"]) == 0
        assert _SNOOZE_URL in capsys.readouterr().out

    def test_corrupt_store_warns_and_shows_everything(self, fake_backend, capsys):
        # Fail-safe direction: a broken store may only ever show more PRs.
        fake_backend["prs"] = [
            _pr(1, url=_SNOOZE_URL, head_ref_oid="cafe", attention_reasons={"review"})
        ]
        path = snooze_path()
        path.parent.mkdir(parents=True)
        path.write_text("{not json")
        assert cli.main(["--no-color"]) == 0
        captured = capsys.readouterr()
        assert "PR 1" in captured.out
        assert "ignoring snoozes" in captured.err


class TestSnoozeActions:
    def test_snooze_normalizes_url_and_records_head_oid(self, monkeypatch, capsys):
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        assert cli.main(["--snooze", f"{_SNOOZE_URL}/files?diff=split"]) == 0
        assert load_snoozes() == {_SNOOZE_URL: "cafe123"}
        assert "Snoozed" in capsys.readouterr().out

    def test_snooze_lookup_failure_stores_nothing(self, monkeypatch, capsys):
        def boom(url):
            raise GhError("no such PR")

        monkeypatch.setattr(cli, "fetch_pr_head", boom)
        assert cli.main(["--snooze", _SNOOZE_URL]) == 1
        assert load_snoozes() == {}
        assert "no such PR" in capsys.readouterr().err

    def test_snooze_rejects_non_pr_url(self, capsys):
        assert cli.main(["--snooze", "https://github.com/acme/widgets"]) == 1
        assert "not a pull request URL" in capsys.readouterr().err

    @pytest.mark.parametrize("flag", ["--snooze", "--unsnooze"])
    def test_empty_url_is_a_clean_error_not_a_fetch(self, monkeypatch, flag, capsys):
        # An empty (falsy) URL must still dispatch to the action and fail URL
        # validation — not fall through to the fetch-and-render path.
        def boom(qualifiers=None, on_warning=None):
            raise AssertionError("fetch_prs must not run")

        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert cli.main([flag, ""]) == 1
        assert "not a pull request URL" in capsys.readouterr().err

    def test_unsnooze_removes_entry(self, capsys):
        save_snoozes({_SNOOZE_URL: "cafe"})
        assert cli.main(["--unsnooze", _SNOOZE_URL]) == 0
        assert load_snoozes() == {}
        assert "Unsnoozed" in capsys.readouterr().out

    def test_unsnooze_missing_entry_errors(self, capsys):
        assert cli.main(["--unsnooze", _SNOOZE_URL]) == 1
        assert "is not snoozed" in capsys.readouterr().err

    def test_snoozed_lists_entries(self, capsys):
        save_snoozes({_SNOOZE_URL: "cafe123deadbeef"})
        assert cli.main(["--snoozed", "--no-color"]) == 0
        out = capsys.readouterr().out
        assert _SNOOZE_URL in out
        assert "cafe123deadbe" not in out  # oid shown truncated to 12 chars
        assert "cafe123deadb" in out

    def test_snoozed_empty_store_says_so(self, capsys):
        assert cli.main(["--snoozed", "--no-color"]) == 0
        assert "No snoozed PRs" in capsys.readouterr().out

    def test_corrupt_store_is_fatal_for_write_actions(self, monkeypatch, capsys):
        # Writing through a corrupt store would clobber it.
        monkeypatch.setattr(cli, "fetch_pr_head", lambda url: "cafe123")
        path = snooze_path()
        path.parent.mkdir(parents=True)
        path.write_text("{not json")
        assert cli.main(["--snooze", _SNOOZE_URL]) == 1
        assert "Error:" in capsys.readouterr().err
        assert path.read_text() == "{not json"
