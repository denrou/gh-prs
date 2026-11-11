//! Everything gh-prs asks of GitHub, always through the `gh` CLI and the
//! user's existing `gh auth` session (`gh_common::gh` runs it).
//!
//! "Error" must never look like "nothing to do": every failure, including a
//! response that deviates from the expected envelope, is a [`GhError`].

mod fetch;
mod parse;
mod queries;

pub use fetch::{
    REVIEW_PAGE_LIMIT, SEARCH_LIMIT, approve_pr, count_prs, fetch_pr, fetch_pr_head, fetch_prs,
    merge_pr, resolve_pr,
};
pub use gh_common::gh::{GH_TIMEOUT, GhCli, GhError, Output, Runner};
use gh_common::gh::{SearchResult, Task, flags, gather, graphql, nodes_by_id, search_ids};
