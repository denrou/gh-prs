//! The tables and `--json`.

use std::collections::HashMap;

use anstyle::{AnsiColor, Style};
use gh_common::table::{Cell, Layout, TITLE_MIN, cell, header, print_table};
use gh_common::ui::{BOLD, DIM, bold, fg, out};
use serde::Serialize;

use crate::issue::{Issue, Reason};

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
    /// Why it needs you; empty when it doesn't.
    attention: Vec<&'static str>,
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
            attention: issue.attention.iter().map(|reason| reason.name()).collect(),
        })
        .collect();
    serde_json::to_string_pretty(&rows)
}

/// The "needs you" sections, in display order.
const SECTIONS: [(Reason, &str, Style); 3] = [
    (
        Reason::Reply,
        "Waiting for your reply",
        bold(AnsiColor::Magenta),
    ),
    (
        Reason::Triage,
        "New in your repositories",
        bold(AnsiColor::Green),
    ),
    (Reason::Assigned, "Assigned to you", bold(AnsiColor::Blue)),
];

/// The floor of the repository column on a narrow screen.
const REPO_MIN: usize = 8;

/// One section per reason: the repository, the number, the title, who is
/// waiting (the last commenter, else the author) and the last update. On a
/// narrow screen the title shrinks first, then the repository.
pub fn attention(issues: &[&Issue], screen: usize) {
    let layout = Layout {
        title: 2,
        title_cap: 70,
        title_min: TITLE_MIN,
        gives_way: Some((0, REPO_MIN)),
    };
    let mut first = true;
    for (reason, title, style) in SECTIONS {
        let rows: Vec<Vec<Cell>> = issues
            .iter()
            .filter(|issue| issue.attention.contains(&reason))
            .map(|issue| {
                let who = issue
                    .last_comment
                    .as_ref()
                    .map_or(issue.author.as_deref(), |comment| comment.author.as_deref());
                vec![
                    cell(
                        issue.repo.rsplit('/').next().unwrap_or(&issue.repo),
                        fg(AnsiColor::Cyan),
                    ),
                    number_cell(issue),
                    cell(issue.title.as_str(), Style::new()),
                    cell(who.unwrap_or("—"), fg(AnsiColor::Magenta)),
                    cell(updated_date(issue), DIM),
                ]
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !first {
            out("");
        }
        first = false;
        header(title, style, rows.len());
        print_table(&rows, &layout, screen);
    }
}

fn number_cell(issue: &Issue) -> Cell {
    Cell {
        text: format!("#{}", issue.number),
        style: BOLD,
        url: issue.url.clone(),
    }
}

/// A date GitHub didn't send shows as unknown, not as blank.
fn updated_date(issue: &Issue) -> &str {
    match issue.updated_at.split('T').next() {
        Some(date) if !date.is_empty() => date,
        Some(_) | None => "—",
    }
}

/// The title is cut to 80 columns; on a narrow screen it shrinks to
/// [`TITLE_MIN`], then the labels (when the table has them) to an
/// ellipsis, then the title again. The number (the link) and the date
/// never shrink.
const fn layout(with_labels: bool) -> Layout {
    Layout {
        title: 1,
        title_cap: 80,
        title_min: TITLE_MIN,
        gives_way: if with_labels { Some((2, 1)) } else { None },
    }
}

/// One table per repository, in the order of their most recently updated
/// issue; within one, most recently updated first.
pub fn by_repository(issues: &[&Issue], screen: usize) {
    let mut repos: Vec<(&str, Vec<&Issue>)> = Vec::new();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    for issue in issues {
        let index = *index_of.entry(&issue.repo).or_insert_with(|| {
            repos.push((&issue.repo, Vec::new()));
            repos.len() - 1
        });
        repos[index].1.push(issue);
    }
    for (index, (repo, issues)) in repos.iter().enumerate() {
        // A table where no issue carries a label has no labels column.
        let with_labels = issues.iter().any(|issue| !issue.labels.is_empty());
        let rows: Vec<Vec<Cell>> = issues
            .iter()
            .map(|issue| {
                let mut row = vec![number_cell(issue), cell(issue.title.as_str(), Style::new())];
                if with_labels {
                    row.push(cell(issue.labels.join(", "), fg(AnsiColor::Yellow)));
                }
                row.push(cell(updated_date(issue), DIM));
                row
            })
            .collect();
        if index > 0 {
            out("");
        }
        header(repo, bold(AnsiColor::Cyan), rows.len());
        print_table(&rows, &layout(with_labels), screen);
    }
}
