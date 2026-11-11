//! The files gh-prs keeps under `$XDG_CONFIG_HOME/gh-prs/`; the JSON I/O is
//! `gh_common::store`.

use std::path::PathBuf;

pub use gh_common::store::{format_timestamp, read_json, write_json};

/// `$XDG_CONFIG_HOME/gh-prs/<name>`, or `~/.config/gh-prs/<name>`.
#[must_use]
pub fn store_path(name: &str) -> PathBuf {
    gh_common::store::store_path("gh-prs", name)
}
