"""Local per-PR snooze store: hide a PR from the attention view until it changes.

A snooze records the PR's head commit oid, the attention reasons it had at
snooze time, and — only when ``--for`` asked for a fixed window — an expiry
timestamp (local midnight for day-based windows, see ``duration``). The PR
stays hidden from the default (attention) view only while ALL hold: the head
still matches, its attention reasons are unchanged, and the window, if any,
has not elapsed. As soon as any breaks — new commits, a rebase, a review
landing that turns a waiting PR into one that's ready to merge, the clock, or
an oid/timestamp that can't be compared — the PR resurfaces and the dead
entry is pruned. Fail-safe direction: a snooze may only ever hide the exact
acknowledged state, never unknown or newer work. An open-ended snooze (no
window) therefore *must* carry reasons: with nothing but the head to watch, a
review landing on an authored PR would never bring it back. The invariant is
structural — ``make_entry`` and ``load_snoozes`` both refuse an open-ended
entry without them — so no code path can hide a PR on its head alone forever.

The store is a JSON object mapping canonical PR URL →
``{"oid", "until", "reasons"?}``, kept at
``$XDG_CONFIG_HOME/gh-prs/snooze.json`` (``~/.config/gh-prs/snooze.json`` by
default). Only the default attention view (its table and ``--count``)
consults it; explicit views (``-c``/``-r``/``-a``), their fast counts, and
``--json`` never do, so their numbers stay exact.
"""

import re
from datetime import datetime
from pathlib import Path
from typing import NotRequired, TypedDict

from gh_prs.duration import Duration
from gh_prs.gh import PullRequest
from gh_prs.store import read_json, store_path, write_json


class SnoozeError(Exception):
    """The snooze store is unreadable/unwritable or an entry is invalid."""


class SnoozeEntry(TypedDict):
    """One stored snooze: the acknowledged head oid, the expiry, and the
    attention reasons at snooze time.

    ``until`` is an ISO timestamp for a fixed window, or ``None`` for an
    open-ended snooze that only the head or the reasons can lift. The key is
    always present: a *missing* ``until`` is a malformed entry, not an
    open-ended one, so a half-written file can never hide a PR for good.

    ``reasons`` is absent on windowed entries written before reason-tracking
    existed, and on windowed snoozes of PRs whose reasons couldn't be
    captured; such entries fall back to the head-and-window rule alone. An
    open-ended entry always has it (see the module docstring).
    """

    oid: str
    until: str | None
    reasons: NotRequired[list[str]]


# Canonical PR URL prefix: scheme, host (github.com or an Enterprise host),
# owner, repo, "pull", number. The number must be followed by end-of-string
# or a separator — browser navigation state such as "/files", "?diff=split",
# or "#discussion_r1", which is discarded. Anything fused to the digits
# (".../pull/42abc") is a typo, and truncating it would snooze the wrong PR.
_PR_URL = re.compile(r"^(https://[^/\s]+/[^/\s]+/[^/\s]+/pull/\d+)(?=$|[/?#])")

_DURATION = re.compile(r"^(\d+)\s*([hdw])$")

# Days a 'w' stands for by default: a calendar week.
CALENDAR_WEEK_DAYS = 7

# The longest duration accepted, about a century: far beyond any real use,
# and far enough from datetime's year-9999 ceiling that turning a duration
# into a timestamp can never overflow.
_MAX_DAYS = 36_500


def snooze_path() -> Path:
    return store_path("snooze.json")


def parse_duration(text: str, *, week_days: int = CALENDAR_WEEK_DAYS) -> Duration:
    """Parse a duration like ``12h``, ``3d``, or ``1w``.

    Hours stay hours; days and weeks become whole days, which callers count
    on the calendar (see ``duration``). ``week_days`` is how many days a
    ``w`` stands for — seven by default. Callers counting working days (with
    weekends skipped) pass five, so ``1w`` stays a same-weekday anniversary
    instead of stretching to seven working days. Only the unit's length
    changes here; which days count is the caller's business.

    Raises ``SnoozeError`` on anything else — malformed input, zero (a snooze
    that never hides anything is a typo, not a request), and durations longer
    than ``_MAX_DAYS``. A value with enough digits trips CPython's int-string
    conversion limit (``ValueError``); that is converted here too, so every
    caller sees the one error type it already handles and a bad duration can
    never escape as an uncaught crash — same fail-safe direction as the rest
    of the store.
    """
    msg = f"invalid duration {text!r} (use a positive number of hours, days, or weeks: e.g. 12h, 3d, 1w)"
    match = _DURATION.match(text.strip().lower())
    if not match:
        raise SnoozeError(msg)
    amount, unit = match.groups()
    try:
        value = int(amount)
    except ValueError as e:
        raise SnoozeError(msg) from e
    if unit == "w":
        value, unit = value * week_days, "d"
    limit = _MAX_DAYS * 24 if unit == "h" else _MAX_DAYS
    if not 0 < value <= limit:
        raise SnoozeError(msg)
    return Duration(value, unit)


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


