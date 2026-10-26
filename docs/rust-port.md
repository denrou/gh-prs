# Rust port: technical design

gh-prs is about 3,000 lines of Python and 4,500 lines of tests (490 tests),
and its behaviour is stable. This document plans a rewrite in Rust that
behaves the same. It is a working document: once the switch is done, its
lasting parts move into the Architecture section of `CLAUDE.md` and the file
is deleted.

Decided: full rewrite, same repository, PyPI dropped, cargo-dist for
releases, strict parsing of GitHub responses, and the old snooze-flag
migration hints dropped.

## Why

- **Correctness rules become types.** The hard part of this codebase is the
  fail-safe rules: an unknown value must never read as "nothing to do".
  Today only tests and comments keep them in place, and nothing checks the
  types (no mypy, pyright or ty). In Rust, `UNKNOWN` mergeability, a missing
  oid or an incomplete label list are enum variants that every `match` has
  to handle.
- **Footprint.** The Homebrew formula pulls in `python@3.14` (83 MB),
  `openssl@4` (25 MB) and about six more formulae, on top of the package's
  own 8.2 MB. A static binary should be a few MB with no runtime
  dependency, and Homebrew bumping Python no longer forces a rebuild.
- **Faster feedback for agents.** A compile error points at the exact
  broken spot, where today a mistake only shows up if a test happens to
  cover it.

Speed is not a goal: network calls dominate every command.

## What must not change

The two versions share the same files during the shadow run (see below), and
users switch versions without migrating anything. So these stay fixed:

- **The CLI.** Same commands, flags and exit codes (`0` success, `1` failure
  or partial batch, `2` usage error). See the divergences table for `130`.
- **The files.** `config.json`, `snooze.json` and `hidden.json`: same paths
  (`$XDG_CONFIG_HOME/gh-prs/`, defaulting to `~/.config`), same shapes, same
  validation. Stores are still written with sorted keys and a 2-space indent
  through write-then-rename, so a file written by Python and rewritten by
  Rust shows no diff. Timestamps are written the way Python's
  `isoformat(timespec="seconds")` writes them (`2027-09-03T12:30:37+00:00`).
- **`--json`.** Same field names, same order, same value vocabulary (`""`
  for a missing value, normalized `checksState`, sorted `roles` and
  `attentionReasons`).
- **`gh` stays the only network path.** No HTTP client and no token
  handling: authentication, Enterprise hosts and proxies stay `gh`'s job,
  exactly as today. Every query keeps going through `gh api graphql`, with
  the same queries, the same variables and the same batching.
- **The behaviour described in `CLAUDE.md`.** That description is the
  functional spec, and it isn't rewritten here.

## Crates

| Need                          | Crate                             | Why this one                                                                                                |
| ----------------------------- | --------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| Argument parsing              | `clap` (derive)                   | Covers mutually exclusive flags, a global `--no-color` and aliases; exits with `2` on misuse, like argparse |
| JSON                          | `serde`, `serde_json`             | Responses and stores alike                                                                                  |
| Dates and time zones          | `jiff`                            | Local midnight across DST changes (`start_of_day`), civil dates, honours `TZ`                               |
| Colour and terminal detection | `anstream`, `anstyle`             | Already pulled in by clap; strip colour when not a terminal, honour `NO_COLOR`                              |
| Table layout                  | `unicode-width`, `terminal_size`  | The table is drawn by hand (see Rendering)                                                                  |
| Spinner                       | `indicatif`                       | On stderr, hidden automatically when stderr is not a terminal                                               |
| Subprocess timeout            | `wait-timeout`                    | `std::process` has no timeout, and the 60 s one stays                                                       |
| Tests                         | `libtest-mimic`, `tempfile` (dev) | Data-driven conformance suite                                                                               |

Deliberately left out:

- **tokio, or any async runtime.** At most about 20 requests per run, capped
  at 8 in parallel; scoped threads are enough.
- **reqwest or octocrab.** `gh` stays the network path.
- **regex.** Two patterns (PR URL, duration) that are easier to read as
  plain parsing code. The PR URL pattern needs a lookahead, which the
  `regex` crate doesn't support anyway.
- **anyhow and thiserror.** Errors are message carriers (see Errors), and
  plain types are enough.
