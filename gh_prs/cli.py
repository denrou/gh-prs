"""Command-line interface for listing GitHub pull requests that need action."""

import argparse
import re
import sys
from datetime import UTC, datetime, timedelta
from importlib.metadata import version
from typing import Any

from rich.console import Console
from rich.markup import escape
from rich.table import Table

from gh_prs.config import ConfigError, load_config
from gh_prs.gh import (
    ALL_QUALIFIERS,
    DEFAULT_STALE_AFTER,
    GhError,
    PullRequest,
    count_prs,
    fetch_pr_head,
    fetch_prs,
    resolve_pr,
)
from gh_prs.snooze import (
    SnoozeEntry,
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
    ("stale", "Waiting on review — time to nudge", "bold blue"),
    ("stale-draft", "Drafts gone quiet — finish or mark ready", "bold blue"),
]

# Sections listing other people's PRs show the author column.
_SECTIONS_WITH_AUTHOR = {"review", "new-commits"}

# A snooze reference that is a bare PR number: resolved through gh (against
# --repo or the current directory), unlike a full URL which normalize_pr_url
# canonicalizes offline.
_BARE_NUMBER = re.compile(r"^\d+$")

# How long a snooze lasts when --for is not given.
_DEFAULT_SNOOZE_FOR = "24h"

# The flags the snooze/unsnooze subcommands replaced, each with the syntax to
# suggest instead. Checked before argparse runs so the error is a migration
# hint rather than a bare "unrecognized arguments".
_REMOVED_FLAGS = {
    "--snooze": "gh prs snooze <pr>...",
    "--unsnooze": "gh prs unsnooze <pr>...",
    "--snoozed": "gh prs snooze",
}

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


def _ref_to_url(ref: str, repo: str | None) -> str:
    """Canonical PR url for one reference, without fetching its head.

    A bare PR number is resolved through gh (against ``repo``, or the current
    directory) so it learns its host and url; a full URL is canonicalized
    offline, so unsnoozing one needs no network.
    """
    ref = ref.strip()
    if _BARE_NUMBER.match(ref):
        url, _ = resolve_pr(ref, repo)
        return url
    return normalize_pr_url(ref)


def _ref_to_url_and_head(ref: str, repo: str | None) -> tuple[str, str]:
    """Canonical PR url plus head oid for one reference (for snoozing).

    A bare number resolves both in a single gh call; a URL is canonicalized
    offline and its head then fetched by url.
    """
    ref = ref.strip()
    if _BARE_NUMBER.match(ref):
        return resolve_pr(ref, repo)
    url = normalize_pr_url(ref)
    return url, fetch_pr_head(url)


def _config_stale_after(err: Console) -> timedelta | None:
    """The persisted staleness threshold from config.json (or the default on a
    config error; ``None`` when the user disabled the 'stale' nudge).

    The view may override this per-invocation with ``--stale-after``; snooze
    capture deliberately uses only this persisted value, so a captured 'stale'
    reason matches what later (unflagged) views will compute for the PR.
    """
    try:
        return load_config().stale_after
    except ConfigError as exc:
        err.print(f"[yellow]Warning:[/yellow] ignoring config: {exc}")
        return DEFAULT_STALE_AFTER


def _attention_reasons_by_url(
    err: Console, stale_after: timedelta | None
) -> dict[str, list[str]]:
    """Map each attention-view PR to its current reasons, for snooze capture.

    ``stale_after`` must be the same threshold later views will use (see
    ``_config_stale_after``) so a captured 'stale' reason doesn't spuriously
    differ from the rendered one and defeat the snooze on the next run.

    Best-effort: a lookup failure degrades to an empty map (the snooze still
    records head + window, just without reason-change invalidation) rather
    than aborting the snooze. Only PRs the attention searches return appear —
    a PR snoozed by an unrelated URL simply gets no reasons.
    """
    qualifiers = _VIEWS["attention"][0]

    def warn(msg: str) -> None:
        err.print(f"[yellow]Warning:[/yellow] {msg}")

    try:
        with err.status("Reading attention state…", spinner="dots"):
            prs = fetch_prs(qualifiers, on_warning=warn, stale_after=stale_after)
    except GhError as exc:
        warn(f"could not read attention state ({exc}); snoozing without it")
        return {}
    return {pr.url: sorted(pr.attention_reasons) for pr in prs}


