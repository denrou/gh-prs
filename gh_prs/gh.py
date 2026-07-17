"""Thin wrapper around the gh CLI for GitHub pull requests."""

from __future__ import annotations

import json
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field

# gh check conclusions / statuses that mean a check has failed.
_FAILED_STATES = {
    "FAILURE",
    "ERROR",
    "TIMED_OUT",
    "CANCELLED",
    "ACTION_REQUIRED",
    "STARTUP_FAILURE",
    "STALE",
}
# States that mean a check is still running (not yet a pass or fail).
_PENDING_STATES = {
    "PENDING",
    "IN_PROGRESS",
    "QUEUED",
    "WAITING",
    "EXPECTED",
    "REQUESTED",
}


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
    head_ref: str = ""
    review_decision: str = ""
    mergeable: str = ""
    # "SUCCESS" | "FAILURE" | "PENDING" | "" (no checks configured)
    checks_state: str = ""
    roles: set[str] = field(default_factory=set)
    # Reasons this PR needs the current user's attention (e.g. {"review", "ready"}).
    # Empty until enrichment runs.
    attention_reasons: set[str] = field(default_factory=set)

    @classmethod
    def from_json(cls, data: dict) -> PullRequest:
        repo = data.get("repository", {})
        repo_name = (
            repo.get("nameWithOwner", "") if isinstance(repo, dict) else str(repo)
        )
        author = data.get("author", {})
        author_login = (
            author.get("login", "") if isinstance(author, dict) else str(author)
        )
        return cls(
            number=data["number"],
            repo=repo_name,
            title=data["title"],
            author=author_login,
            url=data.get("url", ""),
            updated_at=data.get("updatedAt", ""),
            created_at=data.get("createdAt", ""),
            is_draft=data.get("isDraft", False),
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
        """Return True if this PR requires action from the current user.

        Returns False until enrichment has run (conservative default).
        """
        return bool(self.attention_reasons)


def _run_gh(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            ["gh", *args],
            capture_output=True,
            text=True,
            check=check,
        )
    except FileNotFoundError:
        raise RuntimeError(
            "gh CLI not found. Install it from https://cli.github.com/"
        ) from None


def _search_prs(qualifier: str) -> list[PullRequest]:
    """Run a single gh search prs query."""
    result = _run_gh(
        "search",
        "prs",
        f"--{qualifier}=@me",
        "--state=open",
        "--json",
        "number,title,repository,author,url,updatedAt,createdAt,isDraft",
        "--limit",
        "200",
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to fetch PRs: {result.stderr.strip()}")
    raw = result.stdout.strip()
    if not raw:
        return []
    try:
        return [PullRequest.from_json(item) for item in json.loads(raw)]
    except (json.JSONDecodeError, KeyError) as e:
        raise RuntimeError(f"Failed to parse PR data: {e}") from None


def fetch_prs(qualifiers: list[str] | None = None) -> list[PullRequest]:
    """Fetch open PRs the current user is involved with.

    ``qualifiers`` selects which ``gh search prs`` filters to run (``author``,
    ``review-requested``, ``assignee``, ``involves``). Defaults to all four.
    """
    if qualifiers is None:
        qualifiers = ["author", "review-requested", "assignee", "involves"]
    seen: dict[str, PullRequest] = {}
    with ThreadPoolExecutor(max_workers=max(len(qualifiers), 1)) as pool:
        futures = {pool.submit(_search_prs, q): q for q in qualifiers}
        for future in as_completed(futures):
            qualifier = futures[future]
            try:
                for pr in future.result():
                    if pr.id in seen:
                        seen[pr.id].roles.add(qualifier)
                    else:
                        pr.roles.add(qualifier)
                        seen[pr.id] = pr
            except RuntimeError:
                pass
    # Drop PRs from archived repos. Check unique repos in parallel.
    repos = {pr.repo for pr in seen.values()}
    if not repos:
        return []
    with ThreadPoolExecutor(max_workers=min(len(repos), 8)) as pool:
        archived = {
            repo
            for repo, is_archived in pool.map(_is_repo_archived, repos)
            if is_archived
        }
    return sorted(
        (pr for pr in seen.values() if pr.repo not in archived),
        key=lambda p: p.updated_at,
        reverse=True,
    )


def _is_repo_archived(repo: str) -> tuple[str, bool]:
    result = _run_gh("repo", "view", repo, "--json", "isArchived", check=False)
    if result.returncode != 0 or not result.stdout.strip():
        return repo, False
    try:
        return repo, json.loads(result.stdout).get("isArchived", False)
    except json.JSONDecodeError:
        return repo, False


def get_current_user() -> str:
    """Return the login of the authenticated GitHub user."""
    result = _run_gh("api", "user", "--jq", ".login", check=False)
    if result.returncode == 0:
        return result.stdout.strip()
    return ""


def _rollup_state(rollup: list[dict]) -> str:
    """Collapse a statusCheckRollup list into SUCCESS / FAILURE / PENDING / "".

    An empty rollup ("") means no checks are configured on the PR.
    """
    if not rollup:
        return ""
    has_pending = False
    for check in rollup:
        # StatusContext exposes `state`; CheckRun exposes `status` + `conclusion`.
        state = check.get("state")
        if state:
            resolved = state.upper()
        elif (check.get("status") or "").upper() != "COMPLETED":
            resolved = "PENDING"
        else:
            resolved = (check.get("conclusion") or "").upper() or "PENDING"
        if resolved in _FAILED_STATES:
            return "FAILURE"
        if resolved in _PENDING_STATES or resolved == "":
            has_pending = True
    return "PENDING" if has_pending else "SUCCESS"


def enrich_pr(pr: PullRequest, current_user: str = "") -> None:
    """Fetch branch, review decision, CI status, and reviews for a single PR.

    Mutates ``pr`` in place and computes ``attention_reasons``.
    """
    try:
        detail = _run_gh(
            "pr",
            "view",
            str(pr.number),
            "--repo",
            pr.repo,
            "--json",
            "headRefName,reviewDecision,reviews,mergeable,statusCheckRollup",
            check=False,
        )
    except RuntimeError:
        return
    if detail.returncode != 0 or not detail.stdout.strip():
        return
    try:
        info = json.loads(detail.stdout)
    except json.JSONDecodeError:
        return
    pr.head_ref = info.get("headRefName", "")
    pr.review_decision = info.get("reviewDecision", "") or ""
    pr.mergeable = info.get("mergeable", "") or ""
    pr.checks_state = _rollup_state(info.get("statusCheckRollup", []) or [])

    if pr.is_draft:
        return

    reasons: set[str] = set()

    # --- PRs where I am asked to review ---
    reviews = info.get("reviews", [])
    my_reviews = [
        r for r in reviews if (r.get("author") or {}).get("login") == current_user
    ]
    # Only the most recent review matters: a later APPROVED supersedes an earlier
    # DISMISSED, so checking any() over all reviews would give false positives.
    latest_my_review = (
        max(my_reviews, key=lambda r: r.get("submittedAt", "")) if my_reviews else None
    )
    latest_state = latest_my_review.get("state") if latest_my_review else None
    user_has_active_review = latest_state in ("APPROVED", "CHANGES_REQUESTED")
    dismissed = latest_state == "DISMISSED"
    review_req = (
        "review-requested" in pr.roles
        and pr.review_decision in ("REVIEW_REQUIRED", "")
        and not user_has_active_review
    )
    # Skip conflicting PRs: a review would be staled once the author rebases.
    if (review_req or dismissed) and pr.mergeable != "CONFLICTING":
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

    pr.attention_reasons = reasons
