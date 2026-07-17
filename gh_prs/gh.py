"""Thin wrapper around the gh CLI for GitHub pull requests."""

from __future__ import annotations

import json
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field


class GhError(RuntimeError):
    """A gh CLI invocation failed (missing binary, auth, network, bad output)."""


# C0 control characters and DEL. Rich strips most but notably not ESC (0x1b),
# so a crafted PR title could otherwise inject raw terminal escape sequences.
_CONTROL_CHARS = re.compile(r"[\x00-\x1f\x7f]")

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
        commits = (node.get("commits") or {}).get("nodes") or [None]
        rollup = ((commits[0] or {}).get("commit") or {}).get("statusCheckRollup")
        if rollup:
            state = (rollup.get("state") or "").upper()
            checks_state = _ROLLUP_STATE.get(state, "PENDING")
        else:
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
            repo=(node.get("repository") or {}).get("nameWithOwner", ""),
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


def _build_query(qualifier: str) -> str:
    """Build the GraphQL query for one qualifier's search."""
    return (
        "query {\n"
        "  viewer { login }\n"
        f'  results: search(query: "is:pr is:open archived:false '
        f'{_SEARCH_FILTERS[qualifier]}", type: ISSUE, first: {_SEARCH_LIMIT}) '
        "{ nodes { ...prFields } }\n"
        "}\n"
        f"{_PR_FRAGMENT}"
    )


def _search(qualifier: str) -> tuple[str, list[dict]]:
    """Run one qualifier's GraphQL search; return (viewer_login, PR nodes)."""
    result = _run_gh("api", "graphql", "-f", f"query={_build_query(qualifier)}")
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise GhError(f"Search '{qualifier}' failed: {detail}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise GhError(f"Invalid JSON from '{qualifier}' search: {e}") from e
    if payload.get("errors"):
        messages = "; ".join(
            err.get("message", "unknown error") for err in payload["errors"]
        )
        raise GhError(f"Search '{qualifier}' returned errors: {messages}")
    data = payload.get("data") or {}
    viewer = (data.get("viewer") or {}).get("login", "")
    nodes = (data.get("results") or {}).get("nodes") or []
    return viewer, nodes


def fetch_prs(qualifiers: list[str] | None = None) -> list[PullRequest]:
    """Fetch open PRs the current user is involved with, fully enriched.

    Runs one ``gh api graphql`` search per qualifier (``author``,
    ``review-requested``, ``assignee``, ``involves`` — defaults to all four)
    in parallel; GitHub executes aliased searches sequentially, so separate
    requests cost the slowest search instead of the sum. Each search is
    capped at 100 PRs and fetches everything in one shot: review decision,
    mergeability, CI rollup, latest reviews, and review requests. Archived
    repos are excluded by the search filter. Each PR's ``attention_reasons``
    is computed before returning.

    Raises ``GhError`` if any search fails: a partial result would silently
    hide PRs, and "error" must never look like "nothing to do".
    """
    if qualifiers is None:
        qualifiers = list(ALL_QUALIFIERS)
    results: dict[str, tuple[str, list[dict]]] = {}
    errors: list[str] = []
    with ThreadPoolExecutor(max_workers=max(len(qualifiers), 1)) as pool:
        futures = {pool.submit(_search, q): q for q in qualifiers}
        for future in as_completed(futures):
            qualifier = futures[future]
            try:
                results[qualifier] = future.result()
            except GhError as exc:
                errors.append(str(exc))
    if errors:
        raise GhError(
            "Some PR searches failed; results would be incomplete:\n  "
            + "\n  ".join(errors)
        )

    viewer = next((login for login, _ in results.values() if login), "")
    seen: dict[str, PullRequest] = {}
    # Iterate in qualifier order (not completion order) for deterministic roles.
    for qualifier in qualifiers:
        _, nodes = results[qualifier]
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
        # A review would be staled once the author rebases.
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
            and pr.mergeable != "CONFLICTING"
        ):
            reasons.add("ready")

    return reasons
