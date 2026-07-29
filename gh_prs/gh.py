"""Thin wrapper around the gh CLI for GitHub pull requests."""

import json
import re
import subprocess
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from typing import Any


class GhError(RuntimeError):
    """A gh CLI invocation failed (missing binary, auth, network, bad output)."""


# Default silence before an authored PR is flagged 'stale' (still awaiting
# review — a nudge to ping the reviewers) or 'stale-draft' (still a draft — a
# nudge to finish it or mark it ready). Overridable via config / CLI.
DEFAULT_STALE_AFTER = timedelta(days=3)


# C0 control characters, DEL, and C1 controls (U+0080–U+009F). Rich strips
# most C0 but notably not ESC (0x1b), and no C1 (e.g. U+009B, a one-char CSI),
# so a crafted PR title could otherwise inject raw terminal escape sequences.
_CONTROL_CHARS = re.compile(r"[\x00-\x1f\x7f-\x9f]")

# Seconds before a stuck gh subprocess is aborted (stalled network etc.).
_GH_TIMEOUT = 60

# GraphQL search filter per supported qualifier.
_SEARCH_FILTERS = {
    "author": "author:@me",
    "review-requested": "review-requested:@me",
    "reviewed-by": "reviewed-by:@me",
    "assignee": "assignee:@me",
    "involves": "involves:@me",
}
ALL_QUALIFIERS: tuple[str, ...] = tuple(_SEARCH_FILTERS)

# GraphQL search returns at most 100 nodes per request; queries matching more
# open PRs than this are silently truncated.
_SEARCH_LIMIT = 100

# Per-PR cap on the review connections. Must stay in sync with the `first:`
# arguments in _PR_FRAGMENT — pinned by a test, since the fragment is a plain
# literal (interpolating it would mean escaping every GraphQL brace).
_REVIEW_PAGE_LIMIT = 50

# GraphQL statusCheckRollup.state → our normalized checks_state. Unknown
# states map to PENDING so "unrecognized" can never mean "pass".
_ROLLUP_STATE = {
    "SUCCESS": "SUCCESS",
    "FAILURE": "FAILURE",
    "ERROR": "FAILURE",
    "PENDING": "PENDING",
    "EXPECTED": "PENDING",
}

# The first: 50 caps on reviewRequests/latestReviews/latestOpinionatedReviews
# silently truncate on PRs with more than 50 requested reviewers or reviewers;
# the viewer's own entry could then be missed (false-negative review and
# new-commits detection). fetch_prs surfaces the reviewed-by contradiction
# through on_warning. A truncated changes-requested review could otherwise
# *fabricate* the 'stale' nudge rather than suppress it — `all()` over a
# truncated set is weaker than over the whole one — so from_graphql records an
# unknown marker at the cap; see changes_requested_commits.
_PR_FRAGMENT = """
fragment prFields on PullRequest {
  number
  title
  url
  updatedAt
  createdAt
  isDraft
  reviewDecision
  mergeable
  headRefOid
  repository { nameWithOwner }
  author { login }
  reviewRequests(first: 50) {
    nodes { requestedReviewer { __typename ... on User { login } } }
  }
  latestReviews(first: 50) { nodes { author { login } state commit { oid } } }
  latestOpinionatedReviews(first: 50) { nodes { state commit { oid } } }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}
"""

# Static query parametrized with GraphQL variables (bound via gh's -f/-F
# flags), so no untrusted or dynamic text is ever spliced into the query.
_SEARCH_QUERY = (
    """
query($q: String!, $limit: Int!) {
  viewer { login }
  results: search(query: $q, type: ISSUE, first: $limit) {
    issueCount
    nodes { ...prFields }
  }
}
"""
    + _PR_FRAGMENT
)


