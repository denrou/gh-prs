# gh-prs

TUI for reviewing and merging GitHub pull requests, powered by `gh` CLI and [Textual](https://textual.textualize.io/).

Lists all open pull requests assigned to you or requesting your review across all repositories.

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

## Keybindings

| Key     | Action                       |
|---------|------------------------------|
| `j`/`k` | Navigate down/up            |
| `Enter` | Show PR details             |
| `s`     | Toggle select current row   |
| `a`     | Toggle select all           |
| `A`     | Approve PR(s)               |
| `M`     | Merge PR(s) (squash+delete) |
| `o`     | Open in browser             |
| `/`     | Filter (regex)              |
| `c`     | Clear filter                |
| `g`     | Refresh                     |
| `q`     | Quit                        |
