//! Merge policy for `gh prs merge`: what may be merged, and whether to approve
//! first.
//!
//! Pure functions over an enriched [`PullRequest`]; no `gh` calls. This is
//! the one place the tool decides to *write* to GitHub, and a merge is
//! irreversible, so the house fail-safe direction applies at its strictest:
//! every unknown is a blocker, and only positive evidence lets a PR through.

use crate::pr::{Checks, Mergeable, PrState, PullRequest, Reported, ReviewDecision, ReviewState};

/// How the merge is asked for. The CLI makes `--admin` and `--auto`
/// mutually exclusive; this type makes the combination unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMode {
    /// Merge now; checks and the review decision must allow it.
    Now,
    /// GitHub's administrator bypass: checks and the review decision are
    /// not preflighted.
    Admin,
    /// Auto-merge: GitHub waits for checks and pending reviews, but a
    /// standing changes-requested review still blocks (a human objected).
    Auto,
}

/// Whether to submit an approving review before merging.
///
/// Skipped when the viewer authored the PR (GitHub rejects self-approval,
/// so it would only fail the batch) or already has a standing approval. A
/// dismissed or superseded review is approved afresh.
#[must_use]
pub fn should_approve(pr: &PullRequest) -> bool {
    let approved = pr
        .my_review
        .as_ref()
        .is_some_and(|review| review.state == Reported::Known(ReviewState::Approved));
    !pr.is_authored() && !approved
}

