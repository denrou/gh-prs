# gh-prs

A simple CLI that lists the GitHub pull requests you need to act on, powered by
the `gh` CLI. No TUI — just readable, colored, grouped output.

By default it shows only the PRs that need your attention:

- **Needs your review** — PRs where your review is requested and still needed:
  once the PR is approved (mergeable without you) it is hidden unless you are
  personally on the requested-reviewers list (not just through a team), and it
  is also hidden while changes are requested (the author is reworking it).
  Drafts are excluded (not ready for review), as are conflicting PRs (a review
  would be staled by the rebase). A PR also resurfaces here when your previous
  review was dismissed.
- **New commits since your review** — PRs you already reviewed (approved,
  requested changes, left a review comment, or had your review dismissed by a
  push) whose head commit is no longer the one you reviewed — new commits or
  a rebase the author forgot to re-request review for; the case that is
  otherwise easy to miss. Hidden while the PR is conflicting (more commits
  are coming anyway). A PR never appears both here and in **Needs your
  review**: whenever it qualifies there (e.g. a re-request after a
  comment-only review), that section wins; a re-request after your
  still-standing approval keeps it here.
- **Ready to ship** — PRs you created that are approved, with CI green (or no
  checks) and no conflicts.
- **CI failed** — PRs you created where a check is failing.
- **Conflicts to resolve** — PRs you created that have merge conflicts,
  drafts included (the base moved underneath your draft; resolving early is
  cheaper than later).
- **Waiting on review — time to nudge** — PRs you created that are still
  awaiting review and have gone quiet longer than the staleness threshold
  (3 days by default). There's nothing for _you_ to do — the code is fine, CI
  is green, no conflicts — but it has been sitting long enough that pinging the
  reviewers is warranted. Any new activity (a comment, a commit) resets the
  clock, so it won't nag while there's discussion. A PR GitHub still labels
  "changes requested" shows up here too, once you've pushed past every
  outstanding review and a reviewer is on the hook again — GitHub leaves that
  label in place until someone reviews afresh, so without this the PR would
  stay invisible no matter how long it waited. Change the threshold with
  `--stale-after 5d` or the `stale_after` config setting; set it to `null` in
  the config to turn the nudge off entirely.
- **Drafts gone quiet — finish or mark ready** — draft PRs you created that
  have sat untouched longer than the same staleness threshold. A fresh draft
  is deliberately parked work-in-progress and stays out of the way (failing
  CI included — red checks are expected while iterating), but one that has
  gone quiet is probably forgotten: finish it, mark it ready, or close it.
  Shares the **Waiting on review** threshold and off switch.

## Prerequisites

- [GitHub CLI](https://cli.github.com/) (`gh`) installed and authenticated (`gh auth login`)
- Python 3.14+
- [uv](https://docs.astral.sh/uv/)

## Install

With [Homebrew](https://brew.sh/):

```bash
brew install denrou/gh-prs/gh-prs
```

Or from [PyPI](https://pypi.org/project/gh-prs/):

```bash
uv tool install gh-prs
```

Or straight from the repository:

```bash
uv tool install gh-prs --from git+https://github.com/denrou/gh-prs.git
```

### As a `gh` alias

```bash
gh alias set --shell prs 'gh-prs'
```

Then simply run:

```bash
gh prs
```

## Usage

```bash
gh prs              # PRs that need your attention (default)
gh prs -c/--created # every open PR you created
gh prs -r/--review  # every PR awaiting your review
gh prs -a/--all     # every PR you are involved with
gh prs --json       # raw JSON (for scripting)
gh prs --count      # print only the PR count for the selected view
                    # (attention count by default; handy for status bars)
gh prs --no-color   # disable colored output

gh prs snooze 123           # hide a PR (of the current repo) for 24h
gh prs snooze 123 -R o/r    # …of another repo (owner/repo)
gh prs snooze 12 34 --for 3d  # …several at once, for a custom window (12h, 3d, 1w)
gh prs unsnooze 123         # remove a PR's snooze
gh prs snooze               # with no arguments: list snoozed PRs

gh prs --stale-after 5d  # flag your PRs (review-waiting or draft) quiet this long
```

`--count` exits non-zero when fetching fails, so status-bar scripts can tell
"no PRs" apart from "the lookup broke". With `-c` or `-r` it uses a fast
count-only query (well under a second) — ideal for frequent polling.

### Snoozing

Sometimes a PR legitimately needs _someone's_ attention but not yours — say a
dependency bump routed to you through a team when a teammate is the natural
reviewer. `gh prs snooze <pr>...` hides one or more PRs from the default
attention view; `gh prs snooze` with no arguments lists what's currently
snoozed, and `gh prs unsnooze <pr>...` brings a PR back early. Reference a PR
the way `gh` does: a bare number, scoped by `-R/--repo owner/repo` (or the
repository of the current directory when omitted), or a full URL. Bare numbers
are resolved through `gh`, so Enterprise hosts work too.

A snooze lasts 24 hours by default (`--for 12h`/`3d`/`1w` to change) and is
also tied to the PR's state at snooze time: its head commit _and_ the reasons
it needs your attention. Whichever comes first — the window elapsing, new
commits landing, or those reasons changing (say a review lands and a PR that
was waiting is now yours to merge) — resurfaces the PR with a warning and
drops the snooze, so you acknowledge a specific state for a bounded time,
never future work. The attention view prints how many snoozed PRs it withheld
on stderr — hiding is visible, never silent. Explicit views (`-c`/`-r`/`-a`),
`--count` for those views, and `--json` ignore snoozes entirely, so scripts
and exact counts are unaffected.

Snoozes are stored locally in `~/.config/gh-prs/snooze.json` (honors
`$XDG_CONFIG_HOME`); they never touch the PR on GitHub.

### Configuration

Settings live in `~/.config/gh-prs/config.json` (honors `$XDG_CONFIG_HOME`),
separate from the snooze store. It's optional — every setting has a default.
Today the only key is `stale_after`, the silence threshold for the
**Waiting on review** and **Drafts gone quiet** nudges:

```json
{ "stale_after": "5d" }
```

Accepts the same duration syntax as `--for`/`--stale-after` (`12h`, `3d`,
`1w`), or `null` to disable both nudges. The `--stale-after` flag overrides the
file for a single run. An unreadable or invalid config only warns and falls
back to the 3-day default, so a typo never breaks the tool.

For status bars, prefer the `uv tool install` binary (`~/.local/bin/gh-prs`)
over `uv run` inside the repo — it skips ~250 ms of project resolution per
invocation.