- **A rich equivalent.** See Rendering.

## Layout

A single binary crate. `Cargo.toml` sits at the root next to `pyproject.toml`
until the switch, and the two toolchains don't see each other's files.

```
src/
  main.rs        argv → cli::run → exit code
  pr.rs          PullRequest and the enums of the type model
  attention.rs   attention reasons, stale and stale-draft (pure)  ← from gh.py
  duration.rs    Duration, parse_duration, add_days               ← duration.py + parse_duration
  config.rs      Config, week_days, mute rule parsing
  mute.rs        is_muted / split_muted
  merge.rs       merge_blockers / should_approve
  store.rs       XDG paths, read and write JSON
  snooze.rs      Snooze entries, split_snoozed, normalize_pr_url
  hide.rs        Hide entries, split_hidden
  gh/
    mod.rs       the Runner trait, the real runner, the response envelope, GhError
    queries.rs   GraphQL text (_PR_FRAGMENT and the queries)
    parse.rs     raw serde types → PullRequest
    fetch.rs     search, hydrate, gather, count, fetch_pr, approve, merge
  cli/
    mod.rs       dispatch, settings, warnings
    args.rs      clap definitions
    render.rs    tables, the JSON output
    view.rs      the attention and list views (mute → hide → snooze)
    snooze.rs    snooze / unsnooze / prune
    hide.rs      hide / unhide / prune
    merge.rs     the two-phase merge
tests/conformance/
  main.rs        harness = false; also acts as the fake `gh`
  scenarios/     one directory per scenario
```

Two changes from the Python layout:

- The attention logic leaves `gh.py`. It is pure policy and the biggest
  block of unit tests.
- `parse_duration` moves into `duration.rs`. Today it lives in `snooze.py`
  only for historical reasons, which forces `config.py` to import the snooze
  store.

## Type model

This is the core of the port. Every fail-safe rule in `CLAUDE.md` is about
unknown values, so the rule is:

> An unknown is a value, not a default. It is collapsed into a known value
> while parsing only when every consumer agrees on which way to collapse it.
> Otherwise it is carried to the policy code, and the `match` there decides.

Concretely:

- **Collapsed at parse time, because every consumer agrees:**
  - `checks`: an unrecognised or missing rollup becomes `Pending`.
  - `stacked`: an unknown count becomes `true`.
  - `unresolved_feedback`: there is only one consumer.
- **Carried to the policy code:**
  - `mergeable`: `ready` wants positive evidence, while `review` keeps
    `UNKNOWN` visible.
  - `review_decision`: see the merge example below.
  - `labels`: only the mute rules can say what an incomplete list means.
  - The standing changes-requested reviews: only the stale nudge reads them,
    and it inverts the usual fail direction.

```rust
/// A GitHub enum value as received: a variant this version knows, a value
/// it doesn't (kept verbatim for --json), or null/absent.
pub enum Reported<T> { Known(T), Unrecognised(String), Missing }

pub enum PrState { Open, Closed, Merged }
pub enum Mergeable { Mergeable, Conflicting, Unknown } // Unknown: GitHub's own UNKNOWN
pub enum ReviewDecision { Approved, ChangesRequested, ReviewRequired }
pub enum ReviewState { Approved, ChangesRequested, Commented, Dismissed, Pending }
pub enum Checks { NoneConfigured, Success, Failure, Pending }

pub struct Oid(String); // never empty: an empty oid from GitHub parses as None

pub struct MyReview { pub state: Reported<ReviewState>, pub commit: Option<Oid> }

/// Standing CHANGES_REQUESTED reviews. A `None` entry is a review against an
/// unknown commit; `truncated` means the 50-node cap may hide more.
pub struct ChangesRequested { pub commits: Vec<Option<Oid>>, pub truncated: bool }

/// Labels are evidence for a mute exemption only when the list is complete.
pub enum Labels { Complete(Vec<String>), Partial(Vec<String>) }

pub struct PullRequest {
    pub number: u64,
    pub repo: String,           // nameWithOwner, required
    pub title: String,          // control characters stripped at ingestion
    pub author: Option<String>,
    pub url: String,
    pub state: Reported<PrState>,
    pub updated_at: String,     // raw ISO text: sorting, display, --json
    pub created_at: String,
    pub is_draft: bool,
    pub review_decision: Reported<ReviewDecision>,
    pub mergeable: Reported<Mergeable>,
    pub checks: Checks,
    pub my_review: Option<MyReview>,
    pub head: Option<Oid>,
    pub review_requested_explicitly: bool,
    pub has_pending_review_request: bool,
    pub changes_requested: ChangesRequested,
    pub unresolved_feedback: bool,
    pub review_threads_truncated: bool,
    pub stacked: bool,
    pub labels: Labels,
    pub roles: BTreeSet<Role>,
    pub attention: BTreeSet<Reason>,
}
```