def _do_snooze(
    args: argparse.Namespace,
    snoozes: dict[str, SnoozeEntry],
    now: datetime,
    console: Console,
    err: Console,
) -> int:
    """Snooze every PR in ``args.refs``; return the exit code.

    Refs are resolved independently: a bad one is reported and skipped while
    the rest are snoozed (partial success exits non-zero). The store is
    written once, only if at least one ref resolved.
    """
    # Validate the duration up front so a typo fails before any network round-trip.
    duration = parse_duration(args.snooze_for or _DEFAULT_SNOOZE_FOR)
    resolved: dict[str, str] = {}  # canonical url -> head oid
    failures: list[str] = []
    for ref in args.refs:
        try:
            with err.status(f"Looking up {escape(ref)}…", spinner="dots"):
                url, oid = _ref_to_url_and_head(ref, args.repo)
        except (SnoozeError, GhError) as exc:
            failures.append(f"{ref}: {exc}")
            continue
        resolved[url] = oid
    # Capture each resolved PR's current attention reasons so the snooze also
    # lapses when they change — e.g. a review lands and a waiting PR becomes
    # ready to merge — not only when its head moves. Only worth a fetch once
    # at least one ref resolved.
    reasons_by_url = (
        _attention_reasons_by_url(err, _config_stale_after(err)) if resolved else {}
    )
    for url, oid in resolved.items():
        snoozes[url] = make_entry(oid, now, duration, reasons_by_url.get(url))
    if resolved:
        save_snoozes(snoozes)
        for url in resolved:
            console.print(
                f"Snoozed {escape(url)} [dim](until "
                f"{_local(snoozes[url]['until'])}, or sooner if its head moves "
                "or its status changes)[/dim]"
            )
    for failure in failures:
        err.print(f"[red]Error:[/red] {escape(failure)}")
    return 1 if failures else 0


def _do_unsnooze(
    args: argparse.Namespace,
    snoozes: dict[str, SnoozeEntry],
    console: Console,
    err: Console,
) -> int:
    """Remove the snooze on every PR in ``args.refs``; return the exit code.

    As with snoozing, refs are handled independently (a bad or not-snoozed one
    is reported and skipped) and the store is written once if anything changed.
    """
    removed: list[str] = []
    failures: list[str] = []
    for ref in args.refs:
        try:
            url = _ref_to_url(ref, args.repo)
        except (SnoozeError, GhError) as exc:
            failures.append(f"{ref}: {exc}")
            continue
        if snoozes.pop(url, None) is None:
            failures.append(f"{ref} ({url}) is not snoozed")
        else:
            removed.append(url)
    if removed:
        save_snoozes(snoozes)
        for url in removed:
            console.print(f"Unsnoozed {escape(url)}")
    for failure in failures:
        err.print(f"[red]Error:[/red] {escape(failure)}")
    return 1 if failures else 0


def _list_snoozes(
    snoozes: dict[str, SnoozeEntry], now: datetime, console: Console
) -> int:
    """Print the snooze store (the bare ``snooze`` subcommand)."""
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


