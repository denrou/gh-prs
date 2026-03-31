"""Thin wrapper around the gh CLI for GitHub pull requests."""

from __future__ import annotations

import json
import subprocess
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass


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
    def updated_date(self) -> str:
        return self.updated_at.split("T")[0]

    @property
    def id(self) -> str:
        return f"{self.repo}#{self.number}"


def _run_gh(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["gh", *args],
        capture_output=True,
        text=True,
        check=check,
    )


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
    return [PullRequest.from_json(item) for item in json.loads(raw)]


def fetch_prs() -> list[PullRequest]:
    """Fetch open PRs assigned to or requesting review from the current user."""
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [
            pool.submit(_search_prs, q) for q in ("review-requested", "assignee")
        ]
        seen: dict[str, PullRequest] = {}
        for future in futures:
            for pr in future.result():
                seen[pr.id] = pr

    return sorted(seen.values(), key=lambda p: p.updated_at, reverse=True)


def enrich_pr(pr: PullRequest) -> None:
    """Fetch branch and review details for a single PR (mutates in place)."""
    detail = _run_gh(
        "pr",
        "view",
        str(pr.number),
        "--repo",
        pr.repo,
        "--json",
        "headRefName,reviewDecision",
        check=False,
    )
    if detail.returncode == 0 and detail.stdout.strip():
        info = json.loads(detail.stdout)
        pr.head_ref = info.get("headRefName", "")
        pr.review_decision = info.get("reviewDecision", "") or ""


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


def open_in_browser(pr: PullRequest) -> None:
    if pr.url:
        subprocess.run(["open", pr.url], check=False)
