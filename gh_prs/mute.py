"""Mute rules: review requests you never intend to answer, declared once.

A snooze silences one PR for a while; a mute rule silences a *kind* of PR for
good — typically a bot's dependency bumps for a stack you don't own, which
branch protection keeps routing to your team anyway. Rules are authored by
hand in ``config.json`` (parsed by ``config.py``); this module only decides
whether a fetched PR matches one. No ``gh`` calls, like ``snooze.py``.

A rule names an author and, optionally, labels that exempt a PR from it::

    {"author": "centreon-renovate", "unless_labels": ["S-Python"]}

reads "hide centreon-renovate's PRs, except the ones labelled S-Python".
Logins and label names compare case-insensitively, as GitHub treats them.

Muting *hides*, so it follows the store's fail-safe direction: a PR is muted
only on positive evidence. The author must match exactly; an exemption label
can only be ruled out when the PR's label list is known to be complete
(``labels_complete``) — a truncated or missing list could be hiding the very
label that exempts it, so the PR shows. A PR the viewer authored is never
muted: authored reasons (ready, ci-failed, conflict, …) are the actionable
ones, and mute rules are about other people's review requests.
"""

from dataclasses import dataclass

from gh_prs.gh import PullRequest


@dataclass(frozen=True, slots=True)
class MuteRule:
    # Login of the PR author the rule applies to, as GitHub's GraphQL reports
    # it — a GitHub App shows as its slug without the "[bot]" suffix the REST
    # API adds (``gh prs -r --json`` prints the login the tool sees).
    author: str
    # Labels any one of which exempts a PR from the rule; empty means the
    # rule has no exemption.
    unless_labels: frozenset[str] = frozenset()

    def __post_init__(self) -> None:
        # Normalize once so matching is a plain set lookup. GitHub logins and
        # label names are case-insensitive.
        object.__setattr__(self, "author", self.author.casefold())
        object.__setattr__(
            self, "unless_labels", frozenset(x.casefold() for x in self.unless_labels)
        )


def is_muted(pr: PullRequest, rules: tuple[MuteRule, ...]) -> bool:
    """Whether some rule silences this PR (positive evidence only).

    Never true for a PR the viewer authored, for an author the rules don't
    name, or when an exemption can't be ruled out because the label list is
    incomplete.
    """
    if "author" in pr.roles or not pr.author:
        return False
    author = pr.author.casefold()
    for rule in rules:
        if rule.author != author:
            continue
        if not rule.unless_labels:
            return True
        if not pr.labels_complete:
            # A hidden label might be the exempting one: can't prove it isn't.
            continue
        if rule.unless_labels.isdisjoint(label.casefold() for label in pr.labels):
            return True
    return False


def split_muted(
    prs: list[PullRequest], rules: tuple[MuteRule, ...]
) -> tuple[list[PullRequest], list[PullRequest]]:
    """Partition PRs into (visible, muted), preserving order."""
    visible: list[PullRequest] = []
    muted: list[PullRequest] = []
    for pr in prs:
        (muted if is_muted(pr, rules) else visible).append(pr)
    return visible, muted
