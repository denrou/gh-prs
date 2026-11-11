//! What the gh tools of this workspace share, and nothing that knows about
//! pull requests or issues: running `gh`, the store files, durations and
//! terminal output.

pub mod duration;
pub mod gh;
pub mod store;
pub mod ui;