`Role` and `Reason` are enums whose variants are declared in the
alphabetical order of their names (`ci-failed`, `conflict`, `new-commits`,
…), so the derived `Ord` of a `BTreeSet` is exactly what Python's
`sorted()` produces for the store and `--json`. A test pins that order. The
display order of the sections is a separate constant, as it is today.

**What the model already catches.** Writing `merge_blockers` as an
exhaustive match on `Reported<ReviewDecision>` forces a decision for
`Unrecognised`. Python lets it through: only `CHANGES_REQUESTED` and
`REVIEW_REQUIRED` block, so a review decision GitHub adds in the future
would not block a merge. The port blocks it, which is the merge path's own
rule ("never merge on uncertain data"). `Missing` keeps passing, because a
null `reviewDecision` means no review policy applies.

**Stores.** The snooze invariant ("an open-ended snooze carries reasons")
becomes a type instead of a check in two places:

```rust
pub enum Snooze {
    OpenEnded { oid: String, reasons: Vec<String> },
    Windowed { oid: String, until: String, reasons: Option<Vec<String>> },
}
```

It goes through serde's `try_from` and `into` a raw struct. In that struct
`until` must be present (`null` or a string; a missing key is corruption),
and an open-ended entry without reasons fails to load. `until` stays raw
text because an unparseable value means "expired, show the PR", not "corrupt
store". Stored reasons stay strings, not `Reason`, for the same reason: a
reason name this version doesn't know simply doesn't match, so the snooze
lapses and the PR shows.

## Parsing GitHub responses

Parsing has two layers. First, serde deserialises into raw structs that
mirror the GraphQL shape. Every nullable or omittable field there is an
`Option` with `#[serde(default)]`, and every connection's `nodes` is an
`Option<Vec<Option<Node>>>`, so a null list and null nodes both survive as
unknowns. Then a pure function
turns a raw node into a `PullRequest`, applying the rules `from_graphql`
applies today. The unit tests target that function. Its inputs are the same
JSON nodes `test_gh.py` builds, so those fixtures port as they are.

A field that arrives with the wrong JSON type (say `totalCount: "3"`) is a
serde error, so the whole fetch fails with `GhError`. This is deliberate,
and stricter than Python, which reads it as unknown in a few places.
GitHub's schema is typed, so a type change means the contract with `gh` has
changed. We want to hear about it and act, not have the tool quietly keep
running on assumptions that no longer hold. It also keeps the code plain
serde. The existing parse tests feed nulls and missing blocks, not wrong
types, so almost none of them change.

## Seams for tests

- **`gh`.** A `Runner` trait with a single method: run `gh` with these
  arguments and return the exit status, stdout and stderr. The real one
  spawns the process. In unit tests a closure returns canned output. This
  is the seam the Python tests already use (`_run_gh`), so the envelope and
  fetch tests port one to one.
