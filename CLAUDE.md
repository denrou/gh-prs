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

- **`gh.py`** — Stateless wrapper around the `gh` CLI. Uses `gh search prs` and
  `gh pr` subcommands via `subprocess.run()`, relying on the user's existing
  `gh auth` session. Exposes a `PullRequest` dataclass plus `fetch_prs()`,
  `enrich_pr()`, and `get_current_user()`.
- **`cli.py`** — Command-line interface (argparse + [rich](https://rich.readthedocs.io/)).
  Fetches, enriches in parallel, and prints grouped/colored tables. Entry point
  is `gh_prs.cli:main`.
- **`app.py`** — Backwards-compatible shim re-exporting `cli.main` (the old
  Textual TUI was removed in 0.3.0).

### Two-phase loading

`gh search prs --json` only supports a limited set of fields (no `headRefName`,
`reviewDecision`, `statusCheckRollup`). Loading works in two phases:

1. **Fast search** — `fetch_prs(qualifiers)` runs `gh search prs` queries in
   parallel (only the qualifiers needed for the requested view).
2. **Enrichment** — `enrich_pr()` calls `gh pr view --json ...` per PR (up to 8
   concurrent workers) to fetch review decision, mergeability, and CI rollup,
   then computes each PR's `attention_reasons` (pure helper
   `_attention_reasons()`, unit-tested in `tests/test_gh.py`).

### Error handling

"Error" must never look like "nothing to do" (critical for `--count` in status
bars). All `gh` failures raise `GhError`; `fetch_prs()` raises if _any_ search
query fails (partial results would silently hide PRs). Per-PR enrichment
failures don't raise — they set `pr.enrich_error`, and the CLI prints a
warning to stderr and exits non-zero.

### Attention logic (`enrich_pr`)

A non-draft PR needs attention when any of these hold:

- **review** — your review is requested and still pending (no active approval /
  changes-requested from you), or your prior review was dismissed. Conflicting
  PRs are excluded (a review would be staled by the rebase).
- **ready** — you authored it, it's `APPROVED`, CI is green (or none), and it's
  not conflicting.
- **ci-failed** — you authored it and a check is failing.
- **conflict** — you authored it and it has merge conflicts (independent of
  `ci-failed`; a PR can have both).

## Notes

- `ruff` rule `E501` (line length) is not enforced.
- `statusCheckRollup` mixes `CheckRun` (has `status`/`conclusion`) and
  `StatusContext` (has `state`) entries — `_rollup_state()` normalizes both.
- PR titles are attacker-controlled: they are stripped of control characters
  at ingestion (`from_json`) and markup-escaped at render (`_title_cell`).
  Keep both when touching those paths.
