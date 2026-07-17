"""Thin wrapper around the gh CLI for GitHub pull requests."""

from __future__ import annotations

import json
import re
import subprocess
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field


class GhError(RuntimeError):
    """A gh CLI invocation failed (missing binary, auth, network, bad output)."""


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
    "assignee": "assignee:@me",
    "involves": "involves:@me",
}
ALL_QUALIFIERS: tuple[str, ...] = tuple(_SEARCH_FILTERS)

# GraphQL search returns at most 100 nodes per request; queries matching more
# open PRs than this are silently truncated.
_SEARCH_LIMIT = 100

# GraphQL statusCheckRollup.state → our normalized checks_state. Unknown
# states map to PENDING so "unrecognized" can never mean "pass".
_ROLLUP_STATE = {
    "SUCCESS": "SUCCESS",
    "FAILURE": "FAILURE",
    "ERROR": "FAILURE",
    "PENDING": "PENDING",
    "EXPECTED": "PENDING",
}

# The first: 50 caps on reviewRequests/latestReviews silently truncate on PRs
# with more than 50 requested reviewers or reviewers; the viewer's own entry
# could then be missed (false-negative review detection). Accepted trade-off.
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
  repository { nameWithOwner }
  author { login }
  reviewRequests(first: 50) {
    nodes { requestedReviewer { __typename ... on User { login } } }
  }
  latestReviews(first: 50) { nodes { author { login } state } }
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


@dataclass
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
    # True when the current user is personally on the requested-reviewers
    # list (not merely through a team).
    review_requested_explicitly: bool = False
    roles: set[str] = field(default_factory=set)
    # Reasons this PR needs the current user's attention (e.g. {"review", "ready"}).
    attention_reasons: set[str] = field(default_factory=set)

    @classmethod
    def from_graphql(cls, node: dict, current_user: str = "") -> PullRequest:
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
        for review in (node.get("latestReviews") or {}).get("nodes") or []:
            if ((review or {}).get("author") or {}).get("login") == current_user:
                my_review_state = review.get("state") or ""
                break

        # Only User reviewers carry a login in the fragment; a request routed
        # through a Team therefore never matches.
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
            review_requested_explicitly=explicit,
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
    return f"is:pr is:open archived:false {_SEARCH_FILTERS[qualifier]}"


def _graphql(context: str, *args: str) -> dict:
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


def _search(qualifier: str) -> tuple[str, list[dict], int]:
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
) -> list[PullRequest]:
    """Fetch open PRs the current user is involved with, fully enriched.

    Runs one ``gh api graphql`` search per qualifier (``author``,
    ``review-requested``, ``assignee``, ``involves`` — defaults to all four)
    in parallel; GitHub executes aliased searches sequentially, so separate
    requests cost the slowest search instead of the sum. Each search is
    capped at 100 PRs and fetches everything in one shot: review decision,
    mergeability, CI rollup, latest reviews, and review requests. Archived
    repos are excluded by the search filter. Each PR's ``attention_reasons``
    is computed before returning.

    ``on_warning`` (if given) receives a message when a search matched more
    PRs than the cap, so truncation is informed rather than silent.

    Raises ``GhError`` if any search fails or returns unparseable data: a
    partial result would silently hide PRs, and "error" must never look like
    "nothing to do".
    """
    if qualifiers is None:
        qualifiers = list(ALL_QUALIFIERS)
    results: dict[str, tuple[str, list[dict], int]] = {}
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
            except (KeyError, TypeError) as e:
                raise GhError(f"Failed to parse PR data: {e!r}") from e
            if pr.id in seen:
                seen[pr.id].roles.add(qualifier)
            else:
                pr.roles.add(qualifier)
                seen[pr.id] = pr

    for pr in seen.values():
        pr.attention_reasons = _attention_reasons(pr)
    return sorted(seen.values(), key=lambda p: p.updated_at, reverse=True)


def _attention_reasons(pr: PullRequest) -> set[str]:
    """Compute why an enriched PR needs the current user's attention.

    Pure function of the PR's enriched fields. Drafts never need attention.
    """
    if pr.is_draft:
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

    return reasons
