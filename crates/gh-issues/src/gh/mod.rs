//! Everything gh-issues asks of GitHub, always through the `gh` CLI and the
//! user's existing `gh auth` session (`gh_common::gh` runs it). It only
//! reads.
//!
//! "Error" must never look like "nothing to do": every failure, including a
//! response that deviates from the expected envelope, is a `GhError`.

mod fetch;
mod parse;
mod queries;

pub use fetch::{SEARCH_MAX, SEARCH_PAGE, fetch_issues};
