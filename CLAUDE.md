# CLAUDE.md

Guidance for Claude Code in this repository. `ARCHITECTURE.md` has the module map, the attention rules, the stores, the merge preflight and the contracts between modules: read it before touching `src/`. This file is the workflow and the decisions.

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
cargo install --locked --path .  # Put the build in ~/.cargo/bin to try it for real
prettier --write "**/*.md"  # Markdown
```

Conformance scenarios live in `tests/conformance/scenarios/`, one directory each; the scenario format is documented at the top of `tests/conformance/main.rs` and `fake_gh.rs`. A new or changed scenario is blessed and its `expected/` diff reviewed by hand before it is committed: `expected/` is the spec, so a bless that changes behaviour nobody meant to change is a regression, not an update.

## Workflow

Denis's personal project (`github.com/denrou/gh-prs`), outside the Centreon organisation. The Centreon conventions (`/create-pr`, `/check-pr`, Jira references) do not apply, and `/create-pr` must not be used. The workflow is the one `denrou/slack` runs.

Denis leads the project as product owner; Claude is the developer and owns the code, its architecture and its quality.

- **One branch and one pull request per change (since 2026-11-05).** Until then changes were committed straight on `main` and Denis pushed them, which kept one session working at a time and turned each change into a release. Now each session works on its own branch (`feat/…`, `fix/…`, `docs/…`, `chore/…`), from `origin/main` after a fetch, in its own worktree when sessions run side by side. Format, lint and test before each commit; run Prettier on any Markdown touched. `ARCHITECTURE.md`, the README and the decisions below change in the same pull request as the code they describe.
- **`main` takes squash-merged pull requests only.** A ruleset requires the pull request and CI's `check` job, and the pull request's title and body become the commit on `main`: the title is a conventional commit (`feat:`, `fix:`, `docs:`, `chore:`, `ci:`), the body is written as a commit body (writing-voice rule), with `Closes #N` for the issues it settles.
- **The branch is pushed and the pull request opened once the change is ready** (formatted, linted, tested, the docs that describe it updated), without waiting to be asked: Denis asked for it on 2026-11-05. When `main` moves under an open pull request, merge `main` into the branch: rebasing a pushed branch would take a force push.
- **Each pull request is reviewed on its own, before it is merged**: `/code-review` on it, plus the pr-review-toolkit agents when it is large. Fix what matters in the branch; what is left out on purpose becomes an issue labelled `deferred`, with its reason and the review it came from, linked from the pull request.
- **Claude merges once the review is through (since 2026-11-05).** A pull request is squash-merged (`gh pr merge --squash`, with its title and body as the commit) once `/code-review` ran on it, its blocking findings are fixed in the branch, the rest are `deferred` issues linked from it, and CI's `check` passed. One exception: a change whose only proof is a live write to GitHub (`gh prs merge`, the approve before it) waits for Denis to try the branch (`cargo install --locked --path <checkout>` puts it in `~/.cargo/bin`), since only he runs those against real pull requests.
- **Merging is not releasing.** Denis runs `main`; a release is the Homebrew binary, cut when a batch is worth it. Never bump the version or create a tag on your own.
- **A release, when Denis asks for it**:
  1. Read the pull requests merged since the last tag (`git log v<previous>..origin/main`, one squash commit each) and draft the notes the GitHub Release shows: a few Markdown bullets on what changed for whoever uses the tool, a new subcommand or flag, a behaviour that moved, a fix that was felt, not the list of pull requests, which the compare link GitHub adds below them already gives.
  2. Open a `chore: release X.Y.Z` pull request that bumps `version` in `Cargo.toml` (`cargo build` so `Cargo.lock` follows), with the notes in its body, and merge it once CI passes.
  3. Once it is merged, fetch and create the annotated tag on its merge commit, the message being `vX.Y.Z`, a blank line, then the notes: `git tag -a vX.Y.Z --cleanup=whitespace -F <file> origin/main` (`--cleanup=whitespace` keeps a line that starts with `#`, which git would otherwise strip as a comment). Then push it: `git push origin vX.Y.Z`.
  4. Watch the release with `gh run watch` and the tap with `gh run watch --repo denrou/homebrew-tap`, then report to Denis what shipped, with the notes.

  The tag push runs `.github/workflows/release.yml`: it checks the tag against the crate version and reads its notes (a tag without them fails there, before the builds), lints and tests, builds the macOS arm64 and Linux x86_64 binaries, and publishes the GitHub Release with the tag's notes and the compare link, the archives (`gh-prs-vX.Y.Z-<target>.tar.gz`) and `checksums.txt`. Its last job asks the Homebrew tap (`denrou/homebrew-tap`, shared with slack and ggit) to rewrite its formula from `checksums.txt`; that job needs the `GH_TOKEN` secret on the repository and is skipped without it, in which case run `gh workflow run update-formula.yml --repo denrou/homebrew-tap -f formula=gh-prs -f version=X.Y.Z` by hand and watch it with `gh run watch --repo denrou/homebrew-tap`. Check the formula through `gh api repos/denrou/homebrew-tap/contents/Formula/gh-prs.rb`, not raw.githubusercontent.com, whose cache shows the old one for minutes. `.github/workflows/ci.yml` runs fmt, clippy and the tests on every pull request and every push to `main`.

- **Live checks.** The views only read GitHub, so `cargo run -- --json`, `-c`, `-r`, `-a` and `--count` can be run from here against Denis's account to check an output. **Never run `merge` from here**, not even to see an error path: it approves and merges real pull requests. `snooze`, `unsnooze`, `hide` and `unhide` write Denis's own stores, so run them only with `XDG_CONFIG_HOME` pointing at a temporary directory; the conformance suite covers them otherwise.

## Ideas

Denis asks for enhancements, fixes and features as they come to him in use, and wants a reaction, not an order taken. Give each one a moment of thought before building:

- When it is right as stated, build it, no question asked; one line on why it holds is enough.
- When it needs more thought, or another approach serves the same want better (a smaller change, another flag, a reason that already covers it, a cost it would carry later), say so first: the want as understood, the concern, the alternative with its trade-off, and a lean. Then build what he picks.

An idea not built now becomes an issue labelled `enhancement`, with the concern and the lean when there is one. A concern about one of several asks never holds up the others.

## Decisions

Written down because none of them can be read from the code.

- **Rust, not Python (2026-11-05).** The Python version shipped up to 0.18.1 (still on PyPI); the Rust one was ported module by module behind a conformance suite blessed from Python, run side by side with it, then replaced it. The reasons: the fail-safe rules become types an exhaustive `match` enforces, a static binary instead of a Homebrew formula pulling in Python, and compile errors that point at the broken spot. Speed was not one: network calls dominate every command.
- **`gh` is the only network path.** No HTTP client and no token handling: authentication, Enterprise hosts and proxies stay `gh`'s job.
- **Blocking I/O, no async runtime.** At most about 20 requests per run, eight in parallel through `std::thread::scope`.
- **Releases mirror `denrou/slack` (2026-11-05).** One release workflow shape and one tap for both Rust tools; cargo-dist and the Intel macOS and Linux arm64 targets were left out, since nobody runs them.
- **Public repo, MIT**, like slack.

## Backlog

In GitHub issues: `enhancement` for an idea not built yet, `deferred` for what a review left out on purpose, with its reason and the review it came from. Before changing a module, `gh issue list --label deferred --search <module or symbol>` shows what was left there to pick up when the code around it moves.