/// Why `pr` must not be merged right now; empty when it may be.
///
/// Always blocking, whatever the mode: not known to be open, a draft,
/// conflicts or unknown mergeability, no known head commit, or stacked on
/// another open PR (GitHub reports it mergeable, but merging would fold it
/// into the parent instead of shipping it).
#[must_use]
pub fn merge_blockers(pr: &PullRequest, mode: MergeMode) -> Vec<String> {
    let mut blockers: Vec<String> = Vec::new();
    match &pr.state {
        Reported::Known(PrState::Open) => {}
        Reported::Known(PrState::Closed) => blockers.push("is closed".to_owned()),
        Reported::Known(PrState::Merged) => blockers.push("is merged".to_owned()),
        Reported::Unrecognised(state) => blockers.push(format!("is {}", state.to_lowercase())),
        Reported::Missing => blockers.push("has an unknown state".to_owned()),
    }
    if pr.is_draft {
        blockers.push("is a draft".to_owned());
    }
    match pr.mergeable {
        Reported::Known(Mergeable::Mergeable) => {}
        Reported::Known(Mergeable::Conflicting) => blockers.push("has merge conflicts".to_owned()),
        Reported::Known(Mergeable::Unknown) | Reported::Unrecognised(_) | Reported::Missing => {
            blockers.push(
                "has unknown mergeability (GitHub is still computing it; retry shortly)".to_owned(),
            );
        }
    }
    if pr.head.is_none() {
        blockers.push("has no known head commit".to_owned());
    }
    if pr.stacked {
        blockers.push("is stacked on another open PR (merge the parent first)".to_owned());
    }

    if mode == MergeMode::Admin {
        return blockers;
    }
    if mode == MergeMode::Now {
        match pr.checks {
            Checks::Success | Checks::NoneConfigured => {}
            Checks::Failure => blockers.push("has failing checks".to_owned()),
            Checks::Pending => {
                blockers.push("has checks still running (--auto merges once they pass)".to_owned());
            }
        }
    }
    match &pr.review_decision {
        // Missing: no review policy applies to the PR.
        Reported::Known(ReviewDecision::Approved) | Reported::Missing => {}
        Reported::Known(ReviewDecision::ChangesRequested) => {
            blockers.push("has changes requested".to_owned());
        }
        // The viewer's own approval, about to be submitted, may satisfy it.
        Reported::Known(ReviewDecision::ReviewRequired) => {
            if mode == MergeMode::Now && !should_approve(pr) {
                blockers.push("is still awaiting review".to_owned());
            }
        }
        // Deliberately unlike the Python version, which let it through.
        Reported::Unrecognised(decision) => {
            blockers.push(format!(
                "has a review decision this version doesn't know ({decision})"
            ));
        }
    }
    blockers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::Role;
    use crate::pr::testing::{my_review, oid, pr, roles};

    /// A PR that passes preflight outright unless a test says otherwise.
    fn clean() -> PullRequest {
        PullRequest {
            state: Reported::Known(PrState::Open),
            review_decision: Reported::Known(ReviewDecision::Approved),
            mergeable: Reported::Known(Mergeable::Mergeable),
            checks: Checks::Success,
            head: oid("cafe"),
            ..pr()
        }
    }

    fn decision(decision: ReviewDecision) -> PullRequest {
        PullRequest {
            review_decision: Reported::Known(decision),
            ..clean()
        }
    }

    fn authored(pr: PullRequest) -> PullRequest {
        PullRequest {
            roles: roles(&[Role::Author]),
            ..pr
        }
    }

    const MODES: [MergeMode; 3] = [MergeMode::Now, MergeMode::Admin, MergeMode::Auto];

    #[test]
    fn approves_someone_elses_unreviewed_pr() {
        assert!(should_approve(&clean()));
    }

    #[test]
    fn own_pr_is_never_approved() {
        assert!(!should_approve(&authored(clean())));
    }

    #[test]
    fn standing_approval_is_not_repeated() {
        let pr = PullRequest {
            my_review: my_review(ReviewState::Approved, ""),
            ..clean()
        };
        assert!(!should_approve(&pr));
    }

    #[test]
    fn non_approving_review_is_superseded() {
        for state in [
            ReviewState::Dismissed,
            ReviewState::Commented,
            ReviewState::ChangesRequested,
        ] {
            let pr = PullRequest {
                my_review: my_review(state, ""),
                ..clean()
            };
            assert!(should_approve(&pr), "{state:?}");
        }
    }

    #[test]
    fn clean_pr_has_no_blockers() {
        assert!(merge_blockers(&clean(), MergeMode::Now).is_empty());
    }

    #[test]
    fn no_checks_configured_is_fine() {
        let pr = PullRequest {
            checks: Checks::NoneConfigured,
            ..clean()
        };
        assert!(merge_blockers(&pr, MergeMode::Now).is_empty());
    }

    #[test]
    fn no_review_requirement_is_fine() {
        let pr = PullRequest {
            review_decision: Reported::Missing,
            ..clean()
        };
        assert!(merge_blockers(&pr, MergeMode::Now).is_empty());
    }

    #[test]
    fn always_blocking() {
        let cases: Vec<(PullRequest, &str)> = vec![
            (
                PullRequest {
                    state: Reported::Known(PrState::Merged),
                    ..clean()
                },
                "is merged",
            ),
            (
                PullRequest {
                    state: Reported::Known(PrState::Closed),
                    ..clean()
                },
                "is closed",
            ),
            (
                PullRequest {
                    state: Reported::Missing,
                    ..clean()
                },
                "unknown state",
            ),
            (
                PullRequest {
                    state: Reported::Unrecognised("LOCKED".to_owned()),
                    ..clean()
                },
                "is locked",
            ),
            (
                PullRequest {
                    is_draft: true,
                    ..clean()
                },
                "is a draft",
            ),
            (
                PullRequest {
                    mergeable: Reported::Known(Mergeable::Conflicting),
                    ..clean()
                },
                "merge conflicts",
            ),
            (
                PullRequest {
                    mergeable: Reported::Known(Mergeable::Unknown),
                    ..clean()
                },
                "unknown mergeability",
            ),
            (
                PullRequest {
                    mergeable: Reported::Missing,
                    ..clean()
                },
                "unknown mergeability",
            ),
            (
                PullRequest {
                    head: None,
                    ..clean()
                },
                "no known head commit",
            ),
            (
                PullRequest {
                    stacked: true,
                    ..clean()
                },
                "stacked on another open PR",
            ),
        ];
        for mode in MODES {
            for (pr, phrase) in &cases {
                let blockers = merge_blockers(pr, mode);
                assert!(
                    blockers.iter().any(|b| b.contains(phrase)),
                    "{mode:?} {phrase}: {blockers:?}"
                );
            }
        }
    }

    #[test]
    fn blockers_accumulate() {
        let pr = PullRequest {
            is_draft: true,
            mergeable: Reported::Known(Mergeable::Conflicting),
            ..clean()
        };
        assert_eq!(merge_blockers(&pr, MergeMode::Now).len(), 2);
    }

    #[test]
    fn failing_checks_block() {
        let pr = PullRequest {
            checks: Checks::Failure,
            ..clean()
        };
        assert_eq!(merge_blockers(&pr, MergeMode::Now), ["has failing checks"]);
    }

    #[test]
    fn pending_checks_block_and_point_at_auto() {
        let pr = PullRequest {
            checks: Checks::Pending,
            ..clean()
        };
        let blockers = merge_blockers(&pr, MergeMode::Now);
        assert_eq!(blockers.len(), 1);
        assert!(blockers[0].contains("still running") && blockers[0].contains("--auto"));
    }

    #[test]
    fn auto_and_admin_leave_checks_to_github() {
        for checks in [Checks::Failure, Checks::Pending] {
            let pr = PullRequest { checks, ..clean() };
            assert!(
                merge_blockers(&pr, MergeMode::Auto).is_empty(),
                "{checks:?}"
            );
            assert!(
                merge_blockers(&pr, MergeMode::Admin).is_empty(),
                "{checks:?}"
            );
        }
    }

    #[test]
    fn changes_requested_blocks_even_with_auto() {
        // A human objected; auto-merge must not be armed over their head.
        let pr = decision(ReviewDecision::ChangesRequested);
        assert_eq!(
            merge_blockers(&pr, MergeMode::Now),
            ["has changes requested"]
        );
        assert_eq!(
            merge_blockers(&pr, MergeMode::Auto),
            ["has changes requested"]
        );
    }

    #[test]
    fn admin_bypasses_changes_requested() {
        assert!(
            merge_blockers(
                &decision(ReviewDecision::ChangesRequested),
                MergeMode::Admin
            )
            .is_empty()
        );
    }

    #[test]
    fn review_required_passes_when_the_viewer_will_approve() {
        assert!(
            merge_blockers(&decision(ReviewDecision::ReviewRequired), MergeMode::Now).is_empty()
        );
    }

    #[test]
    fn review_required_blocks_own_pr() {
        let pr = authored(decision(ReviewDecision::ReviewRequired));
        assert_eq!(
            merge_blockers(&pr, MergeMode::Now),
            ["is still awaiting review"]
        );
    }

    #[test]
    fn review_required_blocks_when_already_approved_by_viewer() {
        // The viewer's approval is already counted and it wasn't enough.
        let pr = PullRequest {
            my_review: my_review(ReviewState::Approved, ""),
            ..decision(ReviewDecision::ReviewRequired)
        };
        assert_eq!(
            merge_blockers(&pr, MergeMode::Now),
            ["is still awaiting review"]
        );
    }

    #[test]
    fn auto_and_admin_wait_for_or_bypass_review() {
        let pr = authored(decision(ReviewDecision::ReviewRequired));
        assert!(merge_blockers(&pr, MergeMode::Auto).is_empty());
        assert!(merge_blockers(&pr, MergeMode::Admin).is_empty());
    }

    #[test]
    fn unrecognised_review_decision_blocks_unless_admin() {
        // Never merge on uncertain data; the Python version let it through.
        let pr = PullRequest {
            review_decision: Reported::Unrecognised("QUEUED".to_owned()),
            ..clean()
        };
        assert_eq!(
            merge_blockers(&pr, MergeMode::Now),
            ["has a review decision this version doesn't know (QUEUED)"]
        );
        assert_eq!(merge_blockers(&pr, MergeMode::Auto).len(), 1);
        assert!(merge_blockers(&pr, MergeMode::Admin).is_empty());
    }
}
