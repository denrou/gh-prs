"""Local per-PR snooze store: hide a PR from the attention view for a while.

A snooze records the PR's head commit oid, an expiry timestamp (24h by
default), and — when known — the attention reasons it had at snooze time.
The PR stays hidden from the default (attention) view only while ALL hold:
the head still matches, the window has not elapsed, and its attention
reasons are unchanged. As soon as any breaks — new commits, a rebase, the
clock, a review landing that turns a waiting PR into one that's ready to
merge, or an oid/timestamp that can't be compared — the PR resurfaces and
the dead entry is pruned. Fail-safe direction: a snooze may only ever hide
the exact acknowledged state for a bounded time, never unknown or newer work.

The store is a JSON object mapping canonical PR URL →
``{"oid", "until", "reasons"?}`` (``reasons`` is a sorted list, absent on
entries written before it existed), kept at
``$XDG_CONFIG_HOME/gh-prs/snooze.json`` (``~/.config/gh-prs/snooze.json`` by
default). Only the default attention view (its table and ``--count``)
consults it; explicit views (``-c``/``-r``/``-a``), their fast counts, and
``--json`` never do, so their numbers stay exact.
"""

import json
import os
import re
from datetime import datetime, timedelta
from pathlib import Path
from typing import NotRequired, TypedDict

from gh_prs.gh import PullRequest


class SnoozeError(Exception):
    """The snooze store is unreadable/unwritable or an entry is invalid."""


class SnoozeEntry(TypedDict):
    """One stored snooze: the acknowledged head oid, the expiry, and the
    attention reasons at snooze time. ``reasons`` is absent on entries written
    before reason-tracking existed (and on PRs whose reasons couldn't be
    captured); such entries fall back to the head-and-window rule alone.
    """

    oid: str
    until: str
    reasons: NotRequired[list[str]]


# Canonical PR URL prefix: scheme, host (github.com or an Enterprise host),
# owner, repo, "pull", number. The number must be followed by end-of-string
# or a separator — browser navigation state such as "/files", "?diff=split",
# or "#discussion_r1", which is discarded. Anything fused to the digits
# (".../pull/42abc") is a typo, and truncating it would snooze the wrong PR.
_PR_URL = re.compile(r"^(https://[^/\s]+/[^/\s]+/[^/\s]+/pull/\d+)(?=$|[/?#])")

_DURATION = re.compile(r"^(\d+)\s*([hdw])$")
_DURATION_UNITS = {"h": "hours", "d": "days", "w": "weeks"}


def snooze_path() -> Path:
    config_home = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(config_home) / "gh-prs" / "snooze.json"


def parse_duration(text: str) -> timedelta:
    """Parse a snooze duration like ``12h``, ``3d``, or ``1w``.

    Raises ``SnoozeError`` on anything else — malformed input, zero (a snooze
    that never hides anything is a typo, not a request), and durations too
    large to represent. A value with enough digits overflows ``timedelta``
    (``OverflowError``) or trips CPython's int-string conversion limit
    (``ValueError``); both are converted here so every caller sees the one
    error type it already handles and a bad duration can never escape as an
    uncaught crash — same fail-safe direction as the rest of the store.
    """
    msg = f"invalid duration {text!r} (use a positive number of hours, days, or weeks: e.g. 12h, 3d, 1w)"
    match = _DURATION.match(text.strip().lower())
    if not match:
        raise SnoozeError(msg)
    amount, unit = match.groups()
    try:
        value = int(amount)
        if not value:
            raise SnoozeError(msg)
        return timedelta(**{_DURATION_UNITS[unit]: value})
    except (ValueError, OverflowError) as e:
        raise SnoozeError(msg) from e


def normalize_pr_url(ref: str) -> str:
    """Canonicalize a full PR URL to the form GraphQL reports (`…/pull/<n>`).

    Browser suffixes (``/files``, ``?diff=split``, ``#discussion_r1``) are
    stripped. Raises ``SnoozeError`` on anything that is not a full PR URL —
    a bare PR number is resolved through ``gh`` instead (see ``cli``), and
    storing a key that can never match a fetched PR's ``url`` would silently
    do nothing.
    """
    ref = ref.strip()
    match = _PR_URL.match(ref)
    if not match:
        raise SnoozeError(
            f"not a pull request URL: {ref!r} "
            "(pass a PR number with --repo, or a full https://…/pull/<n> URL)"
        )
    return match.group(1)


def _reasons_ok(reasons: object) -> bool:
    """A stored entry's ``reasons`` must be absent or a list of strings."""
    return reasons is None or (
        isinstance(reasons, list) and all(isinstance(r, str) for r in reasons)
    )


