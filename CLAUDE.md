# CLAUDE.md

Guidance for Claude Code in this repository. `ARCHITECTURE.md` has the module map, the attention rules, the stores, the merge preflight and the contracts between modules: read it before touching `crates/`. This file is the workflow and the decisions.

## Commands

```bash
cargo run -- [args]         # Run the CLI (default: PRs needing attention)
cargo run -- -c             # PRs you created
cargo run -- -r             # PRs awaiting your review
cargo fmt                   # Format
cargo clippy --all-targets -- -D warnings  # Lint, warnings are errors
cargo test --lib            # Unit tests
cargo test --test conformance              # Conformance suite
BLESS=1 cargo test --test conformance      # Rewrite expected/ from the binary, then review the diff
cargo install --locked --path crates/gh-prs  # Put the build in ~/.cargo/bin to try it for real
prettier --write <files.md>  # Markdown, only the files touched
```

The crate is `crates/gh-prs`, in a Cargo workspace; `cargo` commands run from the root. Conformance scenarios live in `crates/gh-prs/tests/conformance/scenarios/`, one directory each; the scenario format is documented at the top of `tests/conformance/main.rs` and `fake_gh.rs` there. A new or changed scenario is blessed and its `expected/` diff reviewed by hand before it is committed: `expected/` is the spec, so a bless that changes behaviour nobody meant to change is a regression, not an update.

## Workflow

Denis's personal project (`github.com/denrou/gh-prs`, public, MIT). It runs the pull request flow of Denis's `personal-repos.md` rule (branches, squash merges, review, merge, release, ideas, backlog); only what this repository does differently is written here.

- **Denis tries first** a change to what `gh prs merge` does or reads (`merge.rs`, `cli/merge.rs`, the approve and merge calls, and the parsing of any field the preflight checks: state, draft, mergeability, head, stacked, checks, review decision), because merging is the one irreversible path and the conformance suite only knows the fake `gh`'s idea of GitHub's answers. Only he runs it against real pull requests.
- **Release.** The version is `version` under `[workspace.package]` in the root `Cargo.toml` (`cargo build` so `Cargo.lock` follows), and the formula is `gh-prs`. The tag push runs `.github/workflows/release.yml`: it checks the tag against the crate version and reads its notes (a tag without them fails there, before the builds), lints and tests, builds the macOS arm64 and Linux x86_64 binaries, publishes the GitHub Release with the tag's notes and the compare link, the archives (`gh-prs-vX.Y.Z-<target>.tar.gz`) and `checksums.txt`, then asks `denrou/homebrew-tap` (shared with slack and ggit) to rewrite the formula. `.github/workflows/ci.yml` runs fmt, clippy and the tests on every pull request and every push to `main`.
- **Live checks.** The views only read GitHub, so `cargo run -- --json`, `-c`, `-r`, `-a` and `--count` can be run from here against Denis's account to check an output. **Never run `merge` from here**, not even to see an error path: it approves and merges real pull requests. `snooze`, `unsnooze`, `hide` and `unhide` write Denis's own stores, so run them only with `XDG_CONFIG_HOME` pointing at a temporary directory; the conformance suite covers them otherwise.

## Decisions

Written down because none of them can be read from the code.

- **Rust, not Python (2026-11-05).** The Python version shipped up to 0.18.1 (still on PyPI); the Rust one was ported module by module behind a conformance suite blessed from Python, run side by side with it, then replaced it. The reasons: the fail-safe rules become types an exhaustive `match` enforces, a static binary instead of a Homebrew formula pulling in Python, and compile errors that point at the broken spot. Speed was not one: network calls dominate every command.
- **`gh` is the only network path.** No HTTP client and no token handling: authentication, Enterprise hosts and proxies stay `gh`'s job.
- **Blocking I/O, no async runtime.** At most about 20 requests per run, eight in parallel through `std::thread::scope`.
- **Releases mirror `denrou/slack` (2026-11-05).** One release workflow shape and one tap for both Rust tools; cargo-dist and the Intel macOS and Linux arm64 targets were left out, since nobody runs them.
- **Public repo, MIT**, like slack.
- **A Cargo workspace (2026-11-10).** `gh issues`, the same idea for issues, needs the same plumbing (the `gh` runner, the stores, durations, the table and the conformance harness), and a second repository would have copied about ten modules and let them drift. So the tools are crates of one workspace, with one version and one release. Grouping every `gh` alias in one repository was set aside: `gh-notifications` is Python and shares no code.

A decision taken but not built yet lives in its issue, with the design: the stale nudge ignoring merges of the base branch (2026-10-27) is #21.
