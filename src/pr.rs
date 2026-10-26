//! The pull request as the rest of the tool sees it, and the type model the
//! fail-safe rules rest on (docs/rust-port.md, Type model).
//!
//! An unknown is a value, not a default: it is collapsed while parsing only
//! when every consumer agrees on the direction (`checks`, `stacked`,
//! `unresolved_feedback`), and otherwise carried to the policy code, whose
//! exhaustive `match` has to decide.

use std::collections::BTreeSet;

/// A GitHub enum value as received: a variant this version knows, a value it
/// doesn't (kept verbatim for `--json`), or null/absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported<T> {
    Known(T),
    Unrecognised(String),
    Missing,
}

impl<T: GraphqlEnum> Reported<T> {
    /// The value as GitHub spells it, `""` when missing: what `--json` prints.
    #[must_use]
    pub fn graphql_name(&self) -> &str {
        match self {
            Self::Known(value) => value.graphql_name(),
            Self::Unrecognised(value) => value,
            Self::Missing => "",
        }
    }
}

/// A GitHub enum, read from and printed as its GraphQL name. One table per
/// enum, so parsing and `--json` can't drift apart.
pub trait GraphqlEnum: Copy + 'static {
    const ALL: &'static [Self];

    fn graphql_name(self) -> &'static str;

    fn from_graphql(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|value| value.graphql_name() == name)
    }
}

/// `PullRequest.state`. Searches only return open PRs; the merge preflight
/// and the prune commands are the ones that see the others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

/// `PullRequest.mergeable`. `Unknown` is GitHub's own value, reported while
/// it recomputes mergeability (e.g. right after a push).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

/// `PullRequest.reviewDecision`. `Reported::Missing` is a legitimate value
/// here: no review policy applies to the PR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

/// `PullRequestReview.state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// The CI rollup, already collapsed: every consumer agrees that an
/// unrecognised or missing rollup state counts as still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    /// A head commit with no rollup at all: no checks are configured.
    NoneConfigured,
    Success,
    Failure,
    Pending,
}

impl GraphqlEnum for PrState {
    const ALL: &'static [Self] = &[Self::Open, Self::Closed, Self::Merged];

    fn graphql_name(self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Closed => "CLOSED",
            Self::Merged => "MERGED",
        }
    }
}

impl GraphqlEnum for Mergeable {
    const ALL: &'static [Self] = &[Self::Mergeable, Self::Conflicting, Self::Unknown];

    fn graphql_name(self) -> &'static str {
        match self {
            Self::Mergeable => "MERGEABLE",
            Self::Conflicting => "CONFLICTING",
            Self::Unknown => "UNKNOWN",
        }
    }
}

impl GraphqlEnum for ReviewDecision {
    const ALL: &'static [Self] = &[Self::Approved, Self::ChangesRequested, Self::ReviewRequired];

    fn graphql_name(self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::ChangesRequested => "CHANGES_REQUESTED",
            Self::ReviewRequired => "REVIEW_REQUIRED",
        }
    }
}

impl GraphqlEnum for ReviewState {
    const ALL: &'static [Self] = &[
        Self::Approved,
        Self::ChangesRequested,
        Self::Commented,
        Self::Dismissed,
        Self::Pending,
    ];

    fn graphql_name(self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::ChangesRequested => "CHANGES_REQUESTED",
            Self::Commented => "COMMENTED",
            Self::Dismissed => "DISMISSED",
            Self::Pending => "PENDING",
        }
    }
}

impl Checks {
    /// The normalized rollup as `--json` prints it; `""` is no checks.
    #[must_use]
    pub const fn graphql_name(self) -> &'static str {
        match self {
            Self::NoneConfigured => "",
            Self::Success => "SUCCESS",
            Self::Failure => "FAILURE",
            Self::Pending => "PENDING",
        }
    }
}

/// A commit oid, never empty: an empty one from GitHub parses as `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Oid(String);

impl Oid {
    #[must_use]
    pub fn new(oid: &str) -> Option<Self> {
        (!oid.is_empty()).then(|| Self(oid.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The viewer's latest review on the PR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MyReview {
    pub state: Reported<ReviewState>,
    /// The commit the review was submitted against; `None` when GitHub no
    /// longer links one (e.g. it was force-pushed away).
    pub commit: Option<Oid>,
}

/// The standing CHANGES_REQUESTED reviews, from `latestOpinionatedReviews`
/// (which, unlike `latestReviews`, survives a re-request).
///
/// An empty list means "none in hand", which consumers must read as unknown,
/// not as "nobody objects": the data may be missing or hidden by the cap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangesRequested {
    /// One entry per review; `None` is a review against an unknown commit.
    pub commits: Vec<Option<Oid>>,
    /// The connection came back full, so more reviews may be hidden.
    pub truncated: bool,
}

/// The PR's labels, which are evidence for a mute exemption only when the
/// list is known to be the whole set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Labels {
    Complete(Vec<String>),
    /// The block was missing or malformed, or came back at its cap.
    Partial(Vec<String>),
}

/// Why a search returned the PR. Declared in the alphabetical order of their
/// names, so a `BTreeSet<Role>` iterates in the order the store and
/// `--json` have always sorted them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Assignee,
    Author,
    Involves,
    ReviewRequested,
    ReviewedBy,
}

impl Role {
    pub const ALL: [Self; 5] = [
        Self::Assignee,
        Self::Author,
        Self::Involves,
        Self::ReviewRequested,
        Self::ReviewedBy,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Assignee => "assignee",
            Self::Author => "author",
            Self::Involves => "involves",
            Self::ReviewRequested => "review-requested",
            Self::ReviewedBy => "reviewed-by",
        }
    }
}

