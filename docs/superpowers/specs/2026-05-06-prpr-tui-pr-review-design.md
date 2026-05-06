# prpr — TUI PR Review Tool

**Status:** Design
**Date:** 2026-05-06
**Owner:** poga

## Summary

`prpr` is a fast, keyboard-driven terminal UI for reviewing GitHub Pull
Requests. It reads PR data through the `gh` CLI and renders unified diffs
where each changed line carries a colored gutter showing which commit in the
PR introduced it. The tool is read-only with one write action: merging a PR.

The differentiated feature is **per-commit color attribution in the diff**,
designed to help a visual reviewer tell at a glance which commit each line
came from.

## Goals

1. Browse open PRs in the current GitHub repository.
2. Read each PR's unified diff with per-line commit attribution rendered as
   a colored gutter.
3. Merge a PR with a chosen merge method.
4. Be snappy: every keystroke draws within one frame; data fetches happen
   off the UI thread.
5. Be visually pleasing without animations.
6. Run reliably on Kitty (the primary target). Render correctly on any
   terminal that reports truecolor; degrade gracefully (with a warning) on
   terminals that don't.

## Non-goals (v1)

- Inline review comments, posting reviews, replying to threads.
- Cross-repo PR queue (e.g. all PRs assigned to me across GitHub).
- Side-by-side diff view.
- Light-theme switching at runtime (config-file only in v1).
- Background polling for PR updates.
- Plugin or scripting interfaces.

## User flows

### F1. Triage

1. `cd` into a GitHub repo, run `prpr`.
2. PR list view appears, focused on the most recent PR.
3. `j`/`k` to move; `/` to fuzzy-search; `f` to cycle filter.
4. `q` to quit.

### F2. Review a PR

1. From the PR list, `↵` on a row → PR Review view loads.
2. Diff renders with commit-color gutter; commit strip at top names each color.
3. `j`/`k` move the cursor; status line shows the cursor line's commit.
4. `]f` / `[f` (or `Tab`) move between files; `f` opens fzf-style file picker.
5. `Esc` returns to the PR list.

### F3. Merge

1. From PR list or PR review, press `m`.
2. Modal appears with three methods (Merge / Squash / Rebase). Repo default
   is highlighted.
3. Press a letter to choose a non-default method, or `↵` to confirm the default.
4. `gh pr merge` runs; result reported in the status line.

## Tech stack

Single Rust binary `prpr`. Subprocess-driven: `gh` for GitHub operations,
`git` for diff and blame. No daemon.

| Crate | Purpose |
|-------|---------|
| `ratatui` | TUI rendering |
| `crossterm` | Terminal backend (keyboard, mouse, truecolor); supports Kitty's keyboard protocol and SGR mouse mode |
| `serde`, `serde_json` | Parse `gh --json` output |
| `anyhow`, `thiserror` | Error handling |
| `directories` | Locate `~/.config/prpr/config.toml` |
| `toml` | Parse config |
| `unicode-width` | Correct terminal column accounting |

Concurrency: main thread runs the ratatui event loop; subprocess calls run on
worker threads via channels. The choice between `std::thread` channels and a
single-threaded `tokio` runtime is deferred to implementation; the public
boundary (a `data::cache` API the views call) doesn't change either way.

## Architecture

### Module layout

```
src/
  main.rs             CLI args, config load, run App
  app.rs              Top-level App state + event loop
  view/
    mod.rs            View enum (PrList | PrReview)
    pr_list.rs        PR list view
    pr_review.rs      PR review view
    file_picker.rs    fzf-style overlay
    merge_modal.rs    Merge method picker
  data/
    gh.rs             gh CLI subprocess wrappers
    git.rs            git CLI subprocess wrappers
    pr.rs             Pr / Commit / FileDiff types
    cache.rs          In-memory cache, manual refresh, keyed by (pr#, head_sha)
  render/
    diff.rs           Diff rendering with commit-gutter
    color.rs          Palette + commit-color assignment
    style.rs          Catppuccin Mocha named colors
  config.rs           Config schema, load, defaults
  keys.rs             Keybindings
```

