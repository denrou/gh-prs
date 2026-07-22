"""Command-line interface for listing GitHub pull requests that need action."""

import argparse
import sys
from importlib.metadata import version
from typing import Any

from rich.console import Console
from rich.markup import escape
from rich.table import Table

from gh_prs.gh import (
    ALL_QUALIFIERS,
    GhError,
    PullRequest,
    count_prs,
    fetch_prs,
)

# Grouped sections for the default (attention) view, in display order.
# (reason key, section title, header style)
_SECTIONS = [
    ("review", "Needs your review", "bold cyan"),
    ("new-commits", "New commits since your review", "bold magenta"),
    ("ready", "Ready to ship", "bold green"),
    ("ci-failed", "CI failed", "bold red"),
    ("conflict", "Conflicts to resolve", "bold yellow"),
]

# Sections listing other people's PRs show the author column.
_SECTIONS_WITH_AUTHOR = {"review", "new-commits"}

# Per-view configuration: search qualifiers, flat-list title, and its style.
# The "attention" view renders grouped sections instead of a flat list.
_VIEWS: dict[str, tuple[list[str], str, str]] = {
    "attention": (["author", "review-requested", "reviewed-by"], "", ""),
    "created": (["author"], "PRs you created", "bold blue"),
    "review": (["review-requested"], "PRs awaiting your review", "bold cyan"),
    "all": (list(ALL_QUALIFIERS), "All PRs you are involved with", "bold"),
}

_REVIEW_STYLE = {
    "APPROVED": ("Approved", "green"),
    "CHANGES_REQUESTED": ("Changes req", "red"),
    "REVIEW_REQUIRED": ("Review req", "yellow"),
    "": ("—", "dim"),
}

_CHECKS_STYLE = {
    "SUCCESS": ("✓ pass", "green"),
    "FAILURE": ("✗ fail", "red"),
    "PENDING": ("● running", "yellow"),
    "": ("—", "dim"),
}


def _num_cell(pr: PullRequest) -> str:
    """PR number as a terminal hyperlink to the PR url."""
    label = f"#{pr.number}"
    return f"[link={pr.url}]{label}[/link]" if pr.url else label


def _review_cell(pr: PullRequest) -> str:
    text, style = _REVIEW_STYLE.get(pr.review_decision, (pr.review_decision, "white"))
    return f"[{style}]{text}[/{style}]"


def _checks_cell(pr: PullRequest) -> str:
    text, style = _CHECKS_STYLE.get(pr.checks_state, (pr.checks_state, "white"))
    return f"[{style}]{text}[/{style}]"


def _title_cell(pr: PullRequest) -> str:
    prefix = "[dim](draft)[/dim] " if pr.is_draft else ""
    # PR titles are attacker-controlled; escape so rich renders them as literal
    # text instead of markup (e.g. a [link=...] tag would become a real hyperlink).
    return f"{prefix}{escape(pr.title)}"


def _render_section(
    console: Console,
    title: str,
    style: str,
    prs: list[PullRequest],
    *,
    show_author: bool,
) -> None:
    table = Table(box=None, pad_edge=False, expand=False, show_header=False)
    table.add_column(style="cyan", no_wrap=True)  # repo
    table.add_column(style="bold", no_wrap=True)  # number
    table.add_column(overflow="ellipsis", no_wrap=True, max_width=70)  # title
    if show_author:
        table.add_column(style="magenta", no_wrap=True)  # author
    table.add_column(style="dim", no_wrap=True)  # updated
    for pr in prs:
        row = [pr.repo_short, _num_cell(pr), _title_cell(pr)]
        if show_author:
            row.append(escape(pr.author))
        row.append(pr.updated_date)
        table.add_row(*row)
    console.print(f"[{style}]{title}[/{style}] [dim]({len(prs)})[/dim]")
    console.print(table)
    console.print()


def _render_attention(console: Console, prs: list[PullRequest]) -> None:
    attention = [pr for pr in prs if pr.needs_attention()]
    if not attention:
        console.print("[green]✓[/green] Nothing needs your attention.")
        return
    for reason, title, style in _SECTIONS:
        group = [pr for pr in attention if reason in pr.attention_reasons]
        if group:
            _render_section(
                console,
                title,
                style,
                group,
                show_author=(reason in _SECTIONS_WITH_AUTHOR),
            )


