# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
uv run gh-prs               # Run the CLI (default: PRs needing attention)
uv run gh-prs -c            # PRs you created
uv run gh-prs -r            # PRs awaiting your review
uv run pytest               # Run tests
uv run ruff check .         # Lint
uv run ruff format .        # Format
uv add <pkg>                # Add dependency
uv add --dev <pkg>          # Add dev dependency
```

## Architecture

Two-module design inside `gh_prs/`:

- **`gh.py`** — Stateless wrapper around the `gh` CLI, relying on the user's
  existing `gh auth` session. Exposes a `PullRequest` dataclass plus
  `fetch_prs()`, `ALL_QUALIFIERS`, and the `GhError` exception.
- **`cli.py`** — Command-line interface (argparse + [rich](https://rich.readthedocs.io/)).
  Fetches and prints grouped/colored tables. Entry point is `gh_prs.cli:main`.
- **`app.py`** — Backwards-compatible shim re-exporting `cli.main` (the old
  Textual TUI was removed in 0.3.0).

### Loading (single GraphQL round-trip per qualifier)

`fetch_prs(qualifiers)` runs one `gh api graphql` search per qualifier
(`author`, `review-requested`, `assignee`, `involves`) in parallel threads.
Each search fetches everything in one shot — review decision, mergeability,
CI rollup state, `latestReviews`, `reviewRequests`, plus the viewer's login —
so there is no per-PR enrichment phase. `attention_reasons` is computed by the
pure `_attention_reasons()` helper (unit-tested in `tests/test_gh.py`).

Performance notes (measured):

- GitHub executes aliased search blocks _sequentially_ within one GraphQL
  request — that's why each qualifier gets its own parallel request (cost =
  slowest search, ~2 s for `author:@me`, not the sum).
- GitHub also throttles concurrent searches per token; `-a` is bounded by the
  inherently slow `involves:@me` search (~4 s).
- Each search is capped at `_SEARCH_LIMIT` (100) nodes — larger result sets
  are silently truncated.

### Error handling

"Error" must never look like "nothing to do" (critical for `--count` in status
bars). All `gh` failures raise `GhError` — including per-qualifier search
failures (partial results would silently hide PRs) and subprocess timeouts
(60 s). The CLI prints the error to stderr and exits non-zero.

### Attention logic (`_attention_reasons`)

A non-draft PR needs attention when any of these hold:

- **review** — your review is requested (or your prior review was dismissed)
  and you have no active approval / changes-requested. Hidden when: the PR is
  conflicting (a review would be staled by the rebase); the overall decision
  is `CHANGES_REQUESTED` (author is reworking it); or it's `APPROVED` —
  mergeable without you — unless you are personally on the
  requested-reviewers list (`review_requested_explicitly`, i.e. requested as
  a User, not through a Team).
- **ready** — you authored it, it's `APPROVED`, CI is green (or none), and it's
  not conflicting.
- **ci-failed** — you authored it and a check is failing.
- **conflict** — you authored it and it has merge conflicts (independent of
  `ci-failed`; a PR can have both).

## Notes

- `ruff` rule `E501` (line length) is not enforced.
- GraphQL `statusCheckRollup.state` is normalized via `_ROLLUP_STATE`; unknown
  future states map to `PENDING` so "unrecognized" never counts as passing.
- PR titles are attacker-controlled: they are stripped of control characters
  at ingestion (`from_graphql`) and markup-escaped at render (`_title_cell`).
  Keep both when touching those paths.
