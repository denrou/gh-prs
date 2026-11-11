//! gh-issues's conformance suite: the scenarios under `scenarios/`, run by
//! the shared harness (`gh_conformance`, which documents the scenario
//! format) against the fake `gh` in `fake_gh.rs`.

// Deliberate: a broken fixture or harness failure should panic loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod fake_gh;

use std::process::ExitCode;

use gh_conformance::Suite;

fn main() -> ExitCode {
    gh_conformance::main(Suite {
        tool: "gh-issues",
        bin_var: "GH_ISSUES_BIN",
        default_bin: env!("CARGO_BIN_EXE_gh-issues"),
        scenarios: concat!(env!("CARGO_MANIFEST_DIR"), "/tests/conformance/scenarios"),
        store_files: &[],
        answer: fake_gh::answer,
        is_write: |_| false,
    })
}
