//! Borderless tables laid out for the terminal's width.

use anstyle::Style;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::{DIM, link, out, paint};

/// Below this a title stops being readable.
pub const TITLE_MIN: usize = 20;

/// One table cell: plain text (measured for the layout), its style, and an
/// optional hyperlink.
pub struct Cell {
    pub text: String,
    pub style: Style,
    pub url: String,
}

/// A cell without a link.
pub fn cell(text: impl Into<String>, style: Style) -> Cell {
    Cell {
        text: text.into(),
        style,
        url: String::new(),
    }
}

/// Which columns give way when the screen is too narrow for a table.
pub struct Layout {
    /// The title's column, cut to `title_cap` whatever the screen.
    pub title: usize,
    pub title_cap: usize,
    /// The width the title shrinks to first.
    pub title_min: usize,
    /// The column that shrinks next, and its floor; then the title again.
    pub gives_way: Option<(usize, usize)>,
}

/// A borderless, headerless table with two spaces between columns, each
/// cut ending in an ellipsis. The other columns never shrink, and no
/// column shrinks below its ellipsis. A `layout` naming a column the rows
/// don't have leaves that column alone.
pub fn print_table(rows: &[Vec<Cell>], layout: &Layout, screen: usize) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return;
    }
    let mut widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| cell.text.width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    if let Some(title) = widths.get_mut(layout.title) {
        *title = (*title).min(layout.title_cap);
    }
    let total = widths.iter().sum::<usize>() + 2 * (columns - 1);
    let mut excess = total.saturating_sub(screen);
    let steps = [
        Some((layout.title, layout.title_min)),
        layout.gives_way,
        Some((layout.title, 1)),
    ];
    for (column, floor) in steps.into_iter().flatten() {
        let Some(width) = widths.get_mut(column) else {
            continue;
        };
        let give = excess.min(width.saturating_sub(floor.max(1)));
        *width -= give;
        excess -= give;
    }
    for row in rows {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            let text = truncate(&cell.text, *width);
            let padding = width.saturating_sub(text.width());
            line.push_str(&paint(&link(&text, &cell.url), cell.style));
            line.push_str(&" ".repeat(padding + 2));
        }
        out(line.trim_end());
    }
}

/// `text` cut to `width` columns, the last one an ellipsis when cut (and
/// nothing at all in no column).
#[must_use]
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
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

/// A section title with its count.
pub fn header(title: &str, style: Style, count: usize) {
    out(&format!(
        "{} {}",
        paint(title, style),
        paint(&format!("({count})"), DIM)
    ));
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
        assert_eq!(truncate("Fix the parser", 0), "");
        assert_eq!(truncate("", 0), "");
        // Wide characters count double.
        assert_eq!(truncate("日本語のタイトル", 7), "日本語…");
    }
}
