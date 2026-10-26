//! Terminal output: styled text, the standard messages, and the spinner.
//!
//! Text is styled with ANSI codes and written through `anstream`, which
//! strips them when the stream is not a terminal, under `NO_COLOR`, or with
//! `--no-color`. Nothing here interprets markup, so text from GitHub (PR
//! titles, logins) can never be read as styling.

use std::time::Duration;

use anstyle::{AnsiColor, Style};
use indicatif::{ProgressBar, ProgressStyle};

pub const BOLD: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();

pub const fn fg(color: AnsiColor) -> Style {
    Style::new().fg_color(Some(anstyle::Color::Ansi(color)))
}

pub const fn bold(color: AnsiColor) -> Style {
    fg(color).bold()
}

/// `text` in `style`.
pub fn paint(text: &str, style: Style) -> String {
    format!("{style}{text}{style:#}")
}

/// `text` as a terminal hyperlink to `url` (OSC 8), stripped with the
/// colours when the output isn't a terminal.
pub fn link(text: &str, url: &str) -> String {
    if url.is_empty() {
        text.to_owned()
    } else {
        format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
    }
}

pub fn out(line: &str) {
    anstream::println!("{line}");
}

pub fn err(line: &str) {
    anstream::eprintln!("{line}");
}

pub fn error(message: &str) {
    err(&format!(
        "{} {message}",
        paint("Error:", fg(AnsiColor::Red))
    ));
}

pub fn warn(message: &str) {
    err(&format!(
        "{} {message}",
        paint("Warning:", fg(AnsiColor::Yellow))
    ));
}

/// The width tables are laid out for: `COLUMNS` when set (as rich did),
/// else the terminal's, else 80.
pub fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.trim().parse().ok())
        .or_else(|| terminal_size::terminal_size().map(|(w, _)| usize::from(w.0)))
        .unwrap_or(80)
}

/// A spinner on stderr while `work` runs; hidden when stderr isn't a
/// terminal. `work` gets a way to print a warning without garbling it.
pub fn spinning<T>(message: &str, work: impl FnOnce(&dyn Fn(&str)) -> T) -> T {
    let spinner = ProgressBar::new_spinner().with_message(message.to_owned());
    if let Ok(style) = ProgressStyle::with_template("{spinner} {msg}") {
        spinner.set_style(style);
    }
    spinner.enable_steady_tick(Duration::from_millis(80));
    let result = work(&|message| spinner.suspend(|| warn(message)));
    spinner.finish_and_clear();
    result
}