- **Clock and time zone.** Functions take `now: Timestamp` and
  `tz: &TimeZone` as parameters, and only `main` reads the system clock and
  `TimeZone::system()`. Python tests get Paris DST behaviour by patching;
  here they pass `TimeZone::get("Europe/Paris")`. A bundled tz database
  (jiff's `tzdb-bundle-always`, as a dev-dependency feature) keeps them
  independent of the machine.
- **Config home.** Paths are built from `XDG_CONFIG_HOME` and `HOME`, which
  the conformance suite points at a temporary directory.

## Errors

One plain error type per domain: `GhError`, `SnoozeError`, `HideError`,
`ConfigError`, each carrying a message, as in Python. They stay separate on
purpose. Whether an error degrades (the view warns and shows more) or is
fatal (a subcommand must not overwrite a store it couldn't read) depends on
which store failed and where, and separate types make every call site spell
that out.

There are no panics on input. `clippy::unwrap_used` and `expect_used` are
denied outside tests, the Rust equivalent of "never a raw traceback".

## Concurrency and subprocesses

`_gather` becomes a function over `std::thread::scope`:

- 8 workers pull labelled tasks from a shared index.
- Every error is collected, and any failure fails the whole gather, with
  the same sorted message as today.
- Results come back in submission order, so the hydrated PRs keep Python's
  order, which makes the stable sort on `updated_at` produce identical
  ties.

The runner reads stdout and stderr on two threads and uses `wait-timeout`
for the 60 s limit, killing `gh` when it runs out.

## Rendering

The tables are drawn by hand, with no layout library. They have no headers
and no borders, the columns are left-aligned, and one column (the title)
shrinks with an ellipsis. That is about 100 lines, using `unicode-width` for
display width and `terminal_size` (or `COLUMNS`, falling back to 80) for the
available width.

It also removes a whole class of bug. rich interprets markup in strings,
which is why PR titles are escaped today. The Rust output builds styled
segments, so a title can never be read as markup in the first place. The
control-character stripping at ingestion stays: it protects the terminal,
not the markup.

When stdout is a terminal, PR numbers become terminal hyperlinks (OSC 8);
otherwise they are plain text. `anstream` strips colours when not writing to
a terminal and honours `NO_COLOR`, and `--no-color` forces that.

## Guardrails

- `#![deny(clippy::wildcard_enum_match_arm)]`. A `_ =>` arm on our own
  enums would quietly cancel the exhaustiveness the type model relies on.
  It also fires on other crates' enums (`serde_json::Value`, seen in the
  harness), where it's usually simpler to use the accessor methods
  (`as_str()`, `is_null()`) than to list every variant.
- `unwrap_used` and `expect_used` denied outside tests (see Errors).
- A `ci.yml` workflow on every push to `main`: `cargo fmt --check`,
  `cargo clippy -- -D warnings`, `cargo test`. The repository runs no tests
  in CI today. That was acceptable for one person running pytest locally;
  it isn't once agents commit straight to `main`.

## Testing

**Unit tests** live next to the code (`#[cfg(test)]`). For each module, the
Python tests are translated first and the code is written until they pass.
The pure modules (`duration`, `config`, `mute`, `merge`, `snooze`, `hide`,
attention) and the GraphQL parsing and envelope tests port almost line for
line.

**The conformance suite** is the new piece. `test_cli.py` tests through
Python internals (136 `monkeypatch` calls on `cli` and `gh`), so it can't
check a Rust binary. It is replaced by black-box scenarios run against
whichever binary `GH_PRS_BIN` points at:

- Each scenario is a directory. `scenario.json` holds the argv, the
  environment (`TZ`, `COLUMNS`), the viewer login, the search results per
  qualifier, the GraphQL nodes by id, the PR lookups and `pr view` answers,
  the injected failures, and the initial `config.json`, `snooze.json` and
  `hidden.json`. `expected/` holds the exit code, stdout, stderr, the
  stores afterwards, and a log of the write calls `gh` received.
- **The fake `gh` is the test binary itself.** Before running, the harness
  symlinks its own executable as `gh` into a temporary `PATH` directory.
  When invoked under that name, it answers from the scenario instead of
  running tests. That avoids a second crate and a shell script, and since
  the fake answers by meaning (a search for this qualifier, these ids, this
  URL) rather than by exact argument bytes, it doesn't depend on batching
  or ordering.
- **Expectations come from Python.** `BLESS=1` writes `expected/` from
  whatever binary runs. Scenario inputs are written once, blessed against
  `.venv/bin/gh-prs`, reviewed by hand, and committed. Rust then has to
  match them. That makes "same behaviour" true by construction, not by
  reading code side by side.
- **Comparison rules.** Exact for exit codes, the stores (as parsed JSON),
  `--json` and `--count`, and the write-call log. That log matters most for
  `merge`: it records the exact `--match-head-commit` oid, and that nothing
  was called when the preflight aborted. Stdout and stderr are compared
  with runs of whitespace collapsed, because column spacing is not part of
  the contract. Read-only `gh` calls are not compared.
- **No clock injection.** Python has none, and adding a test-only hook
  just for this isn't worth it. Scenarios use dates far in the past (2001:
  always stale, window elapsed) or far in the future (2099: fresh, window
  open). Anything that depends on the exact current time stays a unit test
  with `now` passed in.