def _run_snooze_command(
    args: argparse.Namespace, console: Console, err: Console
) -> int:
    """Handle the snooze/unsnooze subcommands; returns the exit code.

    A corrupt store is fatal here (a write would clobber it, and the bare
    listing must not show a half-parsed store), unlike in the attention view
    where it merely degrades to "nothing snoozed".
    """
    try:
        snoozes = load_snoozes()
        now = datetime.now(UTC)
        if args.command == "unsnooze":
            return _do_unsnooze(args, snoozes, console, err)
        if args.refs:
            return _do_snooze(args, snoozes, now, console, err)
        return _list_snoozes(snoozes, now, console)
    except (SnoozeError, GhError) as exc:
        err.print(f"[red]Error:[/red] {exc}")
        return 1
    except KeyboardInterrupt:
        err.print("[dim]Interrupted.[/dim]")
        return 130


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:]) if argv is None else list(argv)
    for token in argv:
        flag = token.split("=", 1)[0]
        if flag in _REMOVED_FLAGS:
            Console(stderr=True, highlight=False).print(
                f"[red]Error:[/red] {flag} was replaced by a subcommand: "
                f"try '{_REMOVED_FLAGS[flag]}' (see --help)"
            )
            return 2

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
        "--stale-after",
        dest="stale_after",
        metavar="DURATION",
        help="flag PRs you created that have gone this long without activity "
        "while still awaiting review or still draft "
        "(e.g. 3d, 1w; default 3d, overrides config.json)",
    )
    parser.add_argument(
        "--no-color", action="store_true", help="disable colored output"
    )
    parser.add_argument(
        "--version", action="version", version=f"%(prog)s {version('gh-prs')}"
    )

    # Flags shared by the subcommands. --no-color must also be accepted
    # *after* the subcommand ("gh prs snooze 123 --no-color"); SUPPRESS keeps
    # the subparser's default from clobbering a value the top-level flag
    # already set on the shared namespace.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "-R",
        "--repo",
        metavar="OWNER/REPO",
        help="repository for bare PR numbers "
        "(default: the repository in the current directory)",
    )
    common.add_argument(
        "--no-color",
        action="store_true",
        default=argparse.SUPPRESS,
        help="disable colored output",
    )
    commands = parser.add_subparsers(dest="command", metavar="{snooze,unsnooze}")
    snooze_cmd = commands.add_parser(
        "snooze",
        parents=[common],
        help="hide PRs from the attention view (with no arguments: list snoozed PRs)",
        description="Hide one or more PRs from the attention view for --for's "
        "duration, or until they get new commits or their status changes. "
        "With no PR arguments, list the snoozed PRs instead.",
    )
    snooze_cmd.add_argument(
        "refs",
        nargs="*",
        metavar="PR",
        help="a PR number (scoped by -R, or the current directory's repository) "
        "or a full URL; omit to list snoozed PRs",
    )
    snooze_cmd.add_argument(
        "--for",
        dest="snooze_for",
        metavar="DURATION",
        help="how long to hide the PRs "
        f"(e.g. 12h, 3d, 1w; default {_DEFAULT_SNOOZE_FOR})",
    )
    unsnooze_cmd = commands.add_parser(
        "unsnooze",
        parents=[common],
        help="remove the snooze on one or more PRs",
        description="Remove the snooze on one or more PRs.",
    )
    unsnooze_cmd.add_argument(
        "refs",
        nargs="+",
        metavar="PR",
        help="a PR number (scoped by -R, or the current directory's repository) "
        "or a full URL",
    )

    args = parser.parse_args(argv)

    console = Console(no_color=args.no_color, highlight=False)
    err = Console(stderr=True, no_color=args.no_color, highlight=False)

    if args.command:
        # The view flags parse fine before a subcommand but don't apply to
        # it; reject the mix instead of silently ignoring it.
        if (
            args.view != "attention"
            or args.count
            or args.json
            or args.stale_after is not None
        ):
            err.print(
                "[red]Error:[/red] -c/-r/-a, --count, --json and --stale-after "
                f"do not apply to 'gh prs {args.command}'"
            )
            return 2
        if args.command == "snooze" and args.snooze_for is not None and not args.refs:
            err.print("[red]Error:[/red] --for requires at least one PR to snooze")
            return 2
        return _run_snooze_command(args, console, err)

    qualifiers, list_title, list_style = _VIEWS[args.view]

    def warn(msg: str) -> None:
        err.print(f"[yellow]Warning:[/yellow] {msg}")

    # Count-only fast path: a single-qualifier count (-c/-r with --count)
    # needs no node data and no cross-search de-duplication — a count-only
    # query answers it in a fraction of a full search's time, and the count
    # is exact even beyond the 100-node cap. The default view's count still
    # needs full data (attention reasons); -a needs de-duplication.
    fast_count = args.count and len(qualifiers) == 1

    # Resolve the staleness threshold for the 'stale' nudge: an explicit
    # --stale-after overrides the config file, which falls back to the 3-day
    # default. A bad flag value is a hard error (explicit user input); a bad
    # config file only warns and uses the default (fail-safe: keep working).
    # The fast-count path never computes attention reasons, so skip it there.
    stale_after = None
    if not fast_count:
        stale_after = _config_stale_after(err)
        if args.stale_after is not None:
            try:
                stale_after = parse_duration(args.stale_after)
            except SnoozeError as exc:
                err.print(f"[red]Error:[/red] {exc}")
                return 1

    prs: list[PullRequest] = []
    count = 0
    try:
        with err.status("Fetching pull requests…", spinner="dots"):
            if fast_count:
                count = count_prs(qualifiers[0])
            else:
                prs = fetch_prs(qualifiers, on_warning=warn, stale_after=stale_after)
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
                    warn(f"could not prune expired snoozes: {exc}")
        # Hiding is never silent — even for --count, where the number a
        # status bar shows would otherwise silently drop. Only
        # attention-worthy PRs were actually withheld.
        hidden = sum(pr.needs_attention() for pr in hidden_snoozed)
        if hidden:
            err.print(
                f"[dim]{hidden} snoozed PR(s) hidden — 'gh prs snooze' to list[/dim]"
            )

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