def _entry_ok(value: object) -> bool:
    """Shape check for one stored entry.

    ``until`` must be present — a string (a window) or ``null`` (open-ended);
    its absence is treated as corruption, never as "forever". An open-ended
    entry must also carry ``reasons``: that is the one thing besides the head
    that can lift it, so an entry without them would hide a PR on its head
    alone for good (the module docstring explains why that must not exist).
    """
    if not isinstance(value, dict) or not isinstance(value.get("oid"), str):
        return False
    if "until" not in value or not _reasons_ok(value.get("reasons")):
        return False
    until = value["until"]
    if until is None:
        return value.get("reasons") is not None
    return isinstance(until, str)


def load_snoozes(path: Path | None = None) -> dict[str, SnoozeEntry]:
    """Return the stored snoozes as ``{PR url: {"oid", "until", "reasons"?}}``.

    A missing file is an empty store. Anything else that prevents a clean
    read raises ``SnoozeError`` — the caller decides whether that is fatal
    (write commands must not clobber the file) or degradable (the attention
    view shows more, never less).
    """
    path = path or snooze_path()
    data = read_json(path, SnoozeError)
    if data is None:
        return {}
    if not isinstance(data, dict) or not all(
        isinstance(k, str) and _entry_ok(v) for k, v in data.items()
    ):
        raise SnoozeError(
            f"{path} has an unexpected shape "
            "(want {url: {oid, until, reasons?}}; an entry with a null until "
            "must have reasons)"
        )
    return data


def save_snoozes(snoozes: dict[str, SnoozeEntry], path: Path | None = None) -> None:
    """Write the store, creating its directory if needed.

    Raises ``SnoozeError`` on any I/O failure.
    """
    write_json(path or snooze_path(), snoozes, SnoozeError)


def make_entry(
    oid: str,
    until: datetime | None,
    reasons: list[str] | None = None,
) -> SnoozeEntry:
    """Build a store entry hiding ``oid`` until ``until``, or for good when
    ``until`` is ``None`` (lifted only by the head or the reasons changing).

    A datetime ``until`` comes from ``Duration.until``, so a day-based snooze
    already lands on the morning it was meant for.

    ``reasons`` (the PR's attention reasons at snooze time) is stored sorted
    so a later set-equality check is order-independent; ``None`` omits the
    key, leaving a windowed entry on the head-and-window rule alone. An
    open-ended entry has no window to fall back on, so ``None`` reasons are
    refused there (``SnoozeError``): the caller must capture them or ask for
    a window instead.
    """
    if until is None and reasons is None:
        raise SnoozeError(
            "an open-ended snooze needs the PR's attention reasons; "
            "capture them or give it a window"
        )
    entry: SnoozeEntry = {
        "oid": oid,
        "until": None if until is None else until.isoformat(timespec="seconds"),
    }
    if reasons is not None:
        entry["reasons"] = sorted(reasons)
    return entry


def is_expired(entry: SnoozeEntry, now: datetime) -> bool:
    """True when the entry's window has elapsed.

    An open-ended entry (``until`` is ``None``) never expires: only its head
    or its reasons can lift it. A missing, unparseable, or naive stored
    timestamp counts as expired: fail-safe, the PR shows. ``now`` must be
    timezone-aware — the comparison happens outside the try so a naive
    ``now`` (a caller bug) raises loudly instead of silently expiring every
    entry in the store.
    """
    try:
        stored = entry["until"]
    except KeyError:
        return True
    if stored is None:
        return False
    try:
        until = datetime.fromisoformat(stored)
    except ValueError, TypeError:
        return True
    if until.tzinfo is None:
        return True
    return until <= now


def split_snoozed(
    prs: list[PullRequest], snoozes: dict[str, SnoozeEntry], now: datetime
) -> tuple[list[PullRequest], list[PullRequest], dict[str, str]]:
    """Partition PRs into (visible, hidden) and report dead snoozes.

    A PR is hidden only while its head oid is known and still equals the
    snoozed oid, the window (if any) has not elapsed, AND — when the entry
    recorded them — its attention reasons still match those acknowledged at
    snooze time. Dead entries — head moved, window elapsed (checked even for
    PRs absent from the search), reasons changed (e.g. a review landed and a
    waiting PR is now ready to merge), or a timestamp that can't be compared
    — come back as ``{url: reason}`` for the caller to prune. Live entries
    for absent PRs are kept: the PR may merely be beyond a truncated search.
    An open-ended entry never dies this way for an absent PR — nothing here
    can tell a closed PR from one beyond the cap — so entries for closed or
    merged PRs are the explicit ``snooze --prune``'s to clean up.
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