/// Why a PR needs the viewer's attention. Declared in alphabetical order of
/// their names, like [`Role`]; the display order of the sections is the
/// CLI's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    CiFailed,
    Conflict,
    NewCommits,
    Ready,
    Review,
    Stale,
    StaleDraft,
    Unresolved,
}

impl Reason {
    pub const ALL: [Self; 8] = [
        Self::CiFailed,
        Self::Conflict,
        Self::NewCommits,
        Self::Ready,
        Self::Review,
        Self::Stale,
        Self::StaleDraft,
        Self::Unresolved,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CiFailed => "ci-failed",
            Self::Conflict => "conflict",
            Self::NewCommits => "new-commits",
            Self::Ready => "ready",
            Self::Review => "review",
            Self::Stale => "stale",
            Self::StaleDraft => "stale-draft",
            Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    /// `owner/name`; an identity field, so required when parsing.
    pub repo: String,
    /// Attacker-controlled: control characters are stripped at ingestion.
    pub title: String,
    pub author: Option<String>,
    pub url: String,
    pub state: Reported<PrState>,
    /// Raw ISO text as GitHub sent it: sorting, display and `--json` use it
    /// as is, and the stale nudge parses it (an unparseable one stays quiet).
    pub updated_at: String,
    pub created_at: String,
    pub is_draft: bool,
    pub review_decision: Reported<ReviewDecision>,
    pub mergeable: Reported<Mergeable>,
    pub checks: Checks,
    /// `None` when the viewer never reviewed (or the 50-node cap hid it).
    pub my_review: Option<MyReview>,
    pub head: Option<Oid>,
    /// The viewer is personally on the requested-reviewers list, not merely
    /// through a team.
    pub review_requested_explicitly: bool,
    /// At least one review request is pending, from a user or a team.
    pub has_pending_review_request: bool,
    pub changes_requested: ChangesRequested,
    /// Some unresolved review thread's last comment is someone else's.
    pub unresolved_feedback: bool,
    /// The threads connection came back full, so `unresolved_feedback =
    /// false` can't be trusted.
    pub review_threads_truncated: bool,
    /// The base branch is the head of another open PR (an unknown count
    /// reads as stacked).
    pub stacked: bool,
    pub labels: Labels,
    pub roles: BTreeSet<Role>,
    pub attention: BTreeSet<Reason>,
}

impl PullRequest {
    /// `owner/name#number`, how the CLI names a PR in messages.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    #[must_use]
    pub fn is_authored(&self) -> bool {
        self.roles.contains(&Role::Author)
    }

    #[must_use]
    pub fn needs_attention(&self) -> bool {
        !self.attention.is_empty()
    }
}

/// Builders shared by the policy tests (attention, merge, mute).
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A non-draft PR in `acme/widgets` with every enriched field at its
    /// "nothing known" value; tests override what they exercise.
    pub fn pr() -> PullRequest {
        PullRequest {
            number: 1,
            repo: "acme/widgets".to_owned(),
            title: "Add feature".to_owned(),
            author: Some("octocat".to_owned()),
            url: "https://github.com/acme/widgets/pull/1".to_owned(),
            state: Reported::Missing,
            updated_at: "2026-07-15T12:00:00Z".to_owned(),
            created_at: "2026-07-01T12:00:00Z".to_owned(),
            is_draft: false,
            review_decision: Reported::Missing,
            mergeable: Reported::Missing,
            checks: Checks::NoneConfigured,
            my_review: None,
            head: None,
            review_requested_explicitly: false,
            has_pending_review_request: false,
            changes_requested: ChangesRequested::default(),
            unresolved_feedback: false,
            review_threads_truncated: false,
            stacked: false,
            labels: Labels::Partial(Vec::new()),
            roles: BTreeSet::new(),
            attention: BTreeSet::new(),
        }
    }

    pub fn roles(roles: &[Role]) -> BTreeSet<Role> {
        roles.iter().copied().collect()
    }

    pub fn reasons(reasons: &[Reason]) -> BTreeSet<Reason> {
        reasons.iter().copied().collect()
    }

    pub fn oid(oid: &str) -> Option<Oid> {
        Oid::new(oid)
    }

    pub fn my_review(state: ReviewState, commit: &str) -> Option<MyReview> {
        Some(MyReview {
            state: Reported::Known(state),
            commit: Oid::new(commit),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_declared_in_the_order_of_their_names() {
        let names: Vec<&str> = Role::ALL.iter().map(|r| r.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert!(Role::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn reasons_are_declared_in_the_order_of_their_names() {
        let names: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert!(Reason::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    }

    fn roundtrips<T: GraphqlEnum + PartialEq + std::fmt::Debug>(count: usize) {
        assert_eq!(T::ALL.len(), count, "ALL must list every variant");
        for value in T::ALL {
            assert_eq!(T::from_graphql(value.graphql_name()), Some(*value));
        }
    }

    #[test]
    fn graphql_names_roundtrip() {
        // The counts are the variant counts: a variant missing from ALL
        // would parse as unrecognised.
        roundtrips::<PrState>(3);
        roundtrips::<Mergeable>(3);
        roundtrips::<ReviewDecision>(3);
        roundtrips::<ReviewState>(5);
        assert_eq!(
            Reported::Known(Mergeable::Unknown).graphql_name(),
            "UNKNOWN"
        );
        assert_eq!(
            Reported::<Mergeable>::Unrecognised("MAYBE".to_owned()).graphql_name(),
            "MAYBE"
        );
        assert_eq!(Reported::<Mergeable>::Missing.graphql_name(), "");
    }

    #[test]
    fn an_empty_oid_is_no_oid() {
        assert_eq!(Oid::new(""), None);
        assert_eq!(
            Oid::new("abc").map(|o| o.as_str().to_owned()),
            Some("abc".to_owned())
        );
    }
}