@dataclass(slots=True)
class PullRequest:
    number: int
    repo: str
    title: str
    author: str
    url: str
    updated_at: str
    created_at: str
    is_draft: bool
    review_decision: str = ""
    mergeable: str = ""
    # "SUCCESS" | "FAILURE" | "PENDING" | "" (no checks configured)
    checks_state: str = ""
    # State of the current user's latest review ("APPROVED", "DISMISSED", …),
    # or "" if they never reviewed.
    my_review_state: str = ""
    # Oid of the commit that review was submitted against, "" if they never
    # reviewed or GitHub no longer links one (e.g. it was force-pushed away).
    my_review_commit: str = ""
    # Head commit oid of the PR branch, "" if unavailable.
    head_ref_oid: str = ""
    # True when the current user is personally on the requested-reviewers
    # list (not merely through a team).
    review_requested_explicitly: bool = False
    # Oid of the commit each *standing* CHANGES_REQUESTED review was submitted
    # against. An "" entry means "one stands, but against an unknown commit" —
    # GitHub no longer links one, the node was null, or the 50-node cap may be
    # hiding a review. Empty when nobody stands on changes-requested *or* the
    # data is missing entirely, so consumers must read empty as "unknown", not
    # as "nobody objects". Sourced from latestOpinionatedReviews, which unlike
    # latestReviews survives a re-request (see from_graphql).
    changes_requested_commits: tuple[str, ...] = ()
    # True when at least one review request is pending, from a user or a team.
    has_pending_review_request: bool = False
    roles: set[str] = field(default_factory=set)
    # Reasons this PR needs the current user's attention (e.g. {"review", "ready"}).
    attention_reasons: set[str] = field(default_factory=set)

    @classmethod
    def from_graphql(
        cls, node: dict[str, Any], current_user: str = ""
    ) -> "PullRequest":
        commits = node.get("commits")
        if not isinstance(commits, dict):
            # Shape drift: the commits block should always be present. Unknown
            # must never read as "no checks" (which would count toward ready).
            checks_state = "PENDING"
        else:
            nodes = commits.get("nodes") or [None]
            rollup = ((nodes[0] or {}).get("commit") or {}).get("statusCheckRollup")
            if rollup:
                state = (rollup.get("state") or "").upper()
                checks_state = _ROLLUP_STATE.get(state, "PENDING")
            else:
                # A present commit with a null rollup is the legitimate
                # "no checks configured" case.
                checks_state = ""

        # latestReviews already collapses to each reviewer's most recent review.
        my_review_state = ""
        my_review_commit = ""
        for review in (node.get("latestReviews") or {}).get("nodes") or []:
            if ((review or {}).get("author") or {}).get("login") == current_user:
                my_review_state = review.get("state") or ""
                my_review_commit = ((review.get("commit") or {}).get("oid")) or ""
                break

        # Standing changes-requested reviews — normally the ones reviewDecision
        # is derived from, though the two can disagree (reviewDecision also
        # answers to branch protection, and this connection isn't limited to
        # writers), which is exactly why _changes_requested_addressed treats an
        # empty list as "unknown" rather than "nobody objects".
        #
        # latestOpinionatedReviews — not latestReviews — is the source, because
        # it survives a re-request. GitHub documents latestReviews as the
        # reviews "that are not also pending review", so re-requesting a
        # reviewer drops their review from it entirely: precisely the "author
        # answered and handed it back" state the 'stale' nudge must recognize.
        # Observed (the field is undocumented beyond its name): it keeps each
        # reviewer's most recent opinionated review, and a later comment-review
        # doesn't unseat it. Only CHANGES_REQUESTED entries are collected here.
        opinionated = (node.get("latestOpinionatedReviews") or {}).get("nodes") or []
        changes_requested_commits = [
            ((review or {}).get("commit") or {}).get("oid") or ""
            for review in opinionated
            # A null node is shape drift, not an approval: keep it as an
            # unknown ("") slot rather than dropping it, which would let the
            # nudge read the survivors as the whole set.
            if review is None or review.get("state") == "CHANGES_REQUESTED"
        ]
        # At the cap, a standing changes-requested review may be hidden. Record
        # one unknown slot so _changes_requested_addressed reads "not answered"
        # instead of assuming the visible reviews are all of them. Skipped when
        # nothing is standing — an empty list already reads as unknown.
        if len(opinionated) >= _REVIEW_PAGE_LIMIT and changes_requested_commits:
            changes_requested_commits.append("")

        # Only User reviewers carry a login in the fragment; a request routed
        # through a Team therefore never matches `explicit` — but it is still
        # a pending request, so it counts toward has_pending_review_request.
        requests = (node.get("reviewRequests") or {}).get("nodes") or []
        explicit = bool(current_user) and any(
            ((r or {}).get("requestedReviewer") or {}).get("login") == current_user
            for r in requests
        )

        return cls(
            number=node["number"],
            # repository is an identity field (it keys de-duplication); treat
            # it as required like number/title — a null raises TypeError,
            # which fetch_prs converts to GhError.
            repo=node["repository"]["nameWithOwner"],
            title=_CONTROL_CHARS.sub("", node["title"]),
            author=(node.get("author") or {}).get("login", ""),
            url=node.get("url", ""),
            updated_at=node.get("updatedAt", ""),
            created_at=node.get("createdAt", ""),
            is_draft=node.get("isDraft", False),
            review_decision=node.get("reviewDecision") or "",
            mergeable=node.get("mergeable") or "",
            checks_state=checks_state,
            my_review_state=my_review_state,
            my_review_commit=my_review_commit,
            head_ref_oid=node.get("headRefOid") or "",
            review_requested_explicitly=explicit,
            changes_requested_commits=tuple(changes_requested_commits),
            has_pending_review_request=bool(requests),
        )

    @property
    def repo_short(self) -> str:
        return self.repo.split("/")[-1]

    @property
    def created_date(self) -> str:
        return self.created_at.split("T")[0]

    @property
    def updated_date(self) -> str:
        return self.updated_at.split("T")[0]

    @property
    def id(self) -> str:
        return f"{self.repo}#{self.number}"

    def needs_attention(self) -> bool:
        """Return True if this PR requires action from the current user."""
        return bool(self.attention_reasons)


