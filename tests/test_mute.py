"""Mute rules: matching is positive-evidence only, and never touches your own PRs."""

from gh_prs.gh import PullRequest
from gh_prs.mute import MuteRule, is_muted, split_muted


def _pr(**overrides) -> PullRequest:
    defaults = dict(
        number=1,
        repo="acme/widgets",
        title="chore(deps): bump nats",
        author="renovate",
        url="https://github.com/acme/widgets/pull/1",
        updated_at="2026-07-15T12:00:00Z",
        created_at="2026-07-01T12:00:00Z",
        is_draft=False,
        labels=("C-Deps", "S-Helm"),
        labels_complete=True,
        roles={"reviewer"},
    )
    return PullRequest(**(defaults | overrides))


RENOVATE = MuteRule(author="renovate")
RENOVATE_UNLESS_PYTHON = MuteRule(
    author="renovate", unless_labels=frozenset({"S-Python"})
)


class TestMuteRule:
    def test_normalizes_case_once(self):
        rule = MuteRule(
            author="Centreon-Renovate", unless_labels=frozenset({"S-Python"})
        )
        assert rule.author == "centreon-renovate"
        assert rule.unless_labels == frozenset({"s-python"})


class TestIsMuted:
    def test_no_rules_mutes_nothing(self):
        assert not is_muted(_pr(), ())

    def test_author_rule_without_exemption_mutes(self):
        assert is_muted(_pr(), (RENOVATE,))

    def test_author_match_is_case_insensitive(self):
        assert is_muted(_pr(author="Renovate"), (RENOVATE,))

    def test_other_author_is_not_muted(self):
        assert not is_muted(_pr(author="octocat"), (RENOVATE,))

    def test_empty_author_never_matches(self):
        # A deleted account comes back as a null author; an empty rule login
        # is rejected by the config, so this can only be a non-match.
        assert not is_muted(_pr(author=""), (RENOVATE,))

    def test_exempting_label_keeps_the_pr_visible(self):
        assert not is_muted(
            _pr(labels=("C-Deps", "S-Python")), (RENOVATE_UNLESS_PYTHON,)
        )

    def test_exempting_label_is_case_insensitive(self):
        assert not is_muted(_pr(labels=("s-python",)), (RENOVATE_UNLESS_PYTHON,))

    def test_without_exempting_label_the_pr_is_muted(self):
        assert is_muted(_pr(labels=("C-Deps", "S-Helm")), (RENOVATE_UNLESS_PYTHON,))

    def test_incomplete_label_list_cannot_prove_absence(self):
        # Fail-safe: a label beyond the cap (or a missing block) could be the
        # exempting one, so the PR shows.
        pr = _pr(labels=("C-Deps",), labels_complete=False)
        assert not is_muted(pr, (RENOVATE_UNLESS_PYTHON,))

    def test_incomplete_label_list_still_mutes_unconditional_rule(self):
        # No exemption to disprove: the author alone is positive evidence.
        assert is_muted(_pr(labels_complete=False), (RENOVATE,))

    def test_no_labels_at_all_is_muted_when_the_list_is_known_empty(self):
        assert is_muted(_pr(labels=(), labels_complete=True), (RENOVATE_UNLESS_PYTHON,))

    def test_authored_pr_is_never_muted(self):
        # Even a rule naming the viewer: authored reasons are the actionable
        # ones, and mute rules are about other people's review requests.
        pr = _pr(author="me", roles={"author"})
        assert not is_muted(pr, (MuteRule(author="me"),))

    def test_any_matching_rule_suffices(self):
        rules = (MuteRule(author="dependabot"), RENOVATE_UNLESS_PYTHON)
        assert is_muted(_pr(), rules)
        assert is_muted(_pr(author="dependabot", labels=("S-Python",)), rules)

    def test_a_later_unconditional_rule_mutes_what_an_earlier_one_exempted(self):
        # Rules are independent: matching any one mutes. Two rules for one
        # author is a config smell, not something to reconcile here.
        pr = _pr(labels=("S-Python",))
        assert is_muted(pr, (RENOVATE_UNLESS_PYTHON, RENOVATE))


class TestSplitMuted:
    def test_partitions_preserving_order(self):
        a = _pr(number=1, author="octocat")
        b = _pr(number=2)
        c = _pr(number=3, author="octocat")
        d = _pr(number=4, labels=("S-Python",))
        visible, muted = split_muted([a, b, c, d], (RENOVATE_UNLESS_PYTHON,))
        assert visible == [a, c, d]
        assert muted == [b]

    def test_no_rules_returns_everything_visible(self):
        prs = [_pr(number=1), _pr(number=2)]
        assert split_muted(prs, ()) == (prs, [])
