//! The tables and `--json`.

use anstyle::{AnsiColor, Style};
use serde::Serialize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::ui::{BOLD, DIM, bold, fg, link, out, paint};
use crate::pr::{Checks, PullRequest, Reason, Reported, ReviewDecision};

/// The attention view's sections, in display order.
const SECTIONS: [(Reason, &str, Style); 8] = [
    (Reason::Review, "Needs your review", bold(AnsiColor::Cyan)),
    (
        Reason::NewCommits,
        "New commits since your review",
        bold(AnsiColor::Magenta),
    ),
    (Reason::Ready, "Ready to ship", bold(AnsiColor::Green)),
    (Reason::CiFailed, "CI failed", bold(AnsiColor::Red)),
    (
        Reason::Conflict,
        "Conflicts to resolve",
        bold(AnsiColor::Yellow),
    ),
    (
        Reason::Unresolved,
        "Unresolved comments — reply or resolve",
        bold(AnsiColor::Magenta),
    ),
    (
        Reason::Stale,
        "Waiting on review — time to nudge",
        bold(AnsiColor::Blue),
    ),
    (
        Reason::StaleDraft,
        "Drafts gone quiet — finish or mark ready",
        bold(AnsiColor::Blue),
    ),
];

/// One table cell: plain text (measured for the layout), its style, and an
/// optional hyperlink.
struct Cell {
    text: String,
    style: Style,
    url: String,
}

fn cell(text: impl Into<String>, style: Style) -> Cell {
    Cell {
        text: text.into(),
        style,
        url: String::new(),
    }
}

/// Below this the title stops being readable, so the repository gives way
/// first; the number, author and date never shrink (the number is the link).
const TITLE_MIN: usize = 20;
const REPO_MIN: usize = 8;
const REPO_COLUMN: usize = 0;

/// A borderless, headerless table with two spaces between columns. The
/// title is cut to `title_cap`; on a narrow screen it shrinks to
/// [`TITLE_MIN`], then the repository to [`REPO_MIN`], then the title again,
/// each cut ending in an ellipsis.
fn print_table(rows: &[Vec<Cell>], title_column: usize, title_cap: usize, screen: usize) {
    let Some(first) = rows.first() else { return };
    let mut widths: Vec<usize> = (0..first.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].text.width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    widths[title_column] = widths[title_column].min(title_cap);
    let total = widths.iter().sum::<usize>() + 2 * (widths.len() - 1);
    let mut excess = total.saturating_sub(screen);
    for (column, floor) in [
        (title_column, TITLE_MIN),
        (REPO_COLUMN, REPO_MIN),
        (title_column, 1),
    ] {
        let give = excess.min(widths[column].saturating_sub(floor));
        widths[column] -= give;
        excess -= give;
    }
    for row in rows {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            let text = truncate(&cell.text, widths[column]);
            let padding = widths[column] - text.width();
            line.push_str(&paint(&link(&text, &cell.url), cell.style));
            if column + 1 < row.len() {
                line.push_str(&" ".repeat(padding + 2));
            }
        }
        out(&line);
    }
}

/// `text` cut to `width` columns, the last one an ellipsis when cut.
fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut cut = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        cut.push(c);
        used += w;
    }
    cut.push('…');
    cut
}

fn repo_short(pr: &PullRequest) -> &str {
    pr.repo.rsplit('/').next().unwrap_or(&pr.repo)
}

fn updated_date(pr: &PullRequest) -> &str {
    pr.updated_at.split('T').next().unwrap_or_default()
}

fn number_cell(pr: &PullRequest) -> Cell {
    Cell {
        text: format!("#{}", pr.number),
        style: BOLD,
        url: pr.url.clone(),
    }
}

fn title_cell(pr: &PullRequest) -> Cell {
    // The draft marker is part of the text so the layout measures it.
    let text = if pr.is_draft {
        format!("(draft) {}", pr.title)
    } else {
        pr.title.clone()
    };
    cell(text, Style::new())
}

fn author_cell(pr: &PullRequest) -> Cell {
    cell(
        pr.author.clone().unwrap_or_default(),
        fg(AnsiColor::Magenta),
    )
}

fn review_cell(pr: &PullRequest) -> Cell {
    match &pr.review_decision {
        Reported::Known(ReviewDecision::Approved) => cell("Approved", fg(AnsiColor::Green)),
        Reported::Known(ReviewDecision::ChangesRequested) => {
            cell("Changes req", fg(AnsiColor::Red))
        }
        Reported::Known(ReviewDecision::ReviewRequired) => {
            cell("Review req", fg(AnsiColor::Yellow))
        }
        Reported::Unrecognised(value) => cell(value.clone(), fg(AnsiColor::White)),
        Reported::Missing => cell("—", DIM),
    }
}

fn checks_cell(pr: &PullRequest) -> Cell {
    match pr.checks {
        Checks::Success => cell("✓ pass", fg(AnsiColor::Green)),
        Checks::Failure => cell("✗ fail", fg(AnsiColor::Red)),
        Checks::Pending => cell("● running", fg(AnsiColor::Yellow)),
        Checks::NoneConfigured => cell("—", DIM),
    }
}

fn header(title: &str, style: Style, count: usize) {
    out(&format!(
        "{} {}",
        paint(title, style),
        paint(&format!("({count})"), DIM)
    ));
}

