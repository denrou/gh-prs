# CLAUDE.md

Guidance for Claude Code in this repository. `ARCHITECTURE.md` has the workspace's shape and, for gh-prs, the module map, the attention rules, the stores, the merge preflight and the contracts between modules; `crates/gh-issues/ARCHITECTURE.md` is the same for gh-issues. Read the one that applies before touching `crates/`. This file is the workflow and the decisions.

## Commands

```bash
cargo run -p gh-prs -- [args]  # Run gh prs (default: PRs needing attention)
cargo run -p gh-prs -- -c      # PRs you created
cargo run -p gh-prs -- -r      # PRs awaiting your review
cargo run -p gh-issues -- [args]  # Run gh issues
cargo fmt                   # Format
cargo clippy --all-targets -- -D warnings  # Lint, warnings are errors
cargo test --lib            # Unit tests, every crate (-p <crate> for one)
cargo test --test conformance              # Conformance suites, both tools
BLESS=1 cargo test -p <tool> --test conformance  # Rewrite one tool's expected/ from its binary, then review the diff
cargo install --locked --path crates/gh-prs  # Put the build in ~/.cargo/bin to try it for real (crates/gh-issues for gh issues)
prettier --write <files.md>  # Markdown, only the files touched
```

The tools are `crates/gh-prs` and `crates/gh-issues`, in a Cargo workspace with what they share: `crates/gh-common` (the plumbing) and `crates/gh-conformance` (the conformance harness). `cargo` commands run from the root. Conformance scenarios live in each tool's `tests/conformance/scenarios/`, one directory each; the scenario format is documented at the top of `crates/gh-conformance/src/lib.rs` and `fake.rs`, and of the tool's `tests/conformance/fake_gh.rs`. A new or changed scenario is blessed and its `expected/` diff reviewed by hand before it is committed: `expected/` is the spec, so a bless that changes behaviour nobody meant to change is a regression, not an update.

## Workflow

Denis's personal project (`github.com/denrou/gh-prs`, public, MIT). It runs the pull request flow of Denis's `personal-repos.md` rule (branches, squash merges, review, merge, release, ideas, backlog); only what this repository does differently is written here.

- **Denis tries first** a change to what `gh prs merge` does or reads (`merge.rs`, `cli/merge.rs`, the approve and merge calls, the `gh` runner in `gh-common` they go through, and the parsing of any field the preflight checks: state, draft, mergeability, head, stacked, checks, review decision), because merging is the one irreversible path and the conformance suite only knows the fake `gh`'s idea of GitHub's answers. Only he runs it against real pull requests.
- **Release.** The version is `version` under `[workspace.package]` in the root `Cargo.toml` (`cargo build` so `Cargo.lock` follows), and the formula is `gh-prs`. The tag push runs `.github/workflows/release.yml`: it checks the tag against the crate version and reads its notes (a tag without them fails there, before the builds), lints and tests, builds the macOS arm64 and Linux x86_64 binaries, publishes the GitHub Release with the tag's notes and the compare link, the archives (`gh-prs-vX.Y.Z-<target>.tar.gz`) and `checksums.txt`, then asks `denrou/homebrew-tap` (shared with slack and ggit) to rewrite the formula. `.github/workflows/ci.yml` runs fmt, clippy and the tests on every pull request and every push to `main`.
- **gh-issues is not released yet.** It is built and tested with the workspace, but the release workflow only packages `gh-prs`, until Denis asks for a release that ships it. It only reads GitHub, so nothing in it is reserved for Denis to try first.
- **Live checks.** `gh issues` only reads GitHub, and so do the views of `gh prs`, so `cargo run -p gh-issues`, and `cargo run -p gh-prs -- --json`, `-c`, `-r`, `-a` and `--count` can be run from here against Denis's account to check an output. **Never run `merge` from here**, not even to see an error path: it approves and merges real pull requests. `snooze`, `unsnooze`, `hide` and `unhide` write Denis's own stores, so run them only with `XDG_CONFIG_HOME` pointing at a temporary directory; the conformance suite covers them otherwise.

## Decisions

Written down because none of them can be read from the code.

- **Rust, not Python (2026-11-05).** The Python version shipped up to 0.18.1 (still on PyPI); the Rust one was ported module by module behind a conformance suite blessed from Python, run side by side with it, then replaced it. The reasons: the fail-safe rules become types an exhaustive `match` enforces, a static binary instead of a Homebrew formula pulling in Python, and compile errors that point at the broken spot. Speed was not one: network calls dominate every command.
- **`gh` is the only network path.** No HTTP client and no token handling: authentication, Enterprise hosts and proxies stay `gh`'s job.
- **Blocking I/O, no async runtime.** At most about 20 requests per run, eight in parallel through `std::thread::scope`.
- **Releases mirror `denrou/slack` (2026-11-05).** One release workflow shape and one tap for both Rust tools; cargo-dist and the Intel macOS and Linux arm64 targets were left out, since nobody runs them.
- **Public repo, MIT**, like slack.
- **A Cargo workspace (2026-11-10).** `gh issues`, the same idea for issues, needs the same plumbing (the `gh` runner, the stores, durations, the table and the conformance harness), and a second repository would have copied about ten modules and let them drift. So the tools are crates of one workspace, with one version and one release. A module moves to `gh-common` once the second tool uses it unchanged, not before (ARCHITECTURE.md, Shape). Grouping every `gh` alias in one repository was set aside: `gh-notifications` is Python and shares no code.

A decision taken but not built yet lives in its issue, with the design: the stale nudge ignoring merges of the base branch (2026-10-27) is #21.
