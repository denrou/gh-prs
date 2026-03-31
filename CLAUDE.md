# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
uv run gh-prs               # Run the app
uv run ruff check .         # Lint
uv run ruff format .        # Format
uv add <pkg>                # Add dependency
uv add --dev <pkg>          # Add dev dependency
```

## Architecture

Two-module design inside `gh_prs/`:

- **`gh.py`** — Stateless wrapper around the `gh` CLI. Uses `gh search prs` and `gh pr` subcommands via `subprocess.run()`, relying on the user's existing `gh auth` session. Exposes a `PullRequest` dataclass and functions for fetch/approve/merge/open/enrich.
- **`app.py`** — Textual TUI. `PullRequestsApp` owns all state (`_prs`, `_filtered`, `_selected`, `_filter_text`). API calls run in background threads via `@work(thread=True)` with UI updates through `call_from_thread()`. Modal screens handle filtering (`FilterInput`), detail view (`DetailScreen`), and merge confirmation (`ConfirmScreen`).

Entry point: `gh_prs.app:main`.

### Two-phase loading

`gh search prs --json` only supports a limited set of fields (no `headRefName`, no `reviewDecision`). Loading works in two phases:

1. **Fast search** — `fetch_prs()` runs two `gh search prs` queries in parallel (review-requested + assignee) and renders the table immediately.
2. **Background enrichment** — `_enrich_prs()` calls `gh pr view --json headRefName,reviewDecision` per PR (4 concurrent workers), re-rendering after each completion.

## Notes

- Browser opening uses macOS `open` command (not cross-platform yet).
- Merge uses squash strategy and deletes the source branch.
- Textual `DataTable` row keys are `RowKey` objects — use `row_key.value` (not `str(row_key)`) to get the actual key string.
- `ruff` rule `E501` (line length) is not enforced.
