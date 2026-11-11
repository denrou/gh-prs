//! What the gh tools of this workspace share, and nothing that knows about
//! pull requests or issues: running `gh`, the store files, durations,
//! terminal output and tables.

pub mod duration;
pub mod gh;
pub mod store;
pub mod table;
pub mod ui;