The exact paths may shift during implementation. The boundary that matters:
**views consume already-parsed `Pr` / `FileDiff` structs from
`data::cache`. Only the cache talks to subprocesses.** This keeps views
unit-testable against fixture data.

### Data flow

```
                     ┌──────────────┐
                     │ ratatui loop │  (main thread)
                     └──────┬───────┘
                            │ render(state)
                            │ events → state
                            │ requests → channel
                            ▼
                     ┌──────────────┐
                     │ data::cache  │
                     └──────┬───────┘
                            │ misses → worker
                            ▼
              ┌────────────────────────────┐
              │ workers: gh / git subprocs │
              └────────────────────────────┘
                            │ parsed structs → channel
                            ▲
                     ┌──────┴───────┐
                     │ data::cache  │
                     └──────────────┘
```

Cache invalidation: per-PR cache is keyed by `(pr_number, head_sha)`. On
manual refresh, if `head_sha` changed (force-push), the entry is dropped and
recomputed.

## Views

### PR List (full-screen)

```
  prpr · main · 12 open                                   filter: open
  ──────────────────────────────────────────────────────────────────────
  ● ✓ ✓ #482 fix: race condition in scheduler  [bug]     alice 2d
  ● ✗ ! #479 feat: add /metrics endpoint       [feature] bob   3d
  ○ … · #475 refactor: extract config loader   [chore]   carol 5d
  ● ✓ ✓ #471 docs: update onboarding guide     [docs]    dave  1w
  ──────────────────────────────────────────────────────────────────────
  ↵ open   m merge   r refresh   / search   f filter   q quit
  state ●open ○draft   ci ✓pass ✗fail …pend   review ✓approved !changes ·pending
```

Columns per row: state (●open / ○draft), CI (✓pass / ✗fail / …pending),
review (✓approved / !changes-requested / ·pending), `#number`, title, single
label, author, age.

Data source:
```
gh pr list --json number,title,author,state,isDraft,labels,
                   createdAt,statusCheckRollup,reviewDecision
```
Refreshed on launch and on `r`. `/` opens an inline filter that fuzzy-matches
title, author, and label names.

### PR Review (full-screen, single file at a time)

Five horizontal regions, top to bottom:

1. Header: PR meta (number, title, author, base ← head, CI + review status)
2. Commit strip: legend with colored swatches
3. File title bar: current path and `n / total`
4. Diff body: the only scrollable region
5. Status line: cursor line's commit info + key hints

```
  prpr · #482 fix: race in scheduler · alice · main ← fix-race · ✓ci ✓review
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  commits  █ a1b2c3 init structure       █ d4e5f6 enum dispatch
           █ 789abc add Wait variant     ▒ (older)
  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  src/sched.rs                                              file 1/4
  ──────────────────────────────────────────────────────────────────────
   42 █     pub fn run(&mut self, t: Task) {
   43 █         let lock = self.lock.lock();
   44 █  -      if t.state == State::Run {
   45 █  +      match t.state {
   46 █  +          State::Run  => spawn(t),
   47 █  +          State::Wait => queue.push(t),
   48 █          }
  ──────────────────────────────────────────────────────────────────────
  line 45 from d4e5f6 "enum dispatch" by alice    │ ↵ next file  Esc back
```

Diff format: unified, one file at a time. Layout per line:

```
<line-number, 4 cols right-aligned> <space> <gutter, 1 col> <space>
<diff-op " "/"+"/"-", 1 col> <space> <code>
```

The gutter is one cell, painted with the line's commit color (or the
"older" gray for out-of-window commits, or blank for context lines that
predate the PR).

### Overlay: File picker (`f`)

Centered modal. Top: query input. Below: ranked file paths with simple
fuzzy match. ↑↓ to move, ↵ to jump, Esc to cancel.

### Overlay: Merge modal (`m`)

