# Draft Gutter Rail — Design

## Problem

In the PR list, a draft PR is signaled two ways and both are painted in
`OVERLAY0` (`#6c7086`) — the same dim grey as the age column:

- the leftmost state glyph: `○` (hollow) vs `●` (filled green) for ready PRs
- a small `draft` word tucked before the author name

Against a full-brightness row, two dim-grey marks don't register. A draft
reads as visually identical to a ready PR, so you can't tell at a glance which
rows are actionable.

## Goal

Make draft rows read as their own **distinct, neutral band** — unmistakably
different when scanning the list, without being louder or quieter than ready
PRs. Semantically a draft isn't ready for review; it should be recognizable,
not shouting and not hidden.

## Approach: left gutter rail

Give draft rows a peach `▎` (U+258E, left one-quarter block) in the leftmost
cell. Non-draft rows keep their blank indent. Consecutive drafts form a
continuous peach rail down the left edge, so the band is trackable in
peripheral vision while scanning.

Repaint the two existing draft cues from `OVERLAY0` to the same peach accent so
they stop blending into the age column and read as one "draft" identity:

- the `○` state glyph → peach
- the `draft` word before the author → peach

Accent color: peach `#fab387` (warm, reads as "work in progress" without
alarm; distinct from the green / red / blue already in use for CI, conflicts,
and PR numbers).

### Why a rail, not a background tint or a chip

- **Survives selection.** The rail and glyphs are *foreground* marks, so a
  selected draft still shows its peach band over the `SURFACE0` selection
  highlight. No background collision, no special-casing. A full-row background
  tint would be overwritten by the selection color and would also render
  unevenly across terminals.
- **Neutral, not loud.** A thin rail is a different-not-louder mark. A filled
  `DRAFT` chip would read louder than the neutral band we want.
- **No reflow.** The rail replaces the first of the two leading indent cells
  (`▎ ` for drafts vs `  ` for ready), so every row stays 2 cells wide before
  the state glyph. Column alignment and the title-width math are untouched.

## Changes

### `src/render/style.rs`

Add a named token so the draft accent is self-documenting rather than a bare
`COMMIT_PALETTE[5]` reuse:

```rust
pub const DRAFT_ACCENT: Color = Color::Rgb(0xfa, 0xb3, 0x87); // peach
```

### `src/view/pr_list.rs` — `row_for`

- Emit a leading rail span: for a draft, `▎` styled `row_bg.fg(DRAFT_ACCENT)`
  followed by a single space; for a ready PR, the existing two spaces. Keeping
  it 2 cells wide preserves `left_cols = 9 + pr_num.chars().count()`.
- Recolor the draft state glyph arm from `OVERLAY0` to `DRAFT_ACCENT`.
- Recolor the `draft` word span from `OVERLAY0` to `DRAFT_ACCENT`.

## Testing

Assert observable spans on the rendered `Line` (matching the existing
`draft_pr_shows_draft_badge` style):

- A draft row's first span is `▎` styled `DRAFT_ACCENT`; a ready row's first
  span is not the rail glyph.
- A draft row's `○` state glyph is `DRAFT_ACCENT`, not `OVERLAY0`.

The existing `draft_pr_shows_draft_badge` test still passes — the `draft` badge
text is unchanged; only its color changes.

## Out of scope

- The footer legend (`○draft` stays grey — the rail is self-explanatory).
- The review-view header, which already has its own draft marker.