Expect about 60 to 80 scenarios: every subcommand, every view, every
"degrades vs fatal" path, every usage error, and the merge abort and
stop-at-first-failure paths.

## Port order

Each step is one or more commits on `main`. The Python version keeps
shipping until the switch.

1. **Scaffolding.** `Cargo.toml`, lints, `ci.yml`, the conformance harness
   with the fake `gh`, and the first ten scenarios blessed from Python.
2. **`duration`**, then **`pr` and `attention`.** This is the largest share
   of `test_gh.py`, and it settles the type model early, while it is still
   cheap to change.
3. **`config`, `mute`, `merge`.**
4. **`store`, `snooze`, `hide`.**
5. **`gh`:** runner, envelope, parsing, fetch.
6. **`cli`.** The conformance suite goes green against both binaries.
7. **Shadow run** (below).
8. **Switch.** Release pipeline, delete Python, update `README.md` and
   `CLAUDE.md`, delete this document.

Features are frozen from step 1 to step 8. A bug fix during that window
lands in Python with a conformance scenario, so it can't be lost in the
port.

## Shadow run

For about a week of normal use, run both binaries back to back and compare
their outputs:

```sh
for flags in "" -c -r -a; do
  diff <(gh-prs $flags --json | jq -S .) <(target/release/gh-prs $flags --json | jq -S .)
done
diff <(gh-prs --count) <(target/release/gh-prs --count)
```

Every diff must be explained: either a PR changed between the two calls, or
the difference is in the divergences table. Snooze, hide and merge stay out
of the shadow run because they write; the conformance suite covers them.

## Release

PyPI goes, along with `python-publish.yml` and the PyPI-to-tap race. The
released versions stay on PyPI. The README drops the `uv tool install`
section and the status-bar note about it, and points to Homebrew or the
binaries on the GitHub release. Targets: `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`.

Releases go through dist (cargo-dist). It generates the release workflow,
builds the four targets, attaches them to the GitHub release and pushes a
binary formula to `denrou/homebrew-gh-prs`, which replaces
`update-formula.yml`. It needs a token for the tap, stored as a secret.

One thing to check on the first release: dist triggers on a tag push, and
today's release step, `gh release create vX.Y.Z --target main`, creates the
tag through the API. If that doesn't fire the workflow, the release step
becomes `git tag vX.Y.Z` followed by `! git push origin vX.Y.Z`.

After upgrading, `brew autoremove` drops the now-orphaned `python@3.14`.

## Deliberate divergences

| Area                                     | Python                                            | Rust                                                | Why                                                                              |
| ---------------------------------------- | ------------------------------------------------- | --------------------------------------------------- | -------------------------------------------------------------------------------- |
| Unrecognised `reviewDecision` on merge   | passes                                            | blocks                                              | Merge rule: never on uncertain data (see Type model)                             |
| A GraphQL field with the wrong JSON type | sometimes read as unknown                         | `GhError`                                           | Contract changes with `gh` surface (see Parsing)                                 |
| Ctrl-C                                   | prints "Interrupted.", exits `130`                | default SIGINT: the shell reports `130`, no message | No signal-handling crate. Stores are safe either way (write-then-rename)         |
| `--json` text                            | non-ASCII as `\u` escapes; coloured on a terminal | UTF-8 as is; never coloured                         | Parsed JSON is identical; colouring a scripting interface was a rich side effect |
| `--snooze`/`--unsnooze`/`--snoozed`      | migration hint, exit `2`                          | clap's unexpected-argument error, exit `2`          | The subcommands replaced them long enough ago                                    |
| Table spacing and `--help` layout        | rich and argparse                                 | hand-drawn and clap                                 | Not part of the contract                                                         |