```
  ┌── Merge #482? ─────────────────────────────────────────┐
  │                                                        │
  │   [M] Merge commit       (repo default)                │
  │   [S] Squash and merge                                 │
  │   [R] Rebase and merge                                 │
  │                                                        │
  │   ↵ confirm default     letter to pick     Esc cancel  │
  └────────────────────────────────────────────────────────┘
```

`M`/`S`/`R` selects without confirming. `↵` runs `gh pr merge --merge`,
`--squash`, or `--rebase`. Result reported in the PR list status line.

### Loading and error placeholders

- Cache miss → centered `loading…` line in place of the data region. Input
  remains responsive; navigation stays available.
- Subprocess error → one-line red message in the status line. `?` opens a
  scrollable detail overlay with full stderr.
- Terminal smaller than 80 × 24 → centered "terminal too small (need ≥80×24)"
  message; redraws automatically on resize.

## Commit color engine

### Palette (Catppuccin Mocha)

Theme:

| Role | Hex |
|------|-----|
| Background (base) | `#1e1e2e` |
| Foreground (text) | `#cdd6f4` |
| Diff add text | `#a6e3a1` |
| Diff add bg | `#1f2a1f` |
| Diff remove text | `#f38ba8` |
| Diff remove bg | `#2a1f23` |
| Line numbers (overlay0) | `#6c7086` |
| Status text dim (overlay1) | `#7f849c` |
| Older-commit gutter (surface2) | `#585b70` |

Commit color slots, in fixed order:

| Slot | Hex | Name |
|------|-----|------|
| 1 | `#89b4fa` | blue |
| 2 | `#cba6f7` | mauve |
| 3 | `#fab387` | peach |
| 4 | `#94e2d5` | teal |
| 5 | `#f9e2af` | yellow |
| 6 | `#f5c2e7` | pink |
| 7 | `#74c7ec` | sapphire |

Green and red are reserved for diff add/remove and never appear in the
commit palette. Slot order is fixed: the oldest in-window commit is always
slot 1 (blue), the next is always slot 2 (mauve), and so on. A given commit
SHA does not get a stable color across PRs — its color depends on its
position in the current PR's window.

### Assignment

Inputs from `gh pr view --json` and `git`:

- `base` = merge-base SHA
- `head` = PR head SHA
- `commits` = `git log --reverse --pretty=%H base..head` — chronological,
  oldest first

Algorithm, computed once on PR open and cached:

1. `window` = last `min(window_size, len(commits))` elements of `commits`.
   `older` = the rest.
2. Assign `palette[i]` to `window[i]` for `i in 0..len(window)`.
   All `older` commits share the surface2 gray "older" sentinel.
3. For each file in the PR:
   - For added lines (lines that survive in `head`):
     `git blame --porcelain head -- <file>` parsed once → per-line SHA map.
   - For removed lines (lines from `base`):
     `git blame --porcelain base -- <file>` parsed once → per-line SHA map.
   - Map each line's SHA to its slot color, or to "older" gray if it isn't
     in `window`.
   - Cache the line→color map.

### Edge cases

- **Force-push during review.** Cache key is `(pr_number, head_sha)`. If
  `head_sha` changes on refresh, drop the entry and recompute.
- **Merge commits in the PR.** `git blame` walks through them naturally.
  If a merge commit lands in the window-of-N and owns lines, it gets a slot
  like any other commit.
- **Context lines authored before the PR.** Blank gutter (one space).
- **Binary files, submodules, large files.** Listed in the file picker;
  diff body shows a placeholder ("binary file, not displayed").
- **Renames.** Treated as one file under the new name; blame uses
  `git blame --follow` semantics where supported.

### Performance

Target: PR with 10 files × 200 lines avg → ≤ 1 s for full attribution on
cold cache; ≤ 10 ms on hot cache (in-memory lookup).

## Input

### Global

| Key | Action |
|-----|--------|
| `Ctrl-C` | Quit immediately |
| `?` | Help overlay (full keymap) |
| `r` | Refresh current view |

### PR List

| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `↓` | Down |
| `k` / `↑` | Up |
| `g g` | Top |
| `G` | Bottom |
| `↵` | Open selected PR |
| `m` | Merge modal |
| `/` | Fuzzy filter (title/author/label) |
| `f` | Cycle filter: open → all → draft → open |
| `Esc` | Clear filter |

### PR Review

| Key | Action |
|-----|--------|
| `q` / `Esc` | Back to PR list |
| `j` / `↓`, `k` / `↑` | Cursor down / up |
| `Ctrl-d` / `Ctrl-u` | Half-page down / up |
| `g g` / `G` | Top / bottom of file |
| `]f` / `[f` | Next / previous file |
| `Tab` / `Shift-Tab` | Same as `]f` / `[f` |
| `f` | File picker overlay |
| `m` | Merge modal |
| `c` | Toggle commit strip |
| `s` | Toggle short-SHA-in-margin |

### Mouse (everywhere)

- Wheel scroll → scrolls the focused region
- Single click on PR row → focus that row
- Double click on PR row → open PR
- Single click in diff body → move cursor to that line
- Click on commit-strip swatch → jump cursor to first line attributed to
  that commit in the current file
- Click on file title bar → open file picker overlay
- Click on `[M]` / `[S]` / `[R]` in merge modal → select (Enter still confirms)
- No drag-to-select, no right-click menus

## Configuration

`~/.config/prpr/config.toml`. Every key is optional.

```toml
[colors]
theme = "mocha"          # "mocha" (default) | "latte" (light)

[commit_attribution]
window_size = 7          # commits beyond this share the older-gray gutter

[ui]
show_commit_strip = true
show_sha_margin   = false

[keys]
# Override any keybinding by action name. Defaults are baked in.
# Example: open_pr = "o"
```

CLI flags override the file:
```
prpr --window-size 5
prpr --no-commit-strip
```

## Preconditions and errors

### Launch preconditions (each fails with a clear stderr message)

1. `gh` is on `$PATH` and `gh auth status` exits 0.
2. cwd is inside a git repo whose origin is a GitHub remote.
3. stdout is a TTY.

### Error handling

- **Subprocess non-zero exit** — captured, surfaced as a one-line red status
  message, full stderr in the `?` overlay. No panics.
- **Network failure on refresh** — status line shows `refresh failed: <short>`;
  cached data stays visible.
- **Merge failure** — error from `gh pr merge` shown verbatim in the status
  line; user resolves externally.
- **Rendering errors** — too-small terminal shows a centered message that
  redraws on resize.
- **Panics** — a panic hook restores the terminal (raw mode off, alternate
  screen off) before the panic message prints.

### Truecolor

`$COLORTERM` is checked on launch; `truecolor` or `24bit` enables
truecolor output. Anything else: emit a stderr warning ("colors may render
incorrectly without truecolor support") and proceed. No 256-color fallback
in v1. Kitty is the primary supported terminal.

## Testing

### Unit

- `data::pr` — round-trip JSON fixtures from `gh pr list --json` and
  `gh pr view --json`.
- `render::color::assign_commit_colors` — given commit SHAs and `window_size`,
  asserts which slot each gets, including the "older" sentinel.
- `render::diff::render_line` — given (text, diff op, commit color),
  asserts the produced styled spans.
- Commit attribution — given fixture `git blame --porcelain` output, asserts
  the line→SHA map.

### Integration

A fixture git repo lives under `tests/fixtures/sample-repo/` (initialized
during build or vendored as a bare repo). Tests cover commit-window
assignment, diff rendering for added/removed/context lines, and force-push
cache invalidation.

`data::gh` is a trait. The test suite stubs it; the production binary uses
the subprocess implementation. No real `gh` calls in tests.

### Manual test matrix (per release)

- Kitty (primary)
- Alacritty
- WezTerm
- macOS Terminal.app — truecolor warning, degraded colors
- Inside `tmux` with truecolor passthrough enabled

## Open questions

None at design close. Remaining choices (e.g., `std::thread` vs single-threaded
`tokio`) are local implementation details and don't affect the public design.
