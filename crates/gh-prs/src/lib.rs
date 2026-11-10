//! gh-prs: GitHub pull requests that need your attention, through the `gh` CLI.
//!
//! The binary in `main.rs` stays a thin shell over this library.

pub mod attention;
pub mod cli;
pub mod config;
pub mod gh;
pub mod hide;
pub mod merge;
pub mod mute;
pub mod pr;
pub mod snooze;
pub mod store;

pub use gh_common::duration;