def load_snoozes(path: Path | None = None) -> dict[str, SnoozeEntry]:
    """Return the stored snoozes as ``{PR url: {"oid", "until", "reasons"?}}``.

    A missing file is an empty store. Anything else that prevents a clean
    read raises ``SnoozeError`` — the caller decides whether that is fatal
    (write commands must not clobber the file) or degradable (the attention
    view shows more, never less).
    """
    path = path or snooze_path()
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return {}
    except UnicodeDecodeError as e:
        # Not an OSError: without this clause a corrupt (e.g. truncated)
        # file would crash the caller instead of degrading.
        raise SnoozeError(f"{path} is not valid UTF-8: {e}") from e
    except OSError as e:
        raise SnoozeError(f"cannot read {path}: {e}") from e
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        raise SnoozeError(f"{path} is not valid JSON: {e}") from e
    if not isinstance(data, dict) or not all(
        isinstance(k, str)
        and isinstance(v, dict)
        and isinstance(v.get("oid"), str)
        and isinstance(v.get("until"), str)
        and _reasons_ok(v.get("reasons"))
        for k, v in data.items()
    ):
        raise SnoozeError(
            f"{path} has an unexpected shape (want {{url: {{oid, until, reasons?}}}})"
        )
    return data


def save_snoozes(snoozes: dict[str, SnoozeEntry], path: Path | None = None) -> None:
    """Write the store, creating its directory if needed.

    Raises ``SnoozeError`` on any I/O failure.
    """
    path = path or snooze_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # Write-then-rename so a crash mid-write can't leave a truncated
        # store (which would then read as corrupt).
        tmp = path.with_name(path.name + ".tmp")
        tmp.write_text(
            json.dumps(snoozes, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.replace(tmp, path)
    except OSError as e:
        raise SnoozeError(f"cannot write {path}: {e}") from e


def make_entry(
    oid: str,
    now: datetime,
    duration: timedelta,
    reasons: list[str] | None = None,
) -> SnoozeEntry:
    """Build a store entry hiding ``oid`` until ``now + duration``.

    ``reasons`` (the PR's attention reasons at snooze time) is stored sorted
    so a later set-equality check is order-independent; ``None`` omits the
    key, leaving the entry on the head-and-window rule alone.
    """
    entry: SnoozeEntry = {
        "oid": oid,
        "until": (now + duration).isoformat(timespec="seconds"),
    }
    if reasons is not None:
        entry["reasons"] = sorted(reasons)
    return entry


def is_expired(entry: SnoozeEntry, now: datetime) -> bool:
    """True when the entry's window has elapsed.

    A missing, unparseable, or naive stored timestamp counts as expired:
    fail-safe, the PR shows. ``now`` must be timezone-aware — the comparison
    happens outside the try so a naive ``now`` (a caller bug) raises loudly
    instead of silently expiring every entry in the store.
    """
    try:
        until = datetime.fromisoformat(entry["until"])
    except KeyError, ValueError, TypeError:
        return True
    if until.tzinfo is None:
        return True
    return until <= now


def split_snoozed(
    prs: list[PullRequest], snoozes: dict[str, SnoozeEntry], now: datetime
) -> tuple[list[PullRequest], list[PullRequest], dict[str, str]]:
    """Partition PRs into (visible, hidden) and report dead snoozes.

    A PR is hidden only while its head oid is known and still equals the
    snoozed oid, the window has not elapsed, AND — when the entry recorded
    them — its attention reasons still match those acknowledged at snooze
    time. Dead entries — head moved, window elapsed (checked even for PRs
    absent from the search), reasons changed (e.g. a review landed and a
    waiting PR is now ready to merge), or a timestamp that can't be compared
    — come back as ``{url: reason}`` for the caller to prune. Live entries
    for absent PRs are kept: the PR may merely be beyond a truncated search.
    """
    visible: list[PullRequest] = []
    hidden: list[PullRequest] = []
    dead: dict[str, str] = {}
    for pr in prs:
        entry = snoozes.get(pr.url)
        if entry is None:
            visible.append(pr)
        elif is_expired(entry, now):
            dead[pr.url] = "snooze window elapsed"
            visible.append(pr)
        elif not (entry["oid"] and pr.head_ref_oid == entry["oid"]):
            dead[pr.url] = "head moved since you snoozed it"
            visible.append(pr)
        elif (snoozed := entry.get("reasons")) is not None and sorted(
            pr.attention_reasons
        ) != snoozed:
            # Same head, still within the window, but the PR now needs
            # attention for a different reason than the one acknowledged.
            dead[pr.url] = "its status changed since you snoozed it"
            visible.append(pr)
        else:
            hidden.append(pr)
    fetched = {pr.url for pr in prs}
    for url, entry in snoozes.items():
        if url not in fetched and is_expired(entry, now):
            dead[url] = "snooze window elapsed"
    return visible, hidden, dead
