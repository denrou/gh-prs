//! Why an enriched PR needs the viewer's attention.
//!
//! Pure policy over a [`PullRequest`] plus the clock; the rules and their
//! fail-safe directions are described in `CLAUDE.md` (Attention logic). The
//! stale family is the one place that inverts the house direction: a nudge is
//! additive and non-actionable, so an unknown age or review history keeps it
//! quiet instead of fabricating it.

use std::collections::BTreeSet;

use jiff::civil::{Date, DateTime, date};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use crate::duration::{Duration, Unit};
use crate::pr::{
    Checks, Mergeable, PullRequest, Reason, Reported, ReviewDecision, ReviewState, Role,
};

/// A Monday to anchor the week for the skip-weekends clock, and the offset
/// Saturday falls on from a Monday.
const WEEK_EPOCH: Date = date(1970, 1, 5);
const SATURDAY: i64 = 5;

/// The staleness clock. Without one, neither `stale` nor `stale-draft` can
/// fire: that is how a disabled threshold (`stale_after: null`) reads.
#[derive(Clone, Debug)]
pub struct Staleness {
    pub now: Timestamp,
    pub after: Duration,
    pub skip_weekends: bool,
    /// The local calendar days are counted on.
    pub tz: TimeZone,
}

impl Staleness {
    /// True when `updated_at` is older than the threshold.
    ///
    /// A missing, unparseable or offset-less timestamp is not stale: the
    /// quiet direction, since an unknown age must not cry wolf on a
    /// possibly fresh PR. A threshold in days counts on the local calendar
    /// (see [`Duration::until`]), so the nudge is there on the first look of
    /// the morning, up to a day earlier than a 24-hour span would be. Under
    /// `skip_weekends`, days are working days and hours run on a clock
    /// stopped over the weekend.
    #[must_use]
    pub fn is_stale(&self, updated_at: &str) -> bool {
        let Ok(updated) = updated_at.parse::<Timestamp>() else {
            return false;
        };
        match self.after.unit {
            Unit::Days => self
                .after
                .until(updated, &self.tz, self.skip_weekends)
                .is_ok_and(|deadline| self.now >= deadline),
            Unit::Hours => {
                let elapsed = if self.skip_weekends {
                    working_elapsed(updated, self.now, &self.tz)
                } else {
                    self.now.duration_since(updated)
                };
                elapsed >= SignedDuration::from_hours(self.after.amount.into())
            }
        }
    }
}

/// The reasons `pr` needs the viewer's attention.
///
/// Drafts are deliberately parked WIP: only `conflict` and the `stale-draft`
/// nudge can fire on them, both for the author.
#[must_use]
pub fn attention_reasons(pr: &PullRequest, staleness: Option<&Staleness>) -> BTreeSet<Reason> {
    let conflicting = matches!(pr.mergeable, Reported::Known(Mergeable::Conflicting));
    let stale = || staleness.is_some_and(|clock| clock.is_stale(&pr.updated_at));
    let mut reasons = BTreeSet::new();

    if pr.is_draft {
        // Conflict takes precedence over the nudge, mirroring 'stale'.
        if pr.is_authored() {
            if conflicting {
                reasons.insert(Reason::Conflict);
            } else if stale() {
                reasons.insert(Reason::StaleDraft);
            }
        }
        return reasons;
    }

    // --- PRs where I am asked to review ---
    let my_state = pr.my_review.as_ref().map(|review| &review.state);
    let (has_active_review, dismissed) = match my_state {
        Some(Reported::Known(ReviewState::Approved | ReviewState::ChangesRequested)) => {
            (true, false)
        }
        Some(Reported::Known(ReviewState::Dismissed)) => (false, true),
        Some(
            Reported::Known(ReviewState::Commented | ReviewState::Pending)
            | Reported::Unrecognised(_)
            | Reported::Missing,
        )
        | None => (false, false),
    };
    let wants_my_review =
        (pr.roles.contains(&Role::ReviewRequested) || dismissed) && !has_active_review;
    let decision_wants_review = match pr.review_decision {
        // The author is already reworking the PR.
        Reported::Known(ReviewDecision::ChangesRequested) => false,
        // Mergeable without me, unless I'm personally on the requested
        // reviewers list (not just through a team).
        Reported::Known(ReviewDecision::Approved) => pr.review_requested_explicitly,
        Reported::Known(ReviewDecision::ReviewRequired)
        | Reported::Unrecognised(_)
        | Reported::Missing => true,
    };
    // A review would be staled once the author rebases. GitHub's UNKNOWN
    // mergeability deliberately keeps the request visible while it churns.
    if wants_my_review && !conflicting && decision_wants_review {
        reasons.insert(Reason::Review);
    }

    // --- PRs I already reviewed that moved on without a re-request ---
    // Commit identity, not dates: committer timestamps are mutable. A missing
    // oid on one side still counts as moved; with none at all there is
    // nothing to compare.
    let submitted = match my_state {
        // A state GitHub added since counts as submitted: unknown must never
        // read as "nothing to do".
        Some(
            Reported::Known(
                ReviewState::Approved
                | ReviewState::ChangesRequested
                | ReviewState::Commented
                | ReviewState::Dismissed,
            )
            | Reported::Unrecognised(_),
        ) => true,
        Some(Reported::Known(ReviewState::Pending) | Reported::Missing) | None => false,
    };
    let reviewed_commit = pr
        .my_review
        .as_ref()
        .and_then(|review| review.commit.as_ref());
    let moved = match (reviewed_commit, pr.head.as_ref()) {
        (None, None) => false,
        (reviewed, head) => reviewed != head,
    };
    if !reasons.contains(&Reason::Review)
        && submitted
        // A comment-review on my own PR must not self-flag.
        && !pr.is_authored()
        && moved
        // Once conflicting, more commits are coming.
        && !conflicting
    {
        reasons.insert(Reason::NewCommits);
    }

    // --- PRs I authored that need my action ---
    if pr.is_authored() {
        // Conflicts and failing CI are independent; a PR can need both.
        if conflicting {
            reasons.insert(Reason::Conflict);
        }
        match pr.checks {
            Checks::Failure => {
                reasons.insert(Reason::CiFailed);
            }
            Checks::Success | Checks::NoneConfigured => {
                // Positive evidence only: GitHub's UNKNOWN mergeability must
                // not read as "no conflict", and a stacked PR would merge
                // into its parent instead of shipping.
                if matches!(
                    pr.review_decision,
                    Reported::Known(ReviewDecision::Approved)
                ) && matches!(pr.mergeable, Reported::Known(Mergeable::Mergeable))
                    && !pr.stacked
                {
                    reasons.insert(Reason::Ready);
                }
            }
            Checks::Pending => {}
        }
        // Replying or resolving is mine regardless of the PR's other
        // troubles, and merging over an open question is my call.
        if pr.unresolved_feedback {
            reasons.insert(Reason::Unresolved);
        }
        // The soft nudge comes last: nothing above is mine to act on, yet
        // the PR has waited on reviewers past the threshold.
        if reasons.is_empty() && awaiting_review(pr) && stale() {
            reasons.insert(Reason::Stale);
        }
    }
    reasons
}

