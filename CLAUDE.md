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

Four-module design inside `gh_prs/`:

- **`gh.py`** — Stateless wrapper around the `gh` CLI, relying on the user's
  existing `gh auth` session. Exposes a `PullRequest` dataclass plus
  `fetch_prs()`, `count_prs()`, `fetch_pr_head()`, `ALL_QUALIFIERS`,
  `DEFAULT_STALE_AFTER`, and the `GhError` exception.
- **`snooze.py`** — Local per-PR snooze store (`{PR url: {oid, until}}` JSON
  at `$XDG_CONFIG_HOME/gh-prs/snooze.json`). Pure I/O + partitioning helpers;
  no `gh` calls. Raises `SnoozeError`.
- **`config.py`** — Human-authored settings (`{stale_after}` JSON at
  `$XDG_CONFIG_HOME/gh-prs/config.json`), kept separate from the
  machine-managed snooze store so a hand-edit can't corrupt snooze state.
  Reuses `snooze.parse_duration`; missing file → defaults; raises
  `ConfigError`.
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
- **stale** — a soft nudge: you authored it, it's still awaiting review,
  nothing else actionable fired (`not reasons`, so no `ready`/`ci-failed`
  /`conflict`), and it has gone untouched (`updatedAt`) longer than the
  staleness threshold — time to ping the reviewers. "Awaiting review" means
  not yet `APPROVED` (that's waiting to merge, not a reviewer nudge) and not a
  `CHANGES_REQUESTED` you are still reworking — with one carve-out, because
  GitHub keeps reporting `CHANGES_REQUESTED` long after the author has
  answered it (the decision only clears when a reviewer submits a _new_
  review, so a re-requested reviewer who never comes back leaves it stuck
  there forever). The ball counts as back in the reviewers' court when both
  halves of "over to you" hold: every standing changes-requested review is
  against a superseded commit (`_changes_requested_addressed` — each entry in
  `changes_requested_commits` is an oid ≠ `headRefOid`) _and_ a review request
  is pending again (`has_pending_review_request`, user or team). Requiring the
  pending request is what keeps a push that was never re-requested — the
  author's own unfinished business, by GitHub convention — out of the nudge.
  `changes_requested_commits` comes from `latestOpinionatedReviews`, **not**
  `latestReviews`: GitHub drops a reviewer from `latestReviews` the moment
  their review is re-requested, which is exactly the state this carve-out has
  to recognize, whereas `latestOpinionatedReviews` keeps each reviewer's most
  recent `APPROVED`/`CHANGES_REQUESTED` across the re-request (and isn't
  unseated by a later comment-review) — the same set `reviewDecision` is
  derived from. `_changes_requested_addressed` follows `_is_stale`'s quiet
  fail direction rather than the house one, since the nudge is its only
  caller: no standing review in hand (none, or the 50-node cap hid one), no
  `headRefOid`, or a review GitHub no longer links a commit to all read as
  _not_ answered and keep the nudge silent. The threshold is
  `DEFAULT_STALE_AFTER` (3 days), overridable via `config.json`'s
  `stale_after` or the `--stale-after` flag. This reason is the one place
  that **inverts** the house fail-safe: `_is_stale` treats a missing /
  unparseable / naive `updatedAt` as _not_ stale, because a nudge is additive
  and non-actionable — defaulting an unknown age to "stale" would fabricate a
  reason on a possibly-fresh PR. Disabled entirely when `stale_after` is
  `None` (config `null`) or `now`/`stale_after` aren't passed to
  `_attention_reasons` (so a bare `_attention_reasons(pr)` never returns it).

Drafts are deliberately parked WIP: **review**, **new-commits**, **ci-failed**
(red CI is expected while iterating), and **ready** never fire on them. Two
authored-draft reasons do:

- **conflict** — same meaning as above: the base moved underneath the draft,
  and resolving early is cheaper than later.
- **stale-draft** — the draft counterpart of **stale**: the draft has sat
  untouched (`updatedAt`) past the same staleness threshold — likely
  forgotten; time to finish it, mark it ready, or close it. Fires regardless
  of any review decision on the draft (the nudge is about the parked draft,
  not about waiting on reviewers), is suppressed while the draft is
  conflicting (**conflict** takes precedence), and shares **stale**'s
  inverted fail direction (`_is_stale`) and `now`/`stale_after` gating.

### Configuration (`config.py`, applied in `cli.py`)

User settings live in `$XDG_CONFIG_HOME/gh-prs/config.json`, separate from the
machine-managed `snooze.json` (opposite fail-safe needs; a hand-edit must not
be able to corrupt snooze state). Today the only key is `stale_after` — a
duration string (`"3d"`, `"1w"`) parsed by `snooze.parse_duration`, or `null`
to disable the **stale** and **stale-draft** nudges. Only the view path reads
it, and it degrades
to defaults with an on-stderr warning on any error (the tool never writes it,
so there is nothing to clobber). Resolution order for the threshold:
`--stale-after` flag → `config.json` → `DEFAULT_STALE_AFTER`. A bad flag value
is a hard error (explicit user input); a bad config file only warns.

### Snoozing (`snooze.py`, applied in `cli.py`)

`gh prs snooze <pr>...` records each PR's head oid, an expiry timestamp
(default 24h, `--for 12h/3d/1w`), and the PR's `attention_reasons` at snooze
time (a sorted list; captured by fetching the attention view once, and
omitted when the PR isn't in it or the fetch fails); the default attention
view (table and `--count`) then hides the PR while _all_ hold: head
unchanged, window open, and — when reasons were captured — the reason set
unchanged. The reason check is what lets a PR snoozed while waiting for
review resurface once it's reviewed and becomes yours to merge, even though
its head never moved. The same fail-safe direction as everywhere else
applies: an unknown oid, an uncomparable timestamp, a moved head, a changed
reason set, an elapsed window, or an unreadable store all _show_ the PR (a
corrupt store only warns on the view path, but is fatal for the
`snooze`/`unsnooze` subcommands, which must not clobber the file — including
the bare listing, which must not render a half-parsed store). Entries written
before reason-tracking (no `reasons` key) keep working on head-and-window
alone. Dead entries are pruned — with an on-stderr "snooze expired" warning
when the PR actually resurfaced — and the view reports how many
attention-worthy PRs it withheld. Explicit views (`-c`/`-r`/`-a`), fast
counts, and `--json` never consult the store — their output stays exact.
Entries whose PR no longer appears in any search are kept while their window
is open (the PR may be closed _or_ merely beyond the 100-node cap; deleting
on absence would lose live snoozes) and pruned quietly once it elapses.

Snoozing is exposed as subcommands, matching `gh`'s verb style
(`gh pr close`): `gh prs snooze <pr>...` and `gh prs unsnooze <pr>...`, with
the bare `gh prs snooze` listing the store (like `git branch`; `--for`
without refs is rejected — it means a forgotten ref, not a listing request).
The pre-subcommand flags (`--snooze`/`--unsnooze`/`--snoozed`) hard-error
with a migration hint, and combining a subcommand with the view flags
(`-c`/`-r`/`-a`, `--count`, `--json`, `--stale-after`) is rejected rather
than silently ignored. Each subcommand takes one or more PR references,
following `gh`'s own conventions: a bare number (`123`) or a full URL. A
bare number is scoped by `-R/--repo owner/repo`, or — when that's omitted —
the repository of the current directory. Bare numbers are resolved through
`gh.resolve_pr()` (a `gh pr view` call), so their canonical URL and host
come straight from `gh` and enterprise instances work without host-specific
URL construction; a full URL is canonicalized offline by
`normalize_pr_url()` (keeping `snooze.py` free of `gh` calls), so
`gh prs unsnooze <url>` needs no network. The old `owner/repo/123` /
`owner/repo#123` shorthand was removed in favor of a number plus `--repo`;
both forms now hard-error. References resolve independently: a bad or
not-snoozed one is reported to stderr and skipped while the rest are
applied, the store is written once, and a partial batch exits non-zero —
never clobbering the file.

## Notes

- `ruff` rule `E501` (line length) is not enforced.
- GraphQL `statusCheckRollup.state` is normalized via `_ROLLUP_STATE`; unknown
  future states map to `PENDING` so "unrecognized" never counts as passing.
- PR titles are attacker-controlled: they are stripped of control characters
  at ingestion (`from_graphql`) and markup-escaped at render (`_title_cell`).
  Keep both when touching those paths.
