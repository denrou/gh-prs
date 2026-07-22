"""Local per-PR snooze store: hide a PR from the attention view until it moves.

A snooze records the PR's head commit oid at snooze time. The PR stays hidden
from the default (attention) view while its head still matches; as soon as the
head moves — new commits, a rebase — or either oid is unknown, the PR
resurfaces and the spent entry is pruned. Fail-safe direction: a snooze may
only ever hide the exact acknowledged state, never unknown or newer work.

The store is a flat JSON object mapping canonical PR URL → head oid, kept at
``$XDG_CONFIG_HOME/gh-prs/snooze.json`` (``~/.config/gh-prs/snooze.json`` by
default). Explicit views (``-c``/``-r``/``-a``) and count queries never
consult it, so their numbers stay exact.
"""

import json
import os
import re
from pathlib import Path

from gh_prs.gh import PullRequest


class SnoozeError(Exception):
    """The snooze store is unreadable/unwritable or an entry is invalid."""


# Canonical PR URL prefix: scheme, host (github.com or an Enterprise host),
# owner, repo, "pull", number. Everything after the number (e.g. "/files",
# "?diff=split", "#discussion_r1") is browser navigation state, not identity.
_PR_URL = re.compile(r"^(https://[^/\s]+/[^/\s]+/[^/\s]+/pull/\d+)")


def snooze_path() -> Path:
    config_home = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(config_home) / "gh-prs" / "snooze.json"


def normalize_pr_url(url: str) -> str:
    """Reduce a PR URL to the canonical form GraphQL reports (`…/pull/<n>`).

    Raises ``SnoozeError`` for anything that isn't a PR URL: storing a key
    that can never match a fetched PR's ``url`` would silently do nothing.
    """
    match = _PR_URL.match(url.strip())
    if not match:
        raise SnoozeError(f"not a pull request URL: {url!r}")
    return match.group(1)


def load_snoozes(path: Path | None = None) -> dict[str, str]:
    """Return the stored snoozes as ``{PR url: head oid at snooze time}``.

    A missing file is an empty store. Anything else that prevents a clean
    read raises ``SnoozeError`` — the caller decides whether that is fatal
    (write commands must not clobber the file) or degradable (the attention
    view shows more, never less).
    """
    path = path or snooze_path()
    try:
        raw = path.read_text()
    except FileNotFoundError:
        return {}
    except OSError as e:
        raise SnoozeError(f"cannot read {path}: {e}") from e
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        raise SnoozeError(f"{path} is not valid JSON: {e}") from e
    if not isinstance(data, dict) or not all(
        isinstance(k, str) and isinstance(v, str) for k, v in data.items()
    ):
        raise SnoozeError(f"{path} has an unexpected shape (want {{url: oid}})")
    return data


def save_snoozes(snoozes: dict[str, str], path: Path | None = None) -> None:
    """Write the store, creating its directory if needed.

    Raises ``SnoozeError`` on any I/O failure.
    """
    path = path or snooze_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(snoozes, indent=2, sort_keys=True) + "\n")
    except OSError as e:
        raise SnoozeError(f"cannot write {path}: {e}") from e


def split_snoozed(
    prs: list[PullRequest], snoozes: dict[str, str]
) -> tuple[list[PullRequest], list[PullRequest], list[str]]:
    """Partition PRs into (visible, hidden) and report spent snooze URLs.

    A PR is hidden only when its head oid is known and still equals the
    snoozed oid. Any other snoozed PR — head moved, or either oid missing —
    stays visible and its URL is reported as spent so the caller can prune
    it. Entries matching none of ``prs`` are left alone: the PR may be
    closed, or merely absent from a truncated search.
    """
    visible: list[PullRequest] = []
    hidden: list[PullRequest] = []
    spent: list[str] = []
    for pr in prs:
        snoozed_oid = snoozes.get(pr.url)
        if snoozed_oid is None:
            visible.append(pr)
        elif snoozed_oid and pr.head_ref_oid == snoozed_oid:
            hidden.append(pr)
        else:
            spent.append(pr.url)
            visible.append(pr)
    return visible, hidden, spent