/// True when an authored PR is still waiting on its reviewers.
///
/// Not approved (that's waiting to merge), and not a changes-requested the
/// author is still reworking, with one carve-out: GitHub keeps reporting
/// CHANGES_REQUESTED after the author has answered, so the ball counts as
/// back in the reviewers' court once every standing review is against a
/// superseded commit and a review request is pending. That second half is
/// deliberately weak: `reviewRequests` has no timestamp, so it means somebody
/// is on the hook, not that the author handed the PR back.
fn awaiting_review(pr: &PullRequest) -> bool {
    match pr.review_decision {
        Reported::Known(ReviewDecision::Approved) => false,
        Reported::Known(ReviewDecision::ChangesRequested) => {
            pr.has_pending_review_request && changes_requested_addressed(pr)
        }
        Reported::Known(ReviewDecision::ReviewRequired)
        | Reported::Unrecognised(_)
        | Reported::Missing => true,
    }
}

/// True when changes-requested reviews stand and none is against the head:
/// the author has pushed since the reviewers said no.
///
/// Inherits the quiet direction of the stale nudge, its only caller: no
/// review in hand, no head, a review against an unknown commit, or a
/// truncated connection all read as "not addressed".
fn changes_requested_addressed(pr: &PullRequest) -> bool {
    let standing = &pr.changes_requested;
    let Some(head) = pr.head.as_ref() else {
        return false;
    };
    !standing.truncated
        && !standing.commits.is_empty()
        && standing
            .commits
            .iter()
            .all(|commit| commit.as_ref().is_some_and(|oid| oid != head))
}

/// Time from `start` to `end` with Saturdays and Sundays not counted.
///
/// Measured on the local calendar in wall-clock terms: three working days
/// after Thursday 10:00 is Tuesday 10:00, even across a DST change where the
/// absolute elapsed time is an hour off what the calendar says.
fn working_elapsed(start: Timestamp, end: Timestamp, tz: &TimeZone) -> SignedDuration {
    let local_start = start.to_zoned(tz.clone()).datetime();
    let local_end = end.to_zoned(tz.clone()).datetime();
    let span = local_end.duration_since(local_start);
    if span <= SignedDuration::ZERO {
        // Clock skew: a future update must not have the weekend subtracted
        // from a negative span and come out looking old.
        return span;
    }
    span - (weekend_before(local_end) - weekend_before(local_start))
}

