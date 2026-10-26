//! gh-prs: GitHub pull requests that need your attention, through the `gh` CLI.
//!
//! The Rust port of the Python package in `gh_prs/` (docs/rust-port.md); the
//! binary in `main.rs` stays a thin shell over this library.

pub mod attention;
pub mod cli;
pub mod config;
pub mod duration;
pub mod gh;
pub mod hide;
pub mod merge;
pub mod mute;
pub mod pr;
pub mod snooze;
pub mod store;