/// The default view: one section per reason, a PR in every section it
/// qualifies for.
pub fn attention(prs: &[PullRequest], screen: usize) {
    if !prs.iter().any(PullRequest::needs_attention) {
        out(&format!(
            "{} Nothing needs your attention.",
            paint("✓", fg(AnsiColor::Green))
        ));
        return;
    }
    for (reason, title, style) in SECTIONS {
        let group: Vec<&PullRequest> = prs
            .iter()
            .filter(|pr| pr.attention.contains(&reason))
            .collect();
        if group.is_empty() {
            continue;
        }
        // Sections listing other people's PRs show the author.
        let show_author = matches!(reason, Reason::Review | Reason::NewCommits);
        let rows: Vec<Vec<Cell>> = group
            .iter()
            .map(|pr| {
                let mut row = vec![
                    cell(repo_short(pr), fg(AnsiColor::Cyan)),
                    number_cell(pr),
                    title_cell(pr),
                ];
                if show_author {
                    row.push(author_cell(pr));
                }
                row.push(cell(updated_date(pr), DIM));
                row
            })
            .collect();
        header(title, style, group.len());
        print_table(&rows, 2, 70, screen);
        out("");
    }
}

/// The explicit views (`-c`, `-r`, `-a`): one flat list with the review and
/// CI columns.
pub fn list(prs: &[PullRequest], title: &str, style: Style, screen: usize) {
    if prs.is_empty() {
        out(&paint("No matching PRs.", DIM));
        return;
    }
    let rows: Vec<Vec<Cell>> = prs
        .iter()
        .map(|pr| {
            vec![
                cell(repo_short(pr), fg(AnsiColor::Cyan)),
                number_cell(pr),
                title_cell(pr),
                author_cell(pr),
                review_cell(pr),
                checks_cell(pr),
                cell(updated_date(pr), DIM),
            ]
        })
        .collect();
    header(title, style, prs.len());
    print_table(&rows, 2, 60, screen);
}

/// One PR in `--json`, a scripting interface: field names and order are
/// part of the contract, and serde keeps a struct's declaration order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonPr<'a> {
    repo: &'a str,
    number: u64,
    title: &'a str,
    author: &'a str,
    url: &'a str,
    is_draft: bool,
    review_decision: &'a str,
    checks_state: &'a str,
    mergeable: &'a str,
    my_review_state: &'a str,
    my_review_commit: &'a str,
    head_ref_oid: &'a str,
    review_requested_explicitly: bool,
    /// `""` for a review against an unknown commit, and one more `""` when
    /// the connection was truncated, as the Python version printed it.
    changes_requested_commits: Vec<&'a str>,
    has_pending_review_request: bool,
    unresolved_feedback: bool,
    stacked: bool,
    labels: Vec<&'a str>,
    roles: Vec<&'static str>,
    attention_reasons: Vec<&'static str>,
    updated_at: &'a str,
    created_at: &'a str,
}

pub fn json(prs: &[PullRequest]) -> String {
    let rows: Vec<JsonPr<'_>> = prs
        .iter()
        .map(|pr| {
            let my_review = pr.my_review.as_ref();
            let mut changes_requested: Vec<&str> = pr
                .changes_requested
                .commits
                .iter()
                .map(|oid| oid.as_ref().map_or("", |oid| oid.as_str()))
                .collect();
            if pr.changes_requested.truncated {
                changes_requested.push("");
            }
            let labels = match &pr.labels {
                crate::pr::Labels::Complete(labels) | crate::pr::Labels::Partial(labels) => labels,
            };
            JsonPr {
                repo: &pr.repo,
                number: pr.number,
                title: &pr.title,
                author: pr.author.as_deref().unwrap_or_default(),
                url: &pr.url,
                is_draft: pr.is_draft,
                review_decision: pr.review_decision.graphql_name(),
                checks_state: pr.checks.graphql_name(),
                mergeable: pr.mergeable.graphql_name(),
                my_review_state: my_review.map_or("", |review| review.state.graphql_name()),
                my_review_commit: my_review
                    .and_then(|review| review.commit.as_ref())
                    .map_or("", |oid| oid.as_str()),
                head_ref_oid: pr.head.as_ref().map_or("", |oid| oid.as_str()),
                review_requested_explicitly: pr.review_requested_explicitly,
                changes_requested_commits: changes_requested,
                has_pending_review_request: pr.has_pending_review_request,
                unresolved_feedback: pr.unresolved_feedback,
                stacked: pr.stacked,
                labels: labels.iter().map(String::as_str).collect(),
                roles: pr.roles.iter().map(|role| role.as_str()).collect(),
                attention_reasons: pr.attention.iter().map(|reason| reason.as_str()).collect(),
                updated_at: &pr.updated_at,
                created_at: &pr.created_at,
            }
        })
        .collect();
    // Serializing plain strings and numbers can't fail.
    serde_json::to_string_pretty(&rows).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_width_with_an_ellipsis() {
        assert_eq!(truncate("Fix the parser", 20), "Fix the parser");
        assert_eq!(truncate("Fix the parser", 14), "Fix the parser");
        assert_eq!(truncate("Fix the parser", 8), "Fix the…");
        assert_eq!(truncate("Fix the parser", 1), "…");
        // Wide characters count double.
        assert_eq!(truncate("日本語のタイトル", 7), "日本語…");
    }
}
