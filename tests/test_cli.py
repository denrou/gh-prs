"""Tests for CLI behavior: qualifier selection, --count semantics, escaping."""

import sys

import pytest

from gh_prs import cli
from gh_prs.gh import GhError, PullRequest


def _pr(number: int, *, attention: set[str] | None = None, **overrides) -> PullRequest:
    defaults = dict(
        repo="acme/widgets",
        title=f"PR {number}",
        author="octocat",
        url="",
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
    )
    defaults.update(overrides)
    pr = PullRequest(number=number, **defaults)
    pr.attention_reasons = attention or set()
    return pr


@pytest.fixture
def fake_backend(monkeypatch):
    """Stub out fetch_prs; records the qualifiers requested."""
    calls: dict = {"qualifiers": None, "prs": []}

    def fake_fetch(qualifiers=None, on_warning=None):
        calls["qualifiers"] = qualifiers
        return calls["prs"]

    monkeypatch.setattr(cli, "fetch_prs", fake_fetch)
    return calls


def _run(monkeypatch, argv: list[str]) -> int:
    monkeypatch.setattr(sys, "argv", ["gh-prs", *argv])
    return cli.main()


class TestQualifierSelection:
    def test_default_view_searches_author_and_review_requested(
        self, monkeypatch, fake_backend
    ):
        _run(monkeypatch, [])
        assert fake_backend["qualifiers"] == ["author", "review-requested"]

    def test_created_view_searches_author_only(self, monkeypatch, fake_backend):
        _run(monkeypatch, ["-c"])
        assert fake_backend["qualifiers"] == ["author"]

    def test_review_view_searches_review_requested_only(
        self, monkeypatch, fake_backend
    ):
        _run(monkeypatch, ["-r"])
        assert fake_backend["qualifiers"] == ["review-requested"]

    def test_all_view_searches_all_qualifiers(self, monkeypatch, fake_backend):
        _run(monkeypatch, ["-a"])
        assert fake_backend["qualifiers"] == [
            "author",
            "review-requested",
            "assignee",
            "involves",
        ]


class TestCountSemantics:
    def test_default_count_only_counts_attention(
        self, monkeypatch, fake_backend, capsys
    ):
        fake_backend["prs"] = [
            _pr(1, attention={"review"}),
            _pr(2),
            _pr(3, attention={"ready", "conflict"}),
        ]
        assert _run(monkeypatch, ["--count"]) == 0
        assert capsys.readouterr().out.strip() == "2"

    def test_explicit_view_count_counts_all_prs(
        self, monkeypatch, fake_backend, capsys
    ):
        fake_backend["prs"] = [_pr(1, attention={"review"}), _pr(2), _pr(3)]
        assert _run(monkeypatch, ["-c", "--count"]) == 0
        assert capsys.readouterr().out.strip() == "3"


class TestFailureSurfacing:
    def test_fetch_error_prints_error_and_exits_nonzero(
        self, monkeypatch, fake_backend, capsys
    ):
        def boom(qualifiers=None, on_warning=None):
            raise GhError("token expired")

        monkeypatch.setattr(cli, "fetch_prs", boom)
        assert _run(monkeypatch, []) == 1
        assert "token expired" in capsys.readouterr().err


class TestEscaping:
    def test_title_markup_is_escaped(self):
        pr = _pr(1, title="[link=https://evil.example]click[/link]")
        cell = cli._title_cell(pr)
        # Renders as literal text: escape() backslash-escapes the brackets.
        assert cell.startswith("\\[link=")

    def test_unmatched_closing_tag_does_not_crash_render(self):
        from rich.console import Console

        pr = _pr(1, title="broken [/bold] title", attention={"review"})
        console = Console(no_color=True, force_terminal=False)
        cli._render_attention(console, [pr])  # must not raise MarkupError
