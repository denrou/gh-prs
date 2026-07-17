# gh-prs

A simple CLI that lists the GitHub pull requests you need to act on, powered by
the `gh` CLI. No TUI — just readable, colored, grouped output.

By default it shows only the PRs that need your attention:

- **Needs your review** — PRs where your review is requested and still pending.
- **Ready to ship** — PRs you created that are approved, with CI green and no conflicts.
- **CI failed** — PRs you created where a check is failing.

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
gh prs --no-color   # disable colored output
```
