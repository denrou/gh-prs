"""JSON file I/O shared by the machine-managed stores (snoozes, hidden PRs).

Both stores are small JSON objects under ``$XDG_CONFIG_HOME/gh-prs/``. The
read side turns every way a file can fail to load — unreadable, not UTF-8,
not JSON — into the caller's own error type, so each store module keeps a
single exception its callers already handle; the write side goes through a
temporary file and an atomic rename so a crash mid-write can never leave a
truncated store behind (which would then read as corrupt). Shape validation
stays with each store: only it knows what an entry must look like.
"""

import json
import os
from pathlib import Path


def store_path(name: str) -> Path:
    """``$XDG_CONFIG_HOME/gh-prs/<name>`` (``~/.config`` by default)."""
    config_home = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(config_home) / "gh-prs" / name


def read_json(path: Path, error: type[Exception]) -> object | None:
    """Parse ``path`` as JSON; ``None`` when the file does not exist.

    Any other failure raises ``error`` with a message naming the file.
    """
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except UnicodeDecodeError as e:
        # Not an OSError: without this clause a corrupt (e.g. truncated)
        # file would crash the caller instead of degrading.
        raise error(f"{path} is not valid UTF-8: {e}") from e
    except OSError as e:
        raise error(f"cannot read {path}: {e}") from e
    try:
        return json.loads(raw)
    except json.JSONDecodeError as e:
        raise error(f"{path} is not valid JSON: {e}") from e


def write_json(path: Path, data: object, error: type[Exception]) -> None:
    """Write ``data`` to ``path`` as JSON, creating its directory if needed.

    Raises ``error`` on any I/O failure.
    """
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # Write-then-rename so a crash mid-write can't leave a truncated
        # store (which would then read as corrupt).
        tmp = path.with_name(path.name + ".tmp")
        tmp.write_text(
            json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.replace(tmp, path)
    except OSError as e:
        raise error(f"cannot write {path}: {e}") from e
