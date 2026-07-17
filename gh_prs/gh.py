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
    # Non-empty if enrichment failed; the PR's decision/CI/mergeable fields are
    # then unknown, which is different from legitimately empty.
    enrich_error: str = ""

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
            title=_CONTROL_CHARS.sub("", data["title"]),
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
        raise GhError(
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
        raise GhError(f"Search '--{qualifier}=@me' failed: {result.stderr.strip()}")
    raw = result.stdout.strip()
    if not raw:
        return []
    try:
        return [PullRequest.from_json(item) for item in json.loads(raw)]
    except (json.JSONDecodeError, KeyError) as e:
        raise GhError(f"Failed to parse '--{qualifier}=@me' PR data: {e}") from e


def fetch_prs(qualifiers: list[str] | None = None) -> list[PullRequest]:
    """Fetch open PRs the current user is involved with.

    ``qualifiers`` selects which ``gh search prs`` filters to run (``author``,
    ``review-requested``, ``assignee``, ``involves``). Defaults to all four.

    Raises ``GhError`` if any search query fails: a partial result (e.g. a
    rate-limited ``review-requested`` query) would silently hide PRs, and
    "error" must never look like "nothing to do".
    """
    if qualifiers is None:
        qualifiers = ["author", "review-requested", "assignee", "involves"]
    seen: dict[str, PullRequest] = {}
    errors: list[str] = []
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
            except GhError as exc:
                errors.append(str(exc))
    if errors:
        raise GhError(
            "Some PR searches failed; results would be incomplete:\n  "
            + "\n  ".join(errors)
        )
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
    """Return the login of the authenticated GitHub user.

    Raises ``GhError`` on failure: an empty login would silently break the
    "did I already review this?" logic in ``enrich_pr``, and gh's own stderr
    (e.g. "run gh auth login") is the actionable message the user needs.
    """
    result = _run_gh("api", "user", "--jq", ".login", check=False)
    if result.returncode != 0:
        raise GhError(f"Could not resolve GitHub user: {result.stderr.strip()}")
    return result.stdout.strip()


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
    """Fetch review decision, mergeability, CI status, and reviews for one PR.

    Mutates ``pr`` in place and computes ``attention_reasons``. Drafts get
    enriched fields but never attention reasons. On failure, sets
    ``pr.enrich_error`` instead of raising so one bad PR doesn't sink the run.
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
    except GhError as exc:
        pr.enrich_error = str(exc)
        return
    if detail.returncode != 0 or not detail.stdout.strip():
        pr.enrich_error = detail.stderr.strip() or "gh pr view returned no output"
        return
    try:
        info = json.loads(detail.stdout)
    except json.JSONDecodeError as exc:
        pr.enrich_error = f"invalid JSON from gh pr view: {exc}"
        return
    pr.head_ref = info.get("headRefName", "")
    pr.review_decision = info.get("reviewDecision", "") or ""
    pr.mergeable = info.get("mergeable", "") or ""
    pr.checks_state = _rollup_state(info.get("statusCheckRollup", []) or [])
    pr.attention_reasons = _attention_reasons(pr, info.get("reviews", []), current_user)


def _attention_reasons(
    pr: PullRequest, reviews: list[dict], current_user: str
) -> set[str]:
    """Compute why an enriched PR needs the current user's attention.

    Pure function of the enriched ``pr`` fields, its review history, and the
    current user's login. Drafts never need attention.
    """
    if pr.is_draft:
        return set()

    reasons: set[str] = set()

    # --- PRs where I am asked to review ---
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

    return reasons
