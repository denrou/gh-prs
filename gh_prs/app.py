"""Backwards-compatible entry point.

The interactive Textual TUI was replaced by a plain command-line interface.
This module simply re-exports the CLI entry point so existing references to
``gh_prs.app:main`` keep working.
"""

from gh_prs.cli import main

__all__ = ["main"]


if __name__ == "__main__":
    raise SystemExit(main())
