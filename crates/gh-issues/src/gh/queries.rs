//! The GraphQL sent to GitHub, as static text bound with variables (`gh`'s
//! `-f`/`-F` flags), so no untrusted or dynamic text is ever spliced into a
//! query.

/// Every field a hydrated issue carries. Labels are read up to GitHub's
/// page maximum, so that the list is whole in practice; GitHub itself caps
/// assignees at 10.
pub const ISSUE_FRAGMENT: &str = "
fragment issueFields on Issue {
  number
  title
  url
  state
  updatedAt
  createdAt
  repository { nameWithOwner }
  author { login }
  labels(first: 100) { nodes { name } }
  assignees(first: 20) { nodes { login } }
  comments(last: 1) { totalCount nodes { author { login } createdAt } }
}
";

/// A search returns node ids only, a page at a time: hydrating the nodes
/// is what costs, and an issue matched by several searches is looked up
/// once. `viewer` is for the "needs you" reasons, which compare the last
/// commenter with it.
pub const SEARCH_QUERY: &str = "
query($q: String!, $limit: Int!, $after: String) {
  viewer { login }
  results: search(query: $q, type: ISSUE, first: $limit, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes { ... on Issue { id } }
  }
}
";

/// The ids found by the searches, hydrated in batches.
pub fn hydrate_query() -> String {
    format!(
        "
query($ids: [ID!]!) {{
  nodes(ids: $ids) {{ ... on Issue {{ id ...issueFields }} }}
}}
{ISSUE_FRAGMENT}"
    )
}
