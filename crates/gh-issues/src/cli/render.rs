//! The tables and `--json`.

use anstyle::{AnsiColor, Style};
use gh_common::table::{Cell, Layout, TITLE_MIN, cell, header, print_table};
use gh_common::ui::{BOLD, DIM, bold, fg, out};
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

pub fn json(issues: &[&Issue]) -> Result<String, serde_json::Error> {
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

/// The title is cut to 80 columns; on a narrow screen it shrinks to
/// [`TITLE_MIN`], then the labels to an ellipsis, then the title again. The
/// number (the link) and the date never shrink.
const LAYOUT: Layout = Layout {
    title: 1,
    title_cap: 80,
    title_min: TITLE_MIN,
    gives_way: Some((2, 1)),
};

/// One table per repository, in the order of their most recently updated
/// issue; within one, most recently updated first.
pub fn by_repository(issues: &[&Issue], screen: usize) {
    let mut repos: Vec<&str> = Vec::new();
    for issue in issues {
        if !repos.contains(&issue.repo.as_str()) {
            repos.push(&issue.repo);
        }
    }
    for (index, repo) in repos.iter().enumerate() {
        let rows: Vec<Vec<Cell>> = issues
            .iter()
            .filter(|issue| issue.repo == *repo)
            .map(|issue| {
                vec![
                    Cell {
                        text: format!("#{}", issue.number),
                        style: BOLD,
                        url: issue.url.clone(),
                    },
                    cell(issue.title.as_str(), Style::new()),
                    cell(issue.labels.join(", "), fg(AnsiColor::Yellow)),
                    cell(issue.updated_at.split('T').next().unwrap_or_default(), DIM),
                ]
            })
            .collect();
        if index > 0 {
            out("");
        }
        header(repo, bold(AnsiColor::Cyan), rows.len());
        print_table(&rows, &LAYOUT, screen);
    }
}
