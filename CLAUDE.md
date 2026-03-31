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

- **`gh.py`** — Stateless wrapper around the `gh` CLI. Uses `gh search prs` and `gh pr` subcommands via `subprocess.run()`, relying on the user's existing `gh auth` session. Exposes a `PullRequest` dataclass and functions for fetch/approve/merge/open.
- **`app.py`** — Textual TUI. `PullRequestsApp` owns all state (`_prs`, `_filtered`, `_selected`, `_filter_text`). API calls run in background threads via `@work(thread=True)` with UI updates through `call_from_thread()`. Modal screens handle filtering (`FilterInput`), detail view (`DetailScreen`), and merge confirmation (`ConfirmScreen`).

Entry point: `gh_prs.app:main`.

## Notes

- Browser opening uses macOS `open` command (not cross-platform yet).
- Merge uses squash strategy and deletes the source branch.
- `ruff` rule `E501` (line length) is not enforced.