def _render_list(
    console: Console, prs: list[PullRequest], *, title: str, style: str
) -> None:
    if not prs:
        console.print("[dim]No matching PRs.[/dim]")
        return
    table = Table(box=None, pad_edge=False, expand=False, show_header=False)
    table.add_column(style="cyan", no_wrap=True)  # repo
    table.add_column(style="bold", no_wrap=True)  # number
    table.add_column(overflow="ellipsis", no_wrap=True, max_width=60)  # title
    table.add_column(style="magenta", no_wrap=True)  # author
    table.add_column(no_wrap=True)  # review
    table.add_column(no_wrap=True)  # CI
    table.add_column(style="dim", no_wrap=True)  # updated
    for pr in prs:
        table.add_row(
            pr.repo_short,
            _num_cell(pr),
            _title_cell(pr),
            escape(pr.author),
            _review_cell(pr),
            _checks_cell(pr),
            pr.updated_date,
        )
    console.print(f"[{style}]{title}[/{style}] [dim]({len(prs)})[/dim]")
    console.print(table)


def _to_dict(pr: PullRequest) -> dict[str, Any]:
    return {
        "repo": pr.repo,
        "number": pr.number,
        "title": pr.title,
        "author": pr.author,
        "url": pr.url,
        "isDraft": pr.is_draft,
        "reviewDecision": pr.review_decision,
        "checksState": pr.checks_state,
        "mergeable": pr.mergeable,
        "myReviewState": pr.my_review_state,
        "myReviewCommit": pr.my_review_commit,
        "headRefOid": pr.head_ref_oid,
        "reviewRequestedExplicitly": pr.review_requested_explicitly,
        "roles": sorted(pr.roles),
        "attentionReasons": sorted(pr.attention_reasons),
        "updatedAt": pr.updated_at,
        "createdAt": pr.created_at,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="gh prs",
        description="List GitHub pull requests that need your attention.",
    )
    group = parser.add_mutually_exclusive_group()
    group.add_argument(
        "-c",
        "--created",
        dest="view",
        action="store_const",
        const="created",
        help="PRs you created",
    )
    group.add_argument(
        "-r",
        "--review",
        dest="view",
        action="store_const",
        const="review",
        help="PRs awaiting your review",
    )
    group.add_argument(
        "-a",
        "--all",
        dest="view",
        action="store_const",
        const="all",
        help="all PRs you are involved with",
    )
    parser.set_defaults(view="attention")
    parser.add_argument(
        "--json", action="store_true", help="output raw JSON instead of a table"
    )
    parser.add_argument(
        "--count",
        action="store_true",
        help="print only the number of PRs in the selected view (for status bars)",
    )
    parser.add_argument(
        "--no-color", action="store_true", help="disable colored output"
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {version('gh-prs')}"
    )
    args = parser.parse_args(argv)

    console = Console(no_color=args.no_color, highlight=False)
    err = Console(stderr=True, no_color=args.no_color, highlight=False)

    qualifiers, list_title, list_style = _VIEWS[args.view]

    def warn(msg: str) -> None:
        err.print(f"[yellow]Warning:[/yellow] {msg}")

    # Count-only fast path: a single-qualifier count (-c/-r with --count)
    # needs no node data and no cross-search de-duplication — a count-only
    # query answers it in a fraction of a full search's time, and the count
    # is exact even beyond the 100-node cap. The default view's count still
    # needs full data (attention reasons); -a needs de-duplication.
    fast_count = args.count and len(qualifiers) == 1

    prs: list[PullRequest] = []
    count = 0
    try:
        with err.status("Fetching pull requests…", spinner="dots"):
            if fast_count:
                count = count_prs(qualifiers[0])
            else:
                prs = fetch_prs(qualifiers, on_warning=warn)
    except GhError as exc:
        err.print(f"[red]Error:[/red] {exc}")
        return 1
    except KeyboardInterrupt:
        err.print("[dim]Interrupted.[/dim]")
        return 130

    if fast_count:
        print(count)
        return 0

    if args.count:
        # In the default view "count" means PRs needing attention; the explicit
        # views (-c/-r/-a) count every PR they would list.
        if args.view == "attention":
            print(sum(pr.needs_attention() for pr in prs))
        else:
            print(len(prs))
        return 0

    if args.json:
        console.print_json(data=[_to_dict(pr) for pr in prs])
        return 0

    if args.view == "attention":
        _render_attention(console, prs)
    else:
        _render_list(console, prs, title=list_title, style=list_style)

    return 0


if __name__ == "__main__":
    sys.exit(main())
