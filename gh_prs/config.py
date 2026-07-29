"""User-authored settings for gh-prs, kept separate from the snooze store.

``snooze.json`` is machine-managed state (the tool writes commit oids and
expiry timestamps into it); this file holds preferences a human edits by
hand. Mixing the two would let a hand-edit corrupt the snooze state, and the
two want opposite fail-safe handling, so they live in separate files.

The store is a JSON object at ``$XDG_CONFIG_HOME/gh-prs/config.json``
(``~/.config/gh-prs/config.json`` by default). A missing file means "all
defaults". Today the only setting is ``stale_after`` — the silence threshold
for the 'stale' nudge on authored PRs still awaiting review and the
'stale-draft' nudge on authored drafts:

    {"stale_after": "3d"}    # a duration; null disables both nudges entirely

Only the view path reads this file, and it degrades to defaults (with a
warning) on any error — the tool never writes it, so there is nothing to
clobber.
"""

import json
import os
from dataclasses import dataclass
from datetime import timedelta
from pathlib import Path

from gh_prs.gh import DEFAULT_STALE_AFTER
from gh_prs.snooze import SnoozeError, parse_duration


class ConfigError(Exception):
    """The config file is unreadable or holds an invalid value."""


@dataclass(slots=True)
class Config:
    # Silence threshold for the 'stale' and 'stale-draft' nudges; None
    # disables both entirely.
    stale_after: timedelta | None = DEFAULT_STALE_AFTER


def config_path() -> Path:
    config_home = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(config_home) / "gh-prs" / "config.json"


def load_config(path: Path | None = None) -> Config:
    """Return the stored settings, falling back to defaults.

    A missing file yields an all-defaults ``Config``. Anything that prevents
    a clean read or holds an invalid value raises ``ConfigError`` — the caller
    (the view path) decides to warn and use defaults rather than fail.
    """
    path = path or config_path()
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return Config()
    except UnicodeDecodeError as e:
        raise ConfigError(f"{path} is not valid UTF-8: {e}") from e
    except OSError as e:
        raise ConfigError(f"cannot read {path}: {e}") from e
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as e:
        raise ConfigError(f"{path} is not valid JSON: {e}") from e
    if not isinstance(data, dict):
        raise ConfigError(f"{path} has an unexpected shape (want a JSON object)")
    return Config(stale_after=_parse_stale_after(data, path))


def _parse_stale_after(data: dict, path: Path) -> timedelta | None:
    """Read the ``stale_after`` setting: absent → default, null → disabled."""
    if "stale_after" not in data:
        return DEFAULT_STALE_AFTER
    value = data["stale_after"]
    if value is None:
        return None
    if not isinstance(value, str):
        raise ConfigError(
            f"{path}: 'stale_after' must be a duration string like \"3d\" "
            "(or null to disable the staleness nudges)"
        )
    try:
        return parse_duration(value)
    except SnoozeError as e:
        raise ConfigError(f"{path}: invalid 'stale_after': {e}") from e
