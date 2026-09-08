"""Merge policy for ``gh prs merge``: what may be merged, and whether to approve first.

Pure functions over an enriched ``PullRequest``; no ``gh`` calls. This is the
one place the tool decides to *write* to GitHub, and a merge is irreversible,
so the house fail-safe direction applies at its strictest: every unknown is a
blocker, and only positive evidence lets a PR through.
"""

from gh_prs.gh import PullRequest


def should_approve(pr: PullRequest) -> bool:
    """Whether to submit an approving review before merging.

    Approving is skipped when the viewer authored the PR (GitHub rejects
    self-approval, so attempting it would only fail the batch) or already has
    a standing approval. A dismissed or superseded review is approved afresh.
    """
    return "author" not in pr.roles and pr.my_review_state != "APPROVED"


def merge_blockers(
    pr: PullRequest, *, admin: bool = False, auto: bool = False
) -> list[str]:
    """Why ``pr`` must not be merged right now; empty when it may be.

    Always blocking, whatever the flags: the PR is not (known to be) open, is
    a draft, has conflicts or unknown mergeability, has no known head commit,
    or is stacked on another open PR — GitHub reports a stacked PR mergeable,
    but merging it would fold it into the parent PR instead of shipping it.

    ``admin`` mirrors GitHub's administrator override, which bypasses branch
    protection: failing or pending checks and the review decision stop
    blocking. ``auto`` hands checks and pending reviews to GitHub's
    auto-merge, which waits for them; a standing changes-requested review
    still blocks (a human objected — dismiss it or use ``admin``). Without
    either flag, checks must be green (or absent) and the review decision
    must not stand in the way: approved, not required, or required on a PR
    the viewer is about to approve (``should_approve``).
    """
    blockers: list[str] = []
    if pr.state != "OPEN":
        blockers.append(
            f"is {pr.state.lower()}" if pr.state else "has an unknown state"
        )
    if pr.is_draft:
        blockers.append("is a draft")
    if pr.mergeable == "CONFLICTING":
        blockers.append("has merge conflicts")
    elif pr.mergeable != "MERGEABLE":
        blockers.append(
            "has unknown mergeability (GitHub is still computing it; retry shortly)"
        )
    if not pr.head_ref_oid:
        blockers.append("has no known head commit")
    if pr.stacked:
        blockers.append("is stacked on another open PR (merge the parent first)")

    if not admin:
        if not auto:
            if pr.checks_state == "FAILURE":
                blockers.append("has failing checks")
            elif pr.checks_state == "PENDING":
                blockers.append(
                    "has checks still running (--auto merges once they pass)"
                )
        if pr.review_decision == "CHANGES_REQUESTED":
            blockers.append("has changes requested")
        elif (
            pr.review_decision == "REVIEW_REQUIRED"
            and not auto
            and not should_approve(pr)
        ):
            blockers.append("is still awaiting review")
    return blockers
