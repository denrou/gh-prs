//! gh-issues: the GitHub issues you own, created, are assigned or commented
//! on, in one place, through the `gh` CLI.
//!
//! The binary in `main.rs` stays a thin shell over this library.

pub mod attention;
pub mod cli;
pub mod filter;
pub mod gh;
pub mod issue;
