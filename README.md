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
- **Conflicts to resolve** — PRs you created that have merge conflicts.

## Prerequisites

- [GitHub CLI](https://cli.github.com/) (`gh`) installed and authenticated (`gh auth login`)
- Python 3.14+
- [uv](https://docs.astral.sh/uv/)

## Install

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

gh prs --snooze URL   # hide a PR from the attention view until it moves
gh prs --unsnooze URL # remove a PR's snooze
gh prs --snoozed      # list snoozed PRs
```

`--count` exits non-zero when fetching fails, so status-bar scripts can tell
"no PRs" apart from "the lookup broke". With `-c` or `-r` it uses a fast
count-only query (well under a second) — ideal for frequent polling.

### Snoozing

Sometimes a PR legitimately needs _someone's_ attention but not yours — say a
dependency bump routed to you through a team when a teammate is the natural
reviewer. `gh prs --snooze <url>` hides it from the default attention view.

A snooze is tied to the PR's head commit at snooze time: as soon as the PR
gets new commits (or is rebased) it resurfaces with a warning and the snooze
is dropped, so you acknowledge a specific state, never future work. The
attention view prints how many snoozed PRs it withheld on stderr — hiding is
visible, never silent. Explicit views (`-c`/`-r`/`-a`), `--count` for those
views, and `--json` ignore snoozes entirely, so scripts and exact counts are
unaffected.

Snoozes are stored locally in `~/.config/gh-prs/snooze.json` (honors
`$XDG_CONFIG_HOME`); they never touch the PR on GitHub.

For status bars, prefer the `uv tool install` binary (`~/.local/bin/gh-prs`)
over `uv run` inside the repo — it skips ~250 ms of project resolution per
invocation.