def _run_gh(*args: str) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            ["gh", *args],
            capture_output=True,
            text=True,
            check=False,
            timeout=_GH_TIMEOUT,
        )
    except FileNotFoundError:
        raise GhError(
            "gh CLI not found. Install it from https://cli.github.com/"
        ) from None
    except subprocess.TimeoutExpired:
        raise GhError(f"gh timed out after {_GH_TIMEOUT}s (network stalled?)") from None
    except OSError as e:
        raise GhError(f"Failed to run gh: {e}") from None


def _search_string(qualifier: str) -> str:
    # sort:updated-desc makes _SEARCH_LIMIT truncation keep the most recently
    # updated PRs — the ones most likely to need attention (and makes the
    # truncation warning's "newest" claim literal).
    return (
        f"is:pr is:open archived:false sort:updated-desc {_SEARCH_FILTERS[qualifier]}"
    )


def _graphql(context: str, *args: str) -> dict[str, Any]:
    """Run a gh GraphQL request and return its validated ``data`` block.

    Every deviation from the expected response envelope raises ``GhError`` —
    a mangled or drifted response must never silently read as an empty result.
    """
    result = _run_gh("api", "graphql", *args)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise GhError(f"{context} failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise GhError(f"{context}: invalid JSON from gh: {e}") from e
    if not isinstance(payload, dict):
        raise GhError(f"{context}: unexpected response from gh (not a JSON object)")
    if payload.get("errors"):
        messages = "; ".join(
            err.get("message", "unknown error") if isinstance(err, dict) else str(err)
            for err in payload["errors"]
        )
        raise GhError(f"{context} returned errors: {messages}")
    data = payload.get("data")
    if not isinstance(data, dict):
        raise GhError(f"{context}: response has no data block (unexpected shape)")
    return data


_COUNT_QUERY = """
query($q: String!) {
  results: search(query: $q, type: ISSUE, first: 1) { issueCount }
}
"""


def count_prs(qualifier: str) -> int:
    """Return the exact number of open PRs matching one qualifier's search.

    A count-only query skips node hydration, which is what dominates search
    cost — measured ~0.3s versus 2s+ for a full search. Only valid for a
    single qualifier: counts across several searches can't be de-duplicated.
    ``issueCount`` is exact even beyond the ``_SEARCH_LIMIT`` node cap.

    Raises ``GhError`` on any failure.
    """
    data = _graphql(
        f"Count '{qualifier}'",
        "-f",
        f"query={_COUNT_QUERY}",
        "-f",
        f"q={_search_string(qualifier)}",
    )
    results = data.get("results")
    if not isinstance(results, dict) or "issueCount" not in results:
        raise GhError(
            f"Count '{qualifier}': response has no issueCount (unexpected shape)"
        )
    return int(results["issueCount"] or 0)


def fetch_pr_head(url: str) -> str:
    """Return the head commit oid of one PR, looked up by URL.

    Raises ``GhError`` on any failure, including a missing oid in the
    response: a snooze recorded against an unknown head could hide newer work.
    """
    result = _run_gh("pr", "view", url, "--json", "headRefOid")
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise GhError(f"Lookup of {url} failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise GhError(f"Lookup of {url}: invalid JSON from gh: {e}") from e
    oid = payload.get("headRefOid") if isinstance(payload, dict) else None
    if not oid or not isinstance(oid, str):
        raise GhError(f"Lookup of {url}: response has no headRefOid")
    return oid


def resolve_pr(ref: str, repo: str | None = None) -> tuple[str, str]:
    """Resolve a PR reference to its ``(canonical url, head commit oid)``.

    ``ref`` is anything ``gh pr view`` accepts — most usefully a bare PR
    number, but also a full URL or branch name. ``repo`` (``owner/repo``, or
    ``host/owner/repo``) scopes a bare number; when omitted, gh resolves the
    repository from the current directory (git remotes, ``GH_REPO``) exactly
    as every other gh command does. Delegating to gh means the returned url
    carries the correct host, so enterprise instances work without any
    host-specific URL construction here.

    Raises ``GhError`` on any failure, including a missing url or oid in the
    response: a snooze recorded against an unknown head could hide newer work.
    """
    args = ["pr", "view", ref, "--json", "url,headRefOid"]
    if repo:
        args += ["--repo", repo]
    result = _run_gh(*args)
    where = f" in {repo}" if repo else ""
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise GhError(f"Lookup of PR {ref}{where} failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise GhError(f"Lookup of PR {ref}{where}: invalid JSON from gh: {e}") from e
    if not isinstance(payload, dict):
        raise GhError(f"Lookup of PR {ref}{where}: unexpected response from gh")
    url = payload.get("url")
    oid = payload.get("headRefOid")
    if not isinstance(url, str) or not url:
        raise GhError(f"Lookup of PR {ref}{where}: response has no url")
    if not isinstance(oid, str) or not oid:
        raise GhError(f"Lookup of PR {ref}{where}: response has no headRefOid")
    return url, oid


def _search(qualifier: str) -> tuple[str, list[dict[str, Any]], int]:
    """Run one qualifier's search; return (viewer_login, PR nodes, issue_count).

    ``issue_count`` is the exact server-side match count, which can exceed the
    ``_SEARCH_LIMIT`` cap on returned nodes.
    """
    data = _graphql(
        f"Search '{qualifier}'",
        "-f",
        f"query={_SEARCH_QUERY}",
        "-f",
        f"q={_search_string(qualifier)}",
        "-F",
        f"limit={_SEARCH_LIMIT}",
    )
    results = data.get("results")
    if not isinstance(results, dict):
        raise GhError(
            f"Search '{qualifier}': response has no results block (unexpected shape)"
        )
    viewer = (data.get("viewer") or {}).get("login", "")
    nodes = results.get("nodes") or []
    issue_count = results.get("issueCount") or 0
    return viewer, nodes, issue_count


def fetch_prs(
    qualifiers: list[str] | None = None,
    on_warning: Callable[[str], None] | None = None,
    stale_after: timedelta | None = DEFAULT_STALE_AFTER,
) -> list[PullRequest]:
    """Fetch open PRs the current user is involved with, fully enriched.

    Runs one ``gh api graphql`` search per qualifier (``author``,
    ``review-requested``, ``reviewed-by``, ``assignee``, ``involves`` —
    defaults to all five) in parallel; GitHub executes aliased searches
    sequentially, so separate
    requests cost the slowest search instead of the sum. Each search is
    capped at 100 PRs and fetches everything in one shot: review decision,
    mergeability, CI rollup, latest reviews, and review requests. Archived
    repos are excluded by the search filter. Each PR's ``attention_reasons``
    is computed before returning.

    ``stale_after`` is the silence threshold for the 'stale' nudge on
    authored PRs still awaiting review and the 'stale-draft' nudge on
    authored drafts; ``None`` disables both reasons.

    ``on_warning`` (if given) receives a message when a search matched more
    PRs than the cap, and when a PR matched by ``reviewed-by`` carries no
    parsable own-review (the ``latestReviews`` 50-node cap hid it, so
    new-commit detection may miss that PR) — either way, degraded coverage
    is informed rather than silent.

    Raises ``GhError`` if any search fails or returns unparseable data: a
    partial result would silently hide PRs, and "error" must never look like
    "nothing to do".
    """
    if qualifiers is None:
        qualifiers = list(ALL_QUALIFIERS)
    results: dict[str, tuple[str, list[dict[str, Any]], int]] = {}
    errors: list[str] = []
    with ThreadPoolExecutor(max_workers=max(len(qualifiers), 1)) as pool:
        futures = {pool.submit(_search, q): q for q in qualifiers}
        for future in as_completed(futures):
            qualifier = futures[future]
            try:
                results[qualifier] = future.result()
            except GhError as exc:
                errors.append(str(exc))
            except Exception as exc:  # keep the aggregation exhaustive
                errors.append(f"Search '{qualifier}' crashed: {exc!r}")
    if errors:
        raise GhError(
            "Some PR searches failed; results would be incomplete:\n  "
            + "\n  ".join(errors)
        )

    # viewer.login is non-null in GitHub's schema; its absence means the
    # response can't be trusted, and an empty login would silently disable
    # my_review_state / review_requested_explicitly classification.
    viewer = next((login for login, _, _ in results.values() if login), "")
    if results and not viewer:
        raise GhError("Could not determine the authenticated user from gh's response")

    if on_warning is not None:
        for qualifier in qualifiers:
            issue_count = results[qualifier][2]
            if issue_count > _SEARCH_LIMIT:
                on_warning(
                    f"search '{qualifier}' matched {issue_count} PRs; "
                    f"showing the newest {_SEARCH_LIMIT}"
                )

    seen: dict[str, PullRequest] = {}
    # Iterate in qualifier order (not completion order) so the same search's
    # node deterministically provides each PR's field values (first-seen wins;
    # two searches can return slightly different snapshots of the same PR).
    for qualifier in qualifiers:
        _, nodes, _ = results[qualifier]
        for node in nodes:
            if not node:
                continue
            try:
                pr = PullRequest.from_graphql(node, viewer)
            # AttributeError covers a block arriving as a non-dict (`.get` on a
            # list): the module's contract is that any deviation from the
            # expected envelope surfaces as GhError, never a raw traceback.
            except (KeyError, TypeError, AttributeError) as e:
                raise GhError(f"Failed to parse PR data: {e!r}") from e
            if pr.id in seen:
                seen[pr.id].roles.add(qualifier)
            else:
                pr.roles.add(qualifier)
                seen[pr.id] = pr

    now = datetime.now(UTC)
    for pr in seen.values():
        pr.attention_reasons = _attention_reasons(pr, now=now, stale_after=stale_after)
        # The reviewed-by search positively asserts I reviewed this PR; an
        # empty my_review_state therefore means the latestReviews 50-node cap
        # hid my review — a contradiction that would otherwise silently
        # disable new-commit detection for this PR. Drafts never flag
        # new-commits anyway.
        if (
            on_warning is not None
            and not pr.is_draft
            and "reviewed-by" in pr.roles
            and not pr.my_review_state
        ):
            on_warning(
                f"{pr.id}: your review is not among its first 50 latest "
                "reviews; new-commit detection may miss it"
            )
    return sorted(seen.values(), key=lambda p: p.updated_at, reverse=True)


def _is_stale(updated_at: str, now: datetime, stale_after: timedelta) -> bool:
    """True when ``updated_at`` is older than ``stale_after`` relative to ``now``.

    The fail direction is deliberately the opposite of everywhere else in this
    module: a missing, unparseable, or naive timestamp returns False (no
    nudge), not True. The 'stale' reason is additive and non-actionable —
    defaulting an unknown age to 'stale' would fabricate a reason on a
    possibly-fresh PR and cry wolf, so uncertainty stays quiet here rather
    than showing. ``now`` must be timezone-aware.
    """
    try:
        updated = datetime.fromisoformat(updated_at)
    except ValueError, TypeError:
        return False
    if updated.tzinfo is None:
        return False
    return now - updated >= stale_after


def _changes_requested_addressed(pr: PullRequest) -> bool:
    """True when a changes-requested review stands and none is against the head.

    Read as "the author has pushed something since the reviewers said no". It
    does not (and cannot) prove the new commits actually fix anything — only
    that no standing review still describes the current state of the branch.
    Commit identity is compared, not order, so a force-push that rewinds the
    branch also counts (as it should — the reviewed state is gone either way).

    Inherits _is_stale's quiet fail direction rather than the module's usual
    one, because its only caller is the 'stale' nudge. Every uncertainty
    returns False — "not addressed", nudge stays silent: no standing review in
    hand at all (nobody objects, the data is missing, or the cap hid them all),
    no head oid to compare against, or any review whose commit is unknown —
    including the marker from_graphql records when the 50-node cap may be
    hiding one. Fabricating a nudge on a PR that is genuinely the author's to
    rework is the failure this direction exists to prevent.
    """
    if not pr.changes_requested_commits or not pr.head_ref_oid:
        return False
    return all(oid and oid != pr.head_ref_oid for oid in pr.changes_requested_commits)


def _awaiting_review(pr: PullRequest) -> bool:
    """True when an authored PR is still waiting on its reviewers.

    Not yet APPROVED (that's waiting to merge, not a reviewer nudge), and not a
    CHANGES_REQUESTED the author is still reworking — with one carve-out.
    GitHub keeps reporting CHANGES_REQUESTED long after the author has answered
    it: the decision clears only when a reviewer submits a new *opinionated*
    review (a comment-review doesn't unseat it) or someone dismisses theirs, so
    a re-requested reviewer who never comes back leaves it stuck indefinitely.

    The ball counts as back in the reviewers' court when every standing
    changes-requested review is against a superseded commit and a review
    request is pending. That second half is deliberately weak: reviewRequests
    carries no timestamp, so a request pending since the PR opened can't be
    told apart from a fresh re-request. It still means somebody is on the hook,
    which is enough for an additive nudge — but it does not prove the author
    handed the PR back.
    """
    if pr.review_decision == "APPROVED":
        return False
    if pr.review_decision != "CHANGES_REQUESTED":
        return True
    return pr.has_pending_review_request and _changes_requested_addressed(pr)


def _attention_reasons(
    pr: PullRequest,
    now: datetime | None = None,
    stale_after: timedelta | None = None,
) -> set[str]:
    """Compute why an enriched PR needs the current user's attention.

    Pure function of the PR's enriched fields plus the clock. Drafts are
    deliberately parked WIP, so only two reasons can fire on them, both for
    the author: 'conflict' and the 'stale-draft' nudge. The 'stale' and
    'stale-draft' nudges only fire when both ``now`` and ``stale_after`` are
    supplied; omitting either disables them (so a bare
    ``_attention_reasons(pr)`` never returns either).
    """
    if pr.is_draft:
        # A draft is deliberately parked WIP: review, new-commits, ci-failed
        # (red CI is expected while iterating), and ready never apply. Two
        # authored-draft cases still warrant action: conflicts (the base
        # moved underneath it — resolving early is cheaper than later), and
        # a draft untouched past the staleness threshold (likely forgotten —
        # time to finish it or mark it ready). Conflict takes precedence,
        # mirroring 'stale'; the nudge inherits _is_stale's quiet fail
        # direction and the now/stale_after gating.
        if "author" not in pr.roles:
            return set()
        if pr.mergeable == "CONFLICTING":
            return {"conflict"}
        if (
            now is not None
            and stale_after is not None
            and _is_stale(pr.updated_at, now, stale_after)
        ):
            return {"stale-draft"}
        return set()

    reasons: set[str] = set()

    # --- PRs where I am asked to review ---
    has_active_review = pr.my_review_state in ("APPROVED", "CHANGES_REQUESTED")
    dismissed = pr.my_review_state == "DISMISSED"
    wants_my_review = (
        "review-requested" in pr.roles or dismissed
    ) and not has_active_review
    if (
        wants_my_review
        # A review would be staled once the author rebases. UNKNOWN (GitHub
        # still computing mergeability) deliberately stays visible here: a
        # review request shouldn't vanish while GitHub churns.
        and pr.mergeable != "CONFLICTING"
        # The author is already reworking the PR.
        and pr.review_decision != "CHANGES_REQUESTED"
        # Approved PRs are mergeable without me — unless I'm personally on the
        # requested-reviewers list (not just through a team), my review is moot.
        and (pr.review_decision != "APPROVED" or pr.review_requested_explicitly)
    ):
        reasons.add("review")

    # --- PRs I already reviewed that moved on without a re-request ---
    # Fires when the head commit is no longer the commit my review was
    # submitted against — new commits, or a rebase/force-push that staled the
    # review — and the author never re-requested. Commit identity is compared
    # rather than committedDate: committer timestamps are mutable metadata,
    # while oids pin the exact reviewed state. A missing oid on either side
    # still counts as "moved" (unknown must never read as "nothing to do");
    # only with no oid at all is there nothing to compare. DISMISSED is
    # included for repos that auto-dismiss stale reviews on push; the
    # "review" reason keeps precedence, so a PR is never listed twice.
    if (
        "review" not in reasons
        and pr.my_review_state
        in ("APPROVED", "CHANGES_REQUESTED", "COMMENTED", "DISMISSED")
        # A comment-review on my own PR must not self-flag.
        and "author" not in pr.roles
        and (pr.my_review_commit or pr.head_ref_oid)
        and pr.my_review_commit != pr.head_ref_oid
        # Once conflicting, more commits are coming; reviewing now is premature
        # (same reasoning as for "review").
        and pr.mergeable != "CONFLICTING"
    ):
        reasons.add("new-commits")

    # --- PRs I authored that need my action ---
    if "author" in pr.roles:
        # Conflicts and failing CI are independent actions; a PR can need both.
        if pr.mergeable == "CONFLICTING":
            reasons.add("conflict")
        if pr.checks_state == "FAILURE":
            reasons.add("ci-failed")
        elif (
            pr.review_decision == "APPROVED"
            and pr.checks_state in ("SUCCESS", "")
            # Require a positive MERGEABLE: UNKNOWN (mergeability still being
            # computed, e.g. right after a push) must not read as "no
            # conflict" — same fail-safe direction as _ROLLUP_STATE.
            and pr.mergeable == "MERGEABLE"
        ):
            reasons.add("ready")

        # A soft nudge for a PR that is simply waiting on reviewers, too long:
        # nothing above fired (so there's no action for me — no conflict,
        # failing CI, or ready-to-ship), it is still awaiting review (see
        # _awaiting_review for the CHANGES_REQUESTED carve-out), yet it has sat
        # untouched past the staleness threshold. Time to ping the reviewers.
        # Unlike the reasons above, an unknown age never fires this (see
        # _is_stale); disabled when now/stale_after aren't supplied.
        if (
            not reasons
            and now is not None
            and stale_after is not None
            and _awaiting_review(pr)
            and _is_stale(pr.updated_at, now, stale_after)
        ):
            reasons.add("stale")

    return reasons
