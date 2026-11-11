//! The list and `--json`.

use gh_common::ui::{DIM, link, out, paint};
use serde::Serialize;

use crate::issue::Issue;

/// One issue in `--json`, a scripting interface: field names and order are
/// part of the contract.
#[derive(Serialize)]
struct Row<'a> {
    number: u64,
    repo: &'a str,
    title: &'a str,
    author: Option<&'a str>,
    url: &'a str,
    updated_at: &'a str,
    created_at: &'a str,
    labels: &'a [String],
    assignees: &'a [String],
    /// `null` when GitHub didn't say.
    comments: Option<u64>,
    last_comment: Option<LastComment<'a>>,
    roles: Vec<&'static str>,
}

#[derive(Serialize)]
struct LastComment<'a> {
    author: Option<&'a str>,
    created_at: &'a str,
}

pub fn json(issues: &[Issue]) -> Result<String, serde_json::Error> {
    let rows: Vec<Row<'_>> = issues
        .iter()
        .map(|issue| Row {
            number: issue.number,
            repo: &issue.repo,
            title: &issue.title,
            author: issue.author.as_deref(),
            url: &issue.url,
            updated_at: &issue.updated_at,
            created_at: &issue.created_at,
            labels: &issue.labels,
            assignees: &issue.assignees,
            comments: issue.comments,
            last_comment: issue.last_comment.as_ref().map(|comment| LastComment {
                author: comment.author.as_deref(),
                created_at: &comment.created_at,
            }),
            roles: issue.roles.iter().map(|role| role.name()).collect(),
        })
        .collect();
    serde_json::to_string_pretty(&rows)
}

/// One line per issue, most recently updated first.
pub fn list(issues: &[Issue]) {
    if issues.is_empty() {
        out(&paint("No open issues.", DIM));
        return;
    }
    for issue in issues {
        out(&format!(
            "{}  {}",
            link(&issue.id(), &issue.url),
            issue.title
        ));
    }
}
