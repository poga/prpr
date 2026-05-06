# prpr

A keyboard-driven TUI for reviewing GitHub PRs, with per-commit color
attribution in the diff.

![](demo01.png)
![](demo02.png)

## Requirements

- `gh` CLI on `$PATH`, authenticated (`gh auth login`)
- `git` on `$PATH`
- A truecolor-capable terminal (Kitty recommended)

## Install

```bash
cargo install --path .
```

## Run

From inside a clone of any GitHub-hosted repo:

```bash
prpr
```

## Keys

Press `?` inside the app for the full keymap.