/// Weekend time from a fixed Monday up to `local`. Monotonic, so the
/// difference of two gives the weekend time between them in constant time.
fn weekend_before(local: DateTime) -> SignedDuration {
    let day = local.date().duration_since(WEEK_EPOCH).as_secs() / 86_400;
    let (weeks, weekday) = (day.div_euclid(7), day.rem_euclid(7));
    let mut elapsed = SignedDuration::from_hours(48 * weeks);
    if weekday >= SATURDAY {
        elapsed += SignedDuration::from_hours(24 * (weekday - SATURDAY));
        elapsed += local.duration_since(local.start_of_day());
    }
    elapsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr::testing::{my_review, oid, pr, reasons, roles};
    use crate::pr::{ChangesRequested, MyReview, Oid};

    use Reason::{CiFailed, Conflict, NewCommits, Ready, Review, Stale, StaleDraft, Unresolved};
    use Role::{Author, ReviewRequested, ReviewedBy};

    const NOW: &str = "2026-07-27T12:00:00Z";
    const STALE_AFTER: Duration = Duration::days(3);

    fn known<T>(value: T) -> Reported<T> {
        Reported::Known(value)
    }

    fn clock(now: Timestamp, after: Duration, skip_weekends: bool, tz: TimeZone) -> Staleness {
        Staleness {
            now,
            after,
            skip_weekends,
            tz,
        }
    }

    /// The default clock: 2026-07-27 12:00 UTC, three calendar days.
    fn utc_clock() -> Staleness {
        clock(NOW.parse().unwrap(), STALE_AFTER, false, TimeZone::UTC)
    }

    fn paris() -> TimeZone {
        TimeZone::get("Europe/Paris").unwrap()
    }

    fn at_paris(y: i16, m: i8, d: i8, h: i8, min: i8) -> Timestamp {
        date(y, m, d)
            .at(h, min, 0, 0)
            .to_zoned(paris())
            .unwrap()
            .timestamp()
    }

    fn bare(pr: &PullRequest) -> BTreeSet<Reason> {
        attention_reasons(pr, None)
    }

    fn timed(pr: &PullRequest) -> BTreeSet<Reason> {
        attention_reasons(pr, Some(&utc_clock()))
    }

    mod review_reason {
        use super::*;

        #[test]
        fn requested_and_pending_needs_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::ReviewRequired),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn no_review_decision_still_needs_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn draft_never_needs_review() {
            let pr = PullRequest {
                is_draft: true,
                roles: roles(&[ReviewRequested]),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn my_active_review_suppresses_review() {
            for state in [ReviewState::Approved, ReviewState::ChangesRequested] {
                let pr = PullRequest {
                    roles: roles(&[ReviewRequested]),
                    review_decision: known(ReviewDecision::ReviewRequired),
                    my_review: my_review(state, ""),
                    ..pr()
                };
                assert_eq!(bare(&pr), reasons(&[]), "{state:?}");
            }
        }

        #[test]
        fn dismissed_review_needs_re_review() {
            let pr = PullRequest {
                my_review: my_review(ReviewState::Dismissed, ""),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn conflicting_pr_excluded_from_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::ReviewRequired),
                mergeable: known(Mergeable::Conflicting),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn changes_requested_by_others_hides_review() {
            // The author is reworking the PR; a review now would be premature.
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::ChangesRequested),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn approved_via_team_request_hides_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::Approved),
                review_requested_explicitly: false,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn approved_with_explicit_request_still_shows_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::Approved),
                review_requested_explicitly: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn stacked_pr_still_needs_review() {
            // Reviews happen per-PR in a stack; only 'ready' defers to the parent.
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::ReviewRequired),
                stacked: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }
    }

    mod new_commits_reason {
        use super::*;

        /// Reviewed by me against aaa111; the head has moved to bbb222.
        fn reviewed() -> PullRequest {
            PullRequest {
                roles: roles(&[ReviewedBy]),
                my_review: my_review(ReviewState::Approved, "aaa111"),
                head: oid("bbb222"),
                ..pr()
            }
        }

        #[test]
        fn head_moved_after_any_submitted_review_flags() {
            for state in [
                ReviewState::Approved,
                ReviewState::ChangesRequested,
                ReviewState::Commented,
            ] {
                let pr = PullRequest {
                    my_review: my_review(state, "aaa111"),
                    ..reviewed()
                };
                assert_eq!(bare(&pr), reasons(&[NewCommits]), "{state:?}");
            }
        }

        #[test]
        fn unchanged_head_does_not_flag() {
            let pr = PullRequest {
                head: oid("aaa111"),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn one_missing_oid_still_flags() {
            // An absent oid counts as "moved": unknown must never read as
            // "nothing to do".
            let no_reviewed_commit = PullRequest {
                my_review: my_review(ReviewState::Approved, ""),
                ..reviewed()
            };
            let no_head = PullRequest {
                head: None,
                ..reviewed()
            };
            assert_eq!(bare(&no_reviewed_commit), reasons(&[NewCommits]));
            assert_eq!(bare(&no_head), reasons(&[NewCommits]));
        }

        #[test]
        fn both_oids_missing_stays_quiet() {
            let pr = PullRequest {
                my_review: my_review(ReviewState::Approved, ""),
                head: None,
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn conflicting_pr_does_not_flag() {
            let pr = PullRequest {
                mergeable: known(Mergeable::Conflicting),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn draft_does_not_flag() {
            let pr = PullRequest {
                is_draft: true,
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn comment_review_on_my_own_pr_does_not_self_flag() {
            let pr = PullRequest {
                roles: roles(&[Author, ReviewedBy]),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn changes_requested_decision_does_not_hide() {
            // Unlike "review": commits landing while the decision is
            // CHANGES_REQUESTED are plausibly the rework to look at.
            let pr = PullRequest {
                review_decision: known(ReviewDecision::ChangesRequested),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[NewCommits]));
        }

        #[test]
        fn re_requested_after_comment_yields_review_not_both() {
            let pr = PullRequest {
                roles: roles(&[ReviewedBy, ReviewRequested]),
                my_review: my_review(ReviewState::Commented, "aaa111"),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn re_requested_after_approval_still_flags_new_commits() {
            let pr = PullRequest {
                roles: roles(&[ReviewedBy, ReviewRequested]),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[NewCommits]));
        }

        #[test]
        fn dismissed_with_visible_review_reason_wins() {
            let pr = PullRequest {
                my_review: my_review(ReviewState::Dismissed, "aaa111"),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn dismissed_hidden_by_approved_decision_flags_new_commits() {
            // Auto-dismiss-on-push repos: my approval was dismissed by the
            // push and a colleague approved, so "review" hides, but the head
            // still moved on me.
            let pr = PullRequest {
                my_review: my_review(ReviewState::Dismissed, "aaa111"),
                review_decision: known(ReviewDecision::Approved),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[NewCommits]));
        }

        #[test]
        fn unrecognised_review_state_flags() {
            // Deliberately unlike the Python version, which stayed quiet.
            let pr = PullRequest {
                my_review: Some(MyReview {
                    state: Reported::Unrecognised("SUPERSEDED".to_owned()),
                    commit: oid("aaa111"),
                }),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[NewCommits]));
        }

        #[test]
        fn pending_review_does_not_flag() {
            // A review I haven't submitted yet is not a review of the old head.
            let pr = PullRequest {
                my_review: my_review(ReviewState::Pending, "aaa111"),
                ..reviewed()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }
    }

    mod author_reasons {
        use super::*;

        fn approved(checks: Checks, mergeable: Mergeable) -> PullRequest {
            PullRequest {
                roles: roles(&[Author]),
                review_decision: known(ReviewDecision::Approved),
                checks,
                mergeable: known(mergeable),
                ..pr()
            }
        }

        #[test]
        fn approved_green_or_checkless_is_ready() {
            for checks in [Checks::Success, Checks::NoneConfigured] {
                assert_eq!(
                    bare(&approved(checks, Mergeable::Mergeable)),
                    reasons(&[Ready])
                );
            }
        }

        #[test]
        fn approved_with_pending_checks_is_not_ready() {
            assert_eq!(
                bare(&approved(Checks::Pending, Mergeable::Mergeable)),
                reasons(&[])
            );
        }

        #[test]
        fn unknown_mergeability_is_not_ready() {
            assert_eq!(
                bare(&approved(Checks::Success, Mergeable::Unknown)),
                reasons(&[])
            );
            let missing = PullRequest {
                mergeable: Reported::Missing,
                ..approved(Checks::Success, Mergeable::Mergeable)
            };
            assert_eq!(bare(&missing), reasons(&[]));
        }

        #[test]
        fn unknown_mergeability_still_shows_review() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::ReviewRequired),
                mergeable: known(Mergeable::Unknown),
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Review]));
        }

        #[test]
        fn failing_ci_flagged() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                checks: Checks::Failure,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[CiFailed]));
        }

        #[test]
        fn conflict_flagged() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                mergeable: known(Mergeable::Conflicting),
                checks: Checks::Success,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Conflict]));
        }

        #[test]
        fn conflict_and_ci_failed_are_independent() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                mergeable: known(Mergeable::Conflicting),
                checks: Checks::Failure,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Conflict, CiFailed]));
        }

        #[test]
        fn conflicting_approved_green_is_not_ready() {
            assert_eq!(
                bare(&approved(Checks::Success, Mergeable::Conflicting)),
                reasons(&[Conflict])
            );
        }

        #[test]
        fn stacked_approved_green_is_not_ready() {
            let pr = PullRequest {
                stacked: true,
                ..approved(Checks::Success, Mergeable::Mergeable)
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn stacked_only_gates_ready() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                mergeable: known(Mergeable::Conflicting),
                checks: Checks::Failure,
                stacked: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Conflict, CiFailed]));
        }
    }

    mod unresolved_reason {
        use super::*;

        #[test]
        fn unresolved_feedback_flagged() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                unresolved_feedback: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Unresolved]));
        }

        #[test]
        fn not_my_pr_never_flags() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                review_decision: known(ReviewDecision::Approved),
                unresolved_feedback: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn draft_never_flags() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                is_draft: true,
                unresolved_feedback: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[]));
        }

        #[test]
        fn fires_alongside_conflict_and_ci_failed() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                unresolved_feedback: true,
                mergeable: known(Mergeable::Conflicting),
                checks: Checks::Failure,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Unresolved, Conflict, CiFailed]));
        }

        #[test]
        fn fires_alongside_ready() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                review_decision: known(ReviewDecision::Approved),
                checks: Checks::Success,
                mergeable: known(Mergeable::Mergeable),
                unresolved_feedback: true,
                ..pr()
            };
            assert_eq!(bare(&pr), reasons(&[Ready, Unresolved]));
        }

        #[test]
        fn suppresses_the_stale_nudge() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                review_decision: known(ReviewDecision::ReviewRequired),
                updated_at: "2026-07-01T12:00:00Z".to_owned(),
                unresolved_feedback: true,
                ..pr()
            };
            assert_eq!(timed(&pr), reasons(&[Unresolved]));
        }
    }

    mod stale_reason {
        use super::*;

        /// Authored, review pending, nothing actionable, last touched well
        /// before the threshold.
        fn stale_pr() -> PullRequest {
            PullRequest {
                roles: roles(&[Author]),
                review_decision: known(ReviewDecision::ReviewRequired),
                updated_at: "2026-07-01T12:00:00Z".to_owned(),
                ..pr()
            }
        }

        /// Changes were requested against an older commit, the author has
        /// since pushed, and a reviewer is on the hook again.
        fn answered_pr() -> PullRequest {
            PullRequest {
                review_decision: known(ReviewDecision::ChangesRequested),
                changes_requested: ChangesRequested {
                    commits: vec![oid("old111")],
                    truncated: false,
                },
                head: oid("new222"),
                has_pending_review_request: true,
                ..stale_pr()
            }
        }

        fn standing(commits: &[&str], truncated: bool) -> ChangesRequested {
            ChangesRequested {
                commits: commits.iter().map(|c| Oid::new(c)).collect(),
                truncated,
            }
        }

        #[test]
        fn old_waiting_pr_is_stale() {
            assert_eq!(timed(&stale_pr()), reasons(&[Stale]));
        }

        #[test]
        fn no_review_decision_still_stale() {
            let pr = PullRequest {
                review_decision: Reported::Missing,
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[Stale]));
        }

        #[test]
        fn recently_active_pr_is_not_stale() {
            let pr = PullRequest {
                updated_at: "2026-07-26T12:00:00Z".to_owned(),
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn exactly_at_threshold_is_stale() {
            let pr = PullRequest {
                updated_at: "2026-07-24T12:00:00Z".to_owned(),
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[Stale]));
        }

        #[test]
        fn approved_pr_is_not_stale() {
            // Approved but not known mergeable: waiting to merge, not on reviewers.
            let pr = PullRequest {
                review_decision: known(ReviewDecision::Approved),
                mergeable: known(Mergeable::Unknown),
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn changes_requested_pr_is_not_stale() {
            let pr = PullRequest {
                review_decision: known(ReviewDecision::ChangesRequested),
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn answered_changes_requested_is_stale_again() {
            assert_eq!(timed(&answered_pr()), reasons(&[Stale]));
        }

        #[test]
        fn answered_but_never_re_requested_is_not_stale() {
            let pr = PullRequest {
                has_pending_review_request: false,
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn re_requested_without_pushing_is_not_stale() {
            let pr = PullRequest {
                changes_requested: standing(&["new222"], false),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn one_unanswered_review_blocks_the_nudge() {
            let pr = PullRequest {
                changes_requested: standing(&["old111", "new222"], false),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn unlinked_review_commit_is_not_stale() {
            let pr = PullRequest {
                changes_requested: standing(&[""], false),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn missing_head_oid_is_not_stale() {
            let pr = PullRequest {
                head: None,
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn hidden_changes_requested_review_is_not_stale() {
            let pr = PullRequest {
                changes_requested: standing(&[], false),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn truncated_reviews_block_the_nudge() {
            // A review hidden by the cap may still be against the head.
            let pr = PullRequest {
                changes_requested: standing(&["old111"], true),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn approved_is_never_stale_even_when_answered() {
            let pr = PullRequest {
                review_decision: known(ReviewDecision::Approved),
                mergeable: known(Mergeable::Unknown),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn answered_changes_requested_still_yields_to_real_actions() {
            let conflicting = PullRequest {
                mergeable: known(Mergeable::Conflicting),
                ..answered_pr()
            };
            let failing = PullRequest {
                checks: Checks::Failure,
                ..answered_pr()
            };
            assert_eq!(timed(&conflicting), reasons(&[Conflict]));
            assert_eq!(timed(&failing), reasons(&[CiFailed]));
        }

        #[test]
        fn answered_changes_requested_respects_the_threshold() {
            let pr = PullRequest {
                updated_at: "2026-07-26T12:00:00Z".to_owned(),
                ..answered_pr()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn real_actions_take_precedence_over_stale() {
            let failing = PullRequest {
                checks: Checks::Failure,
                ..stale_pr()
            };
            let conflicting = PullRequest {
                mergeable: known(Mergeable::Conflicting),
                ..stale_pr()
            };
            let ready = PullRequest {
                review_decision: known(ReviewDecision::Approved),
                checks: Checks::Success,
                mergeable: known(Mergeable::Mergeable),
                ..stale_pr()
            };
            assert_eq!(timed(&failing), reasons(&[CiFailed]));
            assert_eq!(timed(&conflicting), reasons(&[Conflict]));
            assert_eq!(timed(&ready), reasons(&[Ready]));
        }

        #[test]
        fn reviewer_not_author_is_never_stale() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                ..stale_pr()
            };
            assert!(!timed(&pr).contains(&Stale));
        }

        #[test]
        fn draft_is_stale_draft_not_stale() {
            let pr = PullRequest {
                is_draft: true,
                ..stale_pr()
            };
            assert_eq!(timed(&pr), reasons(&[StaleDraft]));
        }

        #[test]
        fn disabled_without_a_clock() {
            assert_eq!(bare(&stale_pr()), reasons(&[]));
        }

        #[test]
        fn unparseable_updated_at_is_not_stale() {
            for bad in ["", "not-a-date"] {
                let pr = PullRequest {
                    updated_at: bad.to_owned(),
                    ..stale_pr()
                };
                assert_eq!(timed(&pr), reasons(&[]), "{bad:?}");
            }
        }
    }

    mod draft_reasons {
        use super::*;

        const FRESH: &str = "2026-07-26T12:00:00Z";

        /// Authored draft, last touched well before the threshold.
        fn draft() -> PullRequest {
            PullRequest {
                roles: roles(&[Author]),
                is_draft: true,
                updated_at: "2026-07-01T12:00:00Z".to_owned(),
                ..pr()
            }
        }

        fn fresh_draft() -> PullRequest {
            PullRequest {
                updated_at: FRESH.to_owned(),
                ..draft()
            }
        }

        #[test]
        fn conflicting_draft_flags_conflict() {
            let pr = PullRequest {
                mergeable: known(Mergeable::Conflicting),
                ..fresh_draft()
            };
            assert_eq!(timed(&pr), reasons(&[Conflict]));
        }

        #[test]
        fn quiet_draft_flags_stale_draft() {
            assert_eq!(timed(&draft()), reasons(&[StaleDraft]));
        }

        #[test]
        fn fresh_draft_stays_quiet() {
            assert_eq!(timed(&fresh_draft()), reasons(&[]));
        }

        #[test]
        fn conflict_takes_precedence_over_stale_draft() {
            let pr = PullRequest {
                mergeable: known(Mergeable::Conflicting),
                ..draft()
            };
            assert_eq!(timed(&pr), reasons(&[Conflict]));
        }

        #[test]
        fn failing_ci_neither_flags_nor_gates_a_draft() {
            let fresh = PullRequest {
                checks: Checks::Failure,
                ..fresh_draft()
            };
            let quiet = PullRequest {
                checks: Checks::Failure,
                ..draft()
            };
            assert_eq!(timed(&fresh), reasons(&[]));
            assert_eq!(timed(&quiet), reasons(&[StaleDraft]));
        }

        #[test]
        fn unknown_mergeability_still_flags_stale_draft() {
            let pr = PullRequest {
                mergeable: known(Mergeable::Unknown),
                ..draft()
            };
            assert_eq!(timed(&pr), reasons(&[StaleDraft]));
        }

        #[test]
        fn approved_green_draft_is_not_ready() {
            let pr = PullRequest {
                review_decision: known(ReviewDecision::Approved),
                checks: Checks::Success,
                mergeable: known(Mergeable::Mergeable),
                ..fresh_draft()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn review_decision_does_not_gate_stale_draft() {
            let pr = PullRequest {
                review_decision: known(ReviewDecision::ChangesRequested),
                ..draft()
            };
            assert_eq!(timed(&pr), reasons(&[StaleDraft]));
        }

        #[test]
        fn someone_elses_draft_never_flags() {
            let pr = PullRequest {
                roles: roles(&[ReviewRequested]),
                mergeable: known(Mergeable::Conflicting),
                ..draft()
            };
            assert_eq!(timed(&pr), reasons(&[]));
        }

        #[test]
        fn authored_draft_with_review_requested_role_still_flags() {
            let pr = PullRequest {
                roles: roles(&[Author, ReviewRequested]),
                mergeable: known(Mergeable::Conflicting),
                ..draft()
            };
            assert_eq!(timed(&pr), reasons(&[Conflict]));
        }

        #[test]
        fn disabled_without_a_clock() {
            assert_eq!(bare(&draft()), reasons(&[]));
        }

        #[test]
        fn unparseable_updated_at_is_not_stale_draft() {
            for bad in ["", "not-a-date"] {
                let pr = PullRequest {
                    updated_at: bad.to_owned(),
                    ..draft()
                };
                assert_eq!(timed(&pr), reasons(&[]), "{bad:?}");
            }
        }
    }

    mod is_stale {
        use super::*;

        fn stale(updated: &str) -> bool {
            utc_clock().is_stale(updated)
        }

        #[test]
        fn old_timestamp_is_stale() {
            assert!(stale("2026-07-01T12:00:00Z"));
        }

        #[test]
        fn recent_timestamp_is_not_stale() {
            assert!(!stale("2026-07-26T12:00:00Z"));
        }

        #[test]
        fn boundary_is_stale() {
            assert!(stale("2026-07-24T12:00:00Z"));
        }

        #[test]
        fn unparseable_is_not_stale() {
            for bad in ["", "not-a-date", "2026-13-99"] {
                assert!(!stale(bad), "{bad:?}");
            }
        }

        #[test]
        fn naive_timestamp_is_not_stale() {
            // Without an offset it can't be compared safely: no nudge.
            assert!(!stale("2026-07-01T12:00:00"));
        }
    }

    /// A threshold in days counts local calendar days, not 24-hour spans.
    mod is_stale_counting_days {
        use super::*;

        fn stale(updated: Timestamp, now: Timestamp, after: Duration) -> bool {
            clock(now, after, false, paris()).is_stale(&updated.to_string())
        }

        #[test]
        fn stale_from_the_midnight_opening_the_nth_day() {
            // Monday 16:00 + 3d: stale from Thursday 00:00, not 16:00.
            let updated = at_paris(2026, 7, 20, 16, 0);
            assert!(!stale(updated, at_paris(2026, 7, 22, 23, 59), STALE_AFTER));
            assert!(stale(updated, at_paris(2026, 7, 23, 0, 0), STALE_AFTER));
        }

        #[test]
        fn hour_of_the_update_does_not_matter() {
            for hour in [0, 23] {
                let updated = at_paris(2026, 7, 20, hour, 0);
                assert!(
                    stale(updated, at_paris(2026, 7, 23, 0, 0), STALE_AFTER),
                    "{hour}h"
                );
            }
        }

        #[test]
        fn local_calendar_decides_the_day() {
            // 23:30 UTC on Monday is already Tuesday 01:30 in Paris.
            let updated: Timestamp = "2026-07-20T23:30:00Z".parse().unwrap();
            assert!(!stale(updated, at_paris(2026, 7, 23, 12, 0), STALE_AFTER));
            assert!(stale(updated, at_paris(2026, 7, 24, 0, 0), STALE_AFTER));
        }

        #[test]
        fn hours_stay_an_exact_span() {
            let updated = at_paris(2026, 7, 20, 16, 0);
            assert!(!stale(
                updated,
                at_paris(2026, 7, 23, 15, 59),
                Duration::hours(72)
            ));
            assert!(stale(
                updated,
                at_paris(2026, 7, 23, 16, 0),
                Duration::hours(72)
            ));
        }

        #[test]
        fn offsets_in_the_timestamp_are_honoured() {
            // GitHub sends UTC, but any explicit offset reads the same.
            let clock = clock(at_paris(2026, 7, 23, 0, 0), STALE_AFTER, false, paris());
            assert!(clock.is_stale("2026-07-20T16:00:00+02:00"));
            assert!(!clock.is_stale("2026-07-21T00:30:00+02:00"));
        }
    }

    /// Saturdays and Sundays do not count, on the local calendar.
    mod is_stale_skipping_weekends {
        use super::*;

        fn stale(updated: Timestamp, now: Timestamp, after: Duration) -> bool {
            clock(now, after, true, paris()).is_stale(&updated.to_string())
        }

        #[test]
        fn weekend_does_not_count_toward_the_threshold() {
            // Thursday 10:00 → Monday 10:00: four calendar days, one working.
            let (updated, now) = (at_paris(2026, 7, 23, 10, 0), at_paris(2026, 7, 27, 10, 0));
            assert!(!stale(updated, now, STALE_AFTER));
            assert!(clock(now, STALE_AFTER, false, paris()).is_stale(&updated.to_string()));
        }

        #[test]
        fn three_working_days_from_thursday_land_on_tuesday() {
            let updated = at_paris(2026, 7, 23, 10, 0);
            assert!(!stale(updated, at_paris(2026, 7, 27, 23, 59), STALE_AFTER));
            assert!(stale(updated, at_paris(2026, 7, 28, 0, 0), STALE_AFTER));
        }

        #[test]
        fn a_working_week_is_the_same_weekday_next_week() {
            let updated = at_paris(2026, 7, 23, 10, 0);
            assert!(!stale(
                updated,
                at_paris(2026, 7, 29, 23, 59),
                Duration::days(5)
            ));
            assert!(stale(
                updated,
                at_paris(2026, 7, 30, 0, 0),
                Duration::days(5)
            ));
        }

        #[test]
        fn weekend_activity_counts_like_friday() {
            for day in [24, 25, 26] {
                let updated = at_paris(2026, 7, day, 14, 0);
                assert!(
                    !stale(updated, at_paris(2026, 7, 26, 23, 59), Duration::days(1)),
                    "{day}"
                );
                assert!(
                    stale(updated, at_paris(2026, 7, 27, 0, 0), Duration::days(1)),
                    "{day}"
                );
            }
        }

        #[test]
        fn several_weekends_are_all_skipped() {
            // Three weeks apart: 21 calendar days, 15 working ones.
            let (updated, now) = (at_paris(2026, 7, 6, 10, 0), at_paris(2026, 7, 27, 10, 0));
            assert!(stale(updated, now, Duration::days(15)));
            assert!(!stale(updated, now, Duration::days(16)));
        }

        #[test]
        fn dst_change_over_the_weekend_keeps_the_local_midnight() {
            // Paris falls back on Sunday 2026-10-25.
            let updated = at_paris(2026, 10, 22, 10, 0);
            assert!(!stale(updated, at_paris(2026, 10, 26, 23, 59), STALE_AFTER));
            assert!(stale(updated, at_paris(2026, 10, 27, 0, 0), STALE_AFTER));
        }

        #[test]
        fn hours_run_on_a_clock_stopped_over_the_weekend() {
            // Thursday 10:00 + 72 working hours is Tuesday 10:00, to the minute.
            let updated = at_paris(2026, 7, 23, 10, 0);
            assert!(!stale(
                updated,
                at_paris(2026, 7, 28, 9, 59),
                Duration::hours(72)
            ));
            assert!(stale(
                updated,
                at_paris(2026, 7, 28, 10, 0),
                Duration::hours(72)
            ));
        }

        #[test]
        fn hours_from_inside_a_weekend_start_on_monday() {
            // Saturday 14:00 + 1 working hour is Monday 01:00: none of the
            // weekend counts, including the part before the update.
            let updated = at_paris(2026, 7, 25, 14, 0);
            assert!(!stale(
                updated,
                at_paris(2026, 7, 27, 0, 59),
                Duration::hours(1)
            ));
            assert!(stale(
                updated,
                at_paris(2026, 7, 27, 1, 0),
                Duration::hours(1)
            ));
        }

        #[test]
        fn dst_change_keeps_the_wall_clock_for_hours() {
            // 72 working hours on the calendar, 73 of absolute time.
            let updated = at_paris(2026, 10, 22, 10, 0);
            assert!(!stale(
                updated,
                at_paris(2026, 10, 27, 9, 59),
                Duration::hours(72)
            ));
            assert!(stale(
                updated,
                at_paris(2026, 10, 27, 10, 0),
                Duration::hours(72)
            ));
        }

        #[test]
        fn timestamp_in_the_future_is_not_stale() {
            let (updated, now) = (at_paris(2026, 7, 27, 10, 0), at_paris(2026, 7, 20, 10, 0));
            assert!(!stale(updated, now, STALE_AFTER));
            assert!(!stale(updated, now, Duration::hours(72)));
        }

        #[test]
        fn unusable_timestamp_stays_quiet() {
            let clock = clock(NOW.parse().unwrap(), STALE_AFTER, true, paris());
            for bad in ["", "not-a-date", "2026-07-01T12:00:00"] {
                assert!(!clock.is_stale(bad), "{bad:?}");
            }
        }
    }

    /// Both nudges take the working-time clock under skip_weekends.
    mod stale_nudges_skipping_weekends {
        use super::*;

        fn reasons_on_monday(pr: &PullRequest, skip_weekends: bool) -> BTreeSet<Reason> {
            let clock = clock(
                at_paris(2026, 7, 27, 10, 0),
                STALE_AFTER,
                skip_weekends,
                paris(),
            );
            attention_reasons(pr, Some(&clock))
        }

        fn thursday() -> String {
            at_paris(2026, 7, 23, 10, 0).to_string()
        }

        #[test]
        fn weekend_holds_back_the_stale_nudge() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                review_decision: known(ReviewDecision::ReviewRequired),
                updated_at: thursday(),
                ..pr()
            };
            assert_eq!(reasons_on_monday(&pr, true), reasons(&[]));
            assert_eq!(reasons_on_monday(&pr, false), reasons(&[Stale]));
        }

        #[test]
        fn weekend_holds_back_the_stale_draft_nudge() {
            let pr = PullRequest {
                roles: roles(&[Author]),
                is_draft: true,
                updated_at: thursday(),
                ..pr()
            };
            assert_eq!(reasons_on_monday(&pr, true), reasons(&[]));
            assert_eq!(reasons_on_monday(&pr, false), reasons(&[StaleDraft]));
        }
    }
}
