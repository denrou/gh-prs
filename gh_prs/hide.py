"""Local per-PR hide store: keep a PR out of the attention view for good.

A snooze acknowledges one state of a PR for a bounded time; a hide is the
user's standing decision that a PR is not theirs to act on — typically a
review request that a teammate is already handling. Nothing on the PR side
lifts it: not new commits, not a rebase, not a change in why the PR would
need attention. Only ``gh prs unhide`` brings the PR back, and a hidden PR
that closes simply stops matching the searches (``gh prs hide --prune``
drops such entries). That is the one place this tool hides on the user's
word alone, so the store records nothing about the PR beyond its URL and
when it was hidden.

The store is a JSON object mapping canonical PR URL → ``{"since"}`` kept at
``$XDG_CONFIG_HOME/gh-prs/hidden.json``. Only the default attention view (its
table and ``--count``) consults it; explicit views (``-c``/``-r``/``-a``),
their fast counts, and ``--json`` never do, so their numbers stay exact.
"""

from datetime import datetime
from pathlib import Path
from typing import TypedDict

from gh_prs.gh import PullRequest
from gh_prs.store import read_json, store_path, write_json


class HideError(Exception):
    """The hide store is unreadable/unwritable or an entry is invalid."""


class HideEntry(TypedDict):
    """One hidden PR: when it was hidden (ISO timestamp, informational)."""

    since: str


def hide_path() -> Path:
    return store_path("hidden.json")


def load_hidden(path: Path | None = None) -> dict[str, HideEntry]:
    """Return the stored hides as ``{PR url: {"since"}}``.

    A missing file is an empty store. Anything else that prevents a clean
    read raises ``HideError`` — the caller decides whether that is fatal
    (write commands must not clobber the file) or degradable (the attention
    view shows more, never less).
    """
    path = path or hide_path()
    data = read_json(path, HideError)
    if data is None:
        return {}
    if not isinstance(data, dict) or not all(
        isinstance(k, str) and isinstance(v, dict) and isinstance(v.get("since"), str)
        for k, v in data.items()
    ):
        raise HideError(f"{path} has an unexpected shape (want {{url: {{since}}}})")
    return data


def save_hidden(hidden: dict[str, HideEntry], path: Path | None = None) -> None:
    """Write the store, creating its directory if needed.

    Raises ``HideError`` on any I/O failure.
    """
    write_json(path or hide_path(), hidden, HideError)


def make_entry(now: datetime) -> HideEntry:
    """Build a store entry hiding a PR from ``now`` on."""
    return {"since": now.isoformat(timespec="seconds")}


def split_hidden(
    prs: list[PullRequest], hidden: dict[str, HideEntry]
) -> tuple[list[PullRequest], list[PullRequest]]:
    """Partition PRs into (visible, hidden).

    A PR is hidden when its URL is in the store, whatever its head commit or
    attention reasons: a hide is the user's standing decision, not an
    acknowledgement of one state. Entries for PRs absent from the search are
    left alone (the PR may be closed, or merely beyond the search cap).
    """
    visible: list[PullRequest] = []
    withheld: list[PullRequest] = []
    for pr in prs:
        (withheld if pr.url in hidden else visible).append(pr)
    return visible, withheld
