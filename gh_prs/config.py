"""User-authored settings for gh-prs, kept separate from the snooze store.

``snooze.json`` is machine-managed state (the tool writes commit oids and
expiry timestamps into it); this file holds preferences a human edits by
hand. Mixing the two would let a hand-edit corrupt the snooze state, and the
two want opposite fail-safe handling, so they live in separate files.

The store is a JSON object at ``$XDG_CONFIG_HOME/gh-prs/config.json``
(``~/.config/gh-prs/config.json`` by default). A missing file means "all
defaults". Two settings tune the 'stale' nudge on authored PRs still awaiting
review and the 'stale-draft' nudge on authored drafts; a third lists the
review requests to silence for good (see ``mute.py``):

    {"stale_after": "3d",      # the silence threshold; null disables both nudges
     "skip_weekends": false,   # true: count working days only, not calendar days
     "mute": [                 # hide these authors' PRs from the attention view…
       {"author": "centreon-renovate", "unless_labels": ["S-Python"]}
     ]}                        # …except the ones carrying an exempting label

``skip_weekends`` stops the clock on Saturdays and Sundays, so a PR pushed on
Friday is not nudged on Monday for a weekend nobody was working, and a PR
snoozed for a day on Friday comes back on Monday. It also makes a ``w`` five
days rather than seven (``week_days`` below), which keeps ``"1w"`` a
same-weekday anniversary: a Thursday PR is nudged the following Thursday, not
the Monday after.

Only the view path reads this file, and it degrades to defaults (with a
warning) on any error — the tool never writes it, so there is nothing to
clobber.
"""

import json
import os
from dataclasses import dataclass
from pathlib import Path

from gh_prs.duration import Duration
from gh_prs.gh import DEFAULT_STALE_AFTER
from gh_prs.mute import MuteRule
from gh_prs.snooze import CALENDAR_WEEK_DAYS, SnoozeError, parse_duration


# Days a 'w' stands for once weekends stop counting. Five, not seven, so the
# unit keeps naming the same weekday a week later — the way a human reads
# "give it a week" — instead of silently meaning nine calendar days.
WORKING_WEEK_DAYS = 5


class ConfigError(Exception):
    """The config file is unreadable or holds an invalid value."""


@dataclass(slots=True)
class Config:
    # Silence threshold for the 'stale' and 'stale-draft' nudges; None
    # disables both entirely.
    stale_after: Duration | None = DEFAULT_STALE_AFTER
    # Skip Saturdays and Sundays (in the machine's local timezone) when
    # counting that threshold and snooze windows.
    skip_weekends: bool = False
    # Review requests to hide from the attention view, in file order. Empty
    # by default: nothing is ever muted unless the user asked for it.
    mute: tuple[MuteRule, ...] = ()


def week_days(skip_weekends: bool) -> int:
    """Days a 'w' stands for under the given weekend policy.

    Shared by the config file, the ``--stale-after`` flag and ``snooze
    --for`` so all three read a duration the same way.
    """
    return WORKING_WEEK_DAYS if skip_weekends else CALENDAR_WEEK_DAYS


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
    # skip_weekends first: it decides how long a 'w' is in stale_after.
    skip_weekends = _parse_skip_weekends(data, path)
    return Config(
        stale_after=_parse_stale_after(data, path, skip_weekends),
        skip_weekends=skip_weekends,
        mute=_parse_mute(data, path),
    )


def _parse_skip_weekends(data: dict, path: Path) -> bool:
    """Read the ``skip_weekends`` setting: absent → False (calendar days)."""
    if "skip_weekends" not in data:
        return False
    value = data["skip_weekends"]
    # A bare isinstance check would accept 0/1 (bool subclasses int); a
    # number here means the user meant something this setting cannot express.
    if not isinstance(value, bool):
        raise ConfigError(f"{path}: 'skip_weekends' must be true or false")
    return value


def _parse_stale_after(data: dict, path: Path, skip_weekends: bool) -> Duration | None:
    """Read the ``stale_after`` setting: absent → default, null → disabled.

    ``skip_weekends`` only sets how long a ``w`` is (see ``week_days``); the
    default is a plain 3 days either way.
    """
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
        return parse_duration(value, week_days=week_days(skip_weekends))
    except SnoozeError as e:
        raise ConfigError(f"{path}: invalid 'stale_after': {e}") from e


# The keys a mute rule may carry. Anything else is rejected rather than
# ignored: a misspelt "unless_label" would otherwise silently turn an
# exempting rule into an unconditional one.
_MUTE_RULE_KEYS = frozenset({"author", "unless_labels"})


def _parse_mute(data: dict, path: Path) -> tuple[MuteRule, ...]:
    """Read the ``mute`` list: absent → no rules.

    Each rule is an object with a non-empty ``author`` login and an optional
    ``unless_labels`` list of non-empty label names. The whole list is
    validated before any rule is kept, so a bad entry disables muting
    entirely (the caller warns and falls back to defaults) instead of
    applying the rules around it — a partial rule set would hide a
    different set of PRs than the one the user wrote down.
    """
    if "mute" not in data:
        return ()
    rules = data["mute"]
    if not isinstance(rules, list):
        raise ConfigError(f"{path}: 'mute' must be a list of rules")
    parsed: list[MuteRule] = []
    for index, rule in enumerate(rules):
        where = f"{path}: 'mute[{index}]'"
        if not isinstance(rule, dict):
            raise ConfigError(f'{where} must be an object like {{"author": ...}}')
        unknown = sorted(set(rule) - _MUTE_RULE_KEYS)
        if unknown:
            raise ConfigError(f"{where} has unknown key(s): {', '.join(unknown)}")
        author = rule.get("author")
        if not isinstance(author, str) or not author.strip():
            raise ConfigError(f"{where} needs a non-empty 'author' login")
        labels = rule.get("unless_labels", [])
        if not isinstance(labels, list) or not all(
            isinstance(label, str) and label.strip() for label in labels
        ):
            raise ConfigError(
                f"{where}: 'unless_labels' must be a list of non-empty label names"
            )
        parsed.append(MuteRule(author=author.strip(), unless_labels=frozenset(labels)))
    return tuple(parsed)
