//! The GraphQL sent to GitHub, as static text bound with variables (`gh`'s
//! `-f`/`-F` flags), so no untrusted or dynamic text is ever spliced into a
//! query.

/// Every field a hydrated PR carries. The `first:` caps must stay equal to
/// `REVIEW_PAGE_LIMIT` and `LABEL_PAGE_LIMIT` in `fetch`, which a test pins.
///
/// `reviewThreads` is gated by `$withThreads`: hydrating each thread's last
/// comment is the one measurably expensive field, and only the authored-only
/// `unresolved` reason reads it.
pub const PR_FRAGMENT: &str = "
fragment prFields on PullRequest {
  number
  title
  url
  state
  updatedAt
  createdAt
  isDraft
  reviewDecision
  mergeable
  headRefOid
  baseRef { associatedPullRequests(states: OPEN) { totalCount } }
  repository { nameWithOwner }
  author { login }
  labels(first: 20) { nodes { name } }
  reviewRequests(first: 50) {
    nodes { requestedReviewer { __typename ... on User { login } } }
  }
  latestReviews(first: 50) { nodes { author { login } state commit { oid } } }
  latestOpinionatedReviews(first: 50) { nodes { state commit { oid } } }
  reviewThreads(first: 50) @include(if: $withThreads) {
    nodes { isResolved comments(last: 1) { nodes { author { login } } } }
  }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
}
";

/// A search returns node ids only: hydrating the nodes is what costs, and
/// GitHub answers HTTP 502 to any request running past ~10s, which a 50-PR
/// hydrated search reached.
pub const SEARCH_QUERY: &str = "
query($q: String!, $limit: Int!) {
  viewer { login }
  results: search(query: $q, type: ISSUE, first: $limit) {
    issueCount
    nodes { ... on PullRequest { id } }
  }
}
";

/// Count only: an order of magnitude faster than a hydrated search, and
/// exact beyond the node cap.
pub const COUNT_QUERY: &str = "
query($q: String!) {
  results: search(query: $q, type: ISSUE, first: 1) { issueCount }
}
";

/// The ids found by the searches, hydrated in batches.
pub fn hydrate_query() -> String {
    format!(
        "
query($ids: [ID!]!, $withThreads: Boolean!) {{
  nodes(ids: $ids) {{ ... on PullRequest {{ id ...prFields }} }}
}}
{PR_FRAGMENT}"
    )
}

/// One PR looked up by URL (the merge preflight and the prune commands).
/// `resource` resolves any GitHub URL, so the typename tells a PR from an
/// issue; the viewer's login classifies authorship as a search would.
pub fn pr_query() -> String {
    format!(
        "
query($url: URI!, $withThreads: Boolean!) {{
  viewer {{ login }}
  resource(url: $url) {{ __typename ... on PullRequest {{ ...prFields }} }}
}}
{PR_FRAGMENT}"
    )
}
