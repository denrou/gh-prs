"""Command-line interface for listing GitHub pull requests that need action."""

import argparse
import sys
from datetime import UTC, datetime
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
    fetch_pr_head,
    fetch_prs,
)
from gh_prs.snooze import (
    SnoozeError,
    is_expired,
    load_snoozes,
    make_entry,
    normalize_pr_url,
    parse_duration,
    save_snoozes,
    split_snoozed,
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


def _local(timestamp: str) -> str:
    """An ISO timestamp rendered in the user's local timezone, minute precision."""
    return datetime.fromisoformat(timestamp).astimezone().strftime("%Y-%m-%d %H:%M")


def _run_snooze_action(args: argparse.Namespace, console: Console, err: Console) -> int:
    """Handle --snooze / --unsnooze / --snoozed; returns the exit code.

    A corrupt store is fatal here (writing would clobber it), unlike in the
    attention view where it merely degrades to "nothing snoozed".
    """
    try:
        snoozes = load_snoozes()
        now = datetime.now(UTC)
        if args.snoozed:
            if not snoozes:
                console.print("[dim]No snoozed PRs.[/dim]")
            for url, entry in sorted(snoozes.items()):
                if is_expired(entry, now):
                    detail = "expired"
                else:
                    detail = (
                        f"until {_local(entry['until'])}, "
                        f"or head moving off {entry['oid'][:12]}"
                    )
                console.print(f"{escape(url)} [dim]({detail})[/dim]")
            return 0
        if args.snooze is not None:
            url = normalize_pr_url(args.snooze)
            # Validate the duration before spending a network round-trip.
            duration = parse_duration(args.snooze_for)
            with err.status("Looking up PR…", spinner="dots"):
                oid = fetch_pr_head(url)
            snoozes[url] = make_entry(oid, now, duration)
            save_snoozes(snoozes)
            console.print(
                f"Snoozed {escape(url)} [dim](until "
                f"{_local(snoozes[url]['until'])}, or sooner if its head moves)[/dim]"
            )
            return 0
        url = normalize_pr_url(args.unsnooze)
        if snoozes.pop(url, None) is None:
            err.print(f"[red]Error:[/red] {escape(url)} is not snoozed")
            return 1
        save_snoozes(snoozes)
        console.print(f"Unsnoozed {escape(url)}")
        return 0
    except (SnoozeError, GhError) as exc:
        err.print(f"[red]Error:[/red] {exc}")
        return 1
    except KeyboardInterrupt:
        err.print("[dim]Interrupted.[/dim]")
        return 130


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
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument(
        "--snooze",
        metavar="PR",
        help="hide a PR (URL or owner/repo/number) from the attention view "
        "for --for's duration, or until it gets new commits",
    )
    actions.add_argument(
        "--unsnooze",
        metavar="PR",
        help="remove a PR's snooze (URL or owner/repo/number)",
    )
    actions.add_argument(
        "--snoozed", action="store_true", help="list snoozed PRs and exit"
    )
    parser.add_argument(
        "--for",
        dest="snooze_for",
        default="24h",
        metavar="DURATION",
        help="with --snooze: how long to hide the PR (e.g. 12h, 3d, 1w; default 24h)",
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

    # "is not None", not truthiness: --snooze/--unsnooze "" must reach the
    # action (and fail its URL validation), not fall through to the view.
    if args.snooze is not None or args.unsnooze is not None or args.snoozed:
        return _run_snooze_action(args, console, err)

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

    # Only the attention view (table and --count, not --json) honors snoozes;
    # explicit views (-c/-r/-a) and single-qualifier counts always show
    # everything, so their numbers stay exact.
    hidden_snoozed: list[PullRequest] = []
    if args.view == "attention" and not args.json:
        try:
            snoozes = load_snoozes()
        except SnoozeError as exc:
            # Fail-safe direction: an unreadable store shows more, never less.
            warn(f"ignoring snoozes: {exc}")
            snoozes = {}
        if snoozes:
            fetched = {pr.url for pr in prs}
            prs, hidden_snoozed, dead = split_snoozed(prs, snoozes, datetime.now(UTC))
            for url, why in sorted(dead.items()):
                del snoozes[url]
                # Entries for absent PRs (closed, or beyond the search cap)
                # are pruned quietly: nothing resurfaced.
                if url in fetched:
                    warn(f"snooze expired for {url} ({why})")
            if dead:
                try:
                    save_snoozes(snoozes)
                except SnoozeError as exc:
                    warn(str(exc))

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
        # Only attention-worthy PRs were actually withheld from the table.
        hidden = sum(pr.needs_attention() for pr in hidden_snoozed)
        if hidden:
            err.print(
                f"[dim]{hidden} snoozed PR(s) hidden — 'gh prs --snoozed' to list[/dim]"
            )
    else:
        _render_list(console, prs, title=list_title, style=list_style)

    return 0


if __name__ == "__main__":
    sys.exit(main())
