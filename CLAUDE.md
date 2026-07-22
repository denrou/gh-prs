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

Three-module design inside `gh_prs/`:

- **`gh.py`** — Stateless wrapper around the `gh` CLI, relying on the user's
  existing `gh auth` session. Exposes a `PullRequest` dataclass plus
  `fetch_prs()`, `count_prs()`, `fetch_pr_head()`, `ALL_QUALIFIERS`, and the
  `GhError` exception.
- **`snooze.py`** — Local per-PR snooze store (`{PR url: head oid}` JSON at
  `$XDG_CONFIG_HOME/gh-prs/snooze.json`). Pure I/O + partitioning helpers;
  no `gh` calls. Raises `SnoozeError`.
- **`cli.py`** — Command-line interface (argparse + [rich](https://rich.readthedocs.io/)).
  Fetches and prints grouped/colored tables. Entry point is `gh_prs.cli:main`.

### Loading (single GraphQL round-trip per qualifier)

`fetch_prs(qualifiers)` runs one `gh api graphql` search per qualifier
(`author`, `review-requested`, `reviewed-by`, `assignee`, `involves`) in
parallel threads.
Each search fetches everything in one shot — review decision, mergeability,
CI rollup state, `latestReviews`, `reviewRequests`, plus the viewer's login —
so there is no per-PR enrichment phase. `attention_reasons` is computed by the
pure `_attention_reasons()` helper (unit-tested in `tests/test_gh.py`).

Performance notes (measured once; exact figures drift, the ratios hold):

- GitHub executes aliased search blocks _sequentially_ within one GraphQL
  request — that's why each qualifier gets its own parallel request (cost =
  slowest search, not the sum).
- GitHub also throttles concurrent searches per token; `-a` is bounded by
  `involves:@me`, the slowest search by far.
- Node _hydration_ dominates search cost, not the search itself — a
  count-only `issueCount` query is roughly an order of magnitude faster than
  a hydrated one. `count_prs()` exploits this for single-qualifier `--count`
  (`-c`/`-r`), the status-bar polling path.
- Each search is capped at `_SEARCH_LIMIT` (100) nodes; searches are
  `sort:updated-desc`, so truncation keeps the most recently updated PRs, and
  when `issueCount` exceeds the cap `fetch_prs()` reports the truncation
  through its `on_warning` callback (the CLI prints it to stderr). Counts
  from `count_prs()` are exact regardless of the cap.

### Error handling

"Error" must never look like "nothing to do" (critical for `--count` in status
bars). All `gh` failures raise `GhError` — including per-qualifier search
failures (partial results would silently hide PRs), subprocess timeouts
(60 s), and any deviation from the expected GraphQL response envelope
(validated in `_graphql()`/`_search()`). The same fail-safe direction applies
to per-PR fields: unknown CI states map to `PENDING`, and "ready" requires a
positive `MERGEABLE` (GitHub reports `UNKNOWN` while recomputing
mergeability). The CLI prints errors to stderr and exits non-zero (130 on
Ctrl-C).

### Attention logic (`_attention_reasons`)

A non-draft PR needs attention when any of these hold:

- **review** — your review is requested (or your prior review was dismissed)
  and you have no active approval / changes-requested. Hidden when: the PR is
  conflicting (a review would be staled by the rebase); the overall decision
  is `CHANGES_REQUESTED` (author is reworking it); or it's `APPROVED` —
  mergeable without you — unless you are personally on the
  requested-reviewers list (`review_requested_explicitly`, i.e. requested as
  a User, not through a Team).
- **new-commits** — you reviewed someone else's PR (`APPROVED`,
  `CHANGES_REQUESTED`, `COMMENTED`, or `DISMISSED` — the latter for repos
  that auto-dismiss stale reviews on push) and the head oid no longer
  matches the oid your review was submitted against (`latestReviews.commit`
  vs `headRefOid`) — new commits or a rebase the author forgot to re-request
  review for. Commit identity is compared, not `committedDate`: committer
  timestamps are mutable metadata. A missing oid on either side counts as
  "moved" (unknown must never read as "nothing to do"); only both-missing
  stays quiet. Hidden when: the PR is conflicting (more commits are coming);
  the **review** reason already fired (no double listing); or you authored
  the PR (a comment review on your own PR must not self-flag). Surfaced by
  the `reviewed-by:@me` search in the default view — review requests
  disappear once fulfilled, so these PRs match no other attention qualifier.
  When the `latestReviews` 50-node cap hides your review on a `reviewed-by`
  PR, `fetch_prs` reports the contradiction through `on_warning` instead of
  silently skipping the PR.
- **ready** — you authored it, it's `APPROVED`, CI is green (or none), and it's
  not conflicting.
- **ci-failed** — you authored it and a check is failing.
- **conflict** — you authored it and it has merge conflicts (independent of
  `ci-failed`; a PR can have both).

### Snoozing (`snooze.py`, applied in `cli.py`)

`--snooze <pr>` (full URL or github.com shorthand `owner/repo/123` /
`owner/repo#123`) records the PR's head oid plus an expiry timestamp
(default 24h, `--for 12h/3d/1w`); the default attention view (table and
`--count`) then hides the PR while _both_ hold: head unchanged and window
open. The same fail-safe direction as everywhere else applies: an unknown
oid, an uncomparable timestamp, a moved head, an elapsed window, or an
unreadable store all _show_ the PR (a corrupt store only warns on the view
path, but is fatal for `--snooze`/`--unsnooze`, which must not clobber the
file). Dead entries are pruned — with an on-stderr "snooze expired" warning
when the PR actually resurfaced — and the view reports how many
attention-worthy PRs it withheld. Explicit views (`-c`/`-r`/`-a`), fast
counts, and `--json` never consult the store — their output stays exact.
Entries whose PR no longer appears in any search are kept while their window
is open (the PR may be closed _or_ merely beyond the 100-node cap; deleting
on absence would lose live snoozes) and pruned quietly once it elapses.

## Notes

- `ruff` rule `E501` (line length) is not enforced.
- GraphQL `statusCheckRollup.state` is normalized via `_ROLLUP_STATE`; unknown
  future states map to `PENDING` so "unrecognized" never counts as passing.
- PR titles are attacker-controlled: they are stripped of control characters
  at ingestion (`from_graphql`) and markup-escaped at render (`_title_cell`).
  Keep both when touching those paths.
