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
```

`--count` exits non-zero when fetching fails, so status-bar scripts can tell
"no PRs" apart from "the lookup broke". With `-c` or `-r` it uses a fast
count-only query (well under a second) — ideal for frequent polling.

For status bars, prefer the `uv tool install` binary (`~/.local/bin/gh-prs`)
over `uv run` inside the repo — it skips ~250 ms of project resolution per
invocation.
