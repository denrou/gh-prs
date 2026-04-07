"""Thin wrapper around the gh CLI for GitHub pull requests."""

from __future__ import annotations

import json
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field


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
    roles: set[str] = field(default_factory=set)
    # Computed once during enrichment; None means not yet enriched.
    _attention: bool | None = field(default=None, repr=False)

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
        return bool(self._attention)


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


def fetch_prs() -> list[PullRequest]:
    """Fetch open PRs where the current user is author, reviewer, assignee, or participant."""
    qualifiers = ["author", "review-requested", "assignee", "involves"]
    seen: dict[str, PullRequest] = {}
    with ThreadPoolExecutor(max_workers=4) as pool:
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


def enrich_pr(pr: PullRequest, current_user: str = "") -> None:
    """Fetch branch, review decision, and reviews for a single PR (mutates in place)."""
    try:
        detail = _run_gh(
            "pr",
            "view",
            str(pr.number),
            "--repo",
            pr.repo,
            "--json",
            "headRefName,reviewDecision,reviews",
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

    # Compute and cache attention flag — avoids re-iterating reviews on every render.
    if not pr.is_draft:
        reviews = info.get("reviews", [])
        my_reviews = [
            r for r in reviews if (r.get("author") or {}).get("login") == current_user
        ]
        # Only the most recent review matters: a later APPROVED supersedes an earlier
        # DISMISSED, so checking any() over all reviews would give false positives.
        latest_my_review = (
            max(my_reviews, key=lambda r: r.get("submittedAt", ""))
            if my_reviews
            else None
        )
        latest_state = latest_my_review.get("state") if latest_my_review else None
        user_has_active_review = latest_state in ("APPROVED", "CHANGES_REQUESTED")
        dismissed = latest_state == "DISMISSED"
        review_req = (
            "review-requested" in pr.roles
            and pr.review_decision in ("REVIEW_REQUIRED", "")
            and not user_has_active_review
        )
        ready_to_merge = "author" in pr.roles and pr.review_decision == "APPROVED"
        pr._attention = bool(review_req or dismissed or ready_to_merge)
    else:
        pr._attention = False


def approve_pr(repo: str, number: int) -> None:
    result = _run_gh(
        "pr",
        "review",
        str(number),
        "--repo",
        repo,
        "--approve",
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"Failed to approve {repo}#{number}: {result.stderr.strip()}"
        )


def merge_pr(repo: str, number: int) -> None:
    result = _run_gh(
        "pr",
        "merge",
        str(number),
        "--repo",
        repo,
        "--squash",
        "--delete-branch",
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to merge {repo}#{number}: {result.stderr.strip()}")


def fetch_pr_body(pr: PullRequest) -> str:
    """Fetch the PR description body."""
    result = _run_gh(
        "pr", "view", str(pr.number), "--repo", pr.repo, "--json", "body", check=False
    )
    if result.returncode != 0 or not result.stdout.strip():
        return ""
    try:
        return json.loads(result.stdout).get("body", "") or ""
    except json.JSONDecodeError:
        return ""


def fetch_pr_diff(pr: PullRequest) -> str:
    """Fetch the unified diff for a PR."""
    result = _run_gh("pr", "diff", str(pr.number), "--repo", pr.repo, check=False)
    if result.returncode != 0:
        return ""
    return result.stdout


def parse_diff(diff_text: str) -> list[tuple[str, str]]:
    """Split a unified diff into (filename, chunk) pairs."""
    files: list[tuple[str, str]] = []
    current_file: str | None = None
    current_lines: list[str] = []
    for line in diff_text.splitlines():
        if line.startswith("diff --git "):
            if current_file is not None:
                files.append((current_file, "\n".join(current_lines)))
            # "diff --git a/foo/bar.py b/foo/bar.py" → "foo/bar.py"
            current_file = line.split(" ")[-1][2:]
            current_lines = [line]
        elif current_file is not None:
            current_lines.append(line)
    if current_file is not None and current_lines:
        files.append((current_file, "\n".join(current_lines)))
    return files


def open_in_browser(pr: PullRequest) -> None:
    if pr.url:
        subprocess.run(["open", pr.url], check=False)
