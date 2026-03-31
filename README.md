# gh-prs

TUI for reviewing and merging GitHub pull requests, powered by `gh` CLI and [Textual](https://textual.textualize.io/).

## Install

```bash
uv sync
```

## Usage

```bash
uv run gh-prs
```

Requires the [GitHub CLI](https://cli.github.com/) (`gh`) to be installed and authenticated.

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
