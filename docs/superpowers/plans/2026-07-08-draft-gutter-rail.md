# Draft Gutter Rail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make draft PRs read as a distinct peach band in the PR list — a `▎` gutter rail plus peach-recolored `○` glyph and `draft` word.

**Architecture:** A single view-layer change in `src/view/pr_list.rs::row_for`, backed by one new named color token in `src/render/style.rs`. All three draft cues (rail, state glyph, badge word) share one peach accent. Marks are foreground-only, so they survive the `SURFACE0` selection highlight and require no layout changes.

**Tech Stack:** Rust, ratatui (TUI), Catppuccin Mocha palette.

## Global Constraints

- Accent color is peach `#fab387` = `Color::Rgb(0xfa, 0xb3, 0x87)`.
- Rail glyph is `▎` (U+258E, LEFT ONE QUARTER BLOCK).
- Every row stays 2 cells wide before the state glyph — do not change the title-width math (`left_cols = 9 + pr_num.chars().count()`).
- Comments never exceed one line (80 char); keep minimal.
- No mocks — tests assert observable spans on the rendered `Line`.
- Out of scope: footer legend, review-view header.

---

### Task 1: Draft gutter rail + peach recolors

**Files:**
- Modify: `src/render/style.rs` (add `DRAFT_ACCENT` token near the other named colors)
- Modify: `src/view/pr_list.rs` — `row_for` (`src/view/pr_list.rs:228-309`)
- Test: `src/view/pr_list.rs` (`#[cfg(test)] mod tests`, alongside `draft_pr_shows_draft_badge`)

**Interfaces:**
- Consumes: `crate::render::style::DRAFT_ACCENT: Color` (added in Step 3); `row_for(pr: &Pr, selected: bool, now: DateTime<Utc>, area_width: u16) -> Line<'static>` (existing, signature unchanged).
- Produces: no new public API. `row_for` still returns a `Line`; the draft row's first span becomes the rail.

- [ ] **Step 1: Baseline — run the suite green**

Run: `cargo test`
Expected: PASS (establish a clean baseline before changing anything).

- [ ] **Step 2: Write the failing tests**

Add these two tests inside `mod tests` in `src/view/pr_list.rs`, next to `draft_pr_shows_draft_badge`:

```rust
#[test]
fn draft_row_shows_peach_rail() {
    let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
    let mk = |is_draft: bool| Pr {
        number: 1, title: "t".into(), is_draft, state: PrState::Open,
        author: crate::data::pr::Author { login: "a".into() },
        created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        base_ref_name: "main".into(), head_ref_name: "f".into(),
        labels: vec![], status_check_rollup: vec![],
        review_decision: None, mergeable: None,
    };
    // Draft row leads with the rail glyph, painted in the draft accent.
    let draft = row_for(&mk(true), false, now, 80);
    assert_eq!(draft.spans[0].content, "▎", "draft row must lead with the rail glyph");
    assert_eq!(draft.spans[0].style.fg, Some(DRAFT_ACCENT), "rail must use DRAFT_ACCENT");
    // Ready row does not.
    let ready = row_for(&mk(false), false, now, 80);
    assert_ne!(ready.spans[0].content, "▎", "ready row must not show the rail");
}

#[test]
fn draft_state_glyph_is_peach() {
    let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
    let draft = Pr {
        number: 1, title: "t".into(), is_draft: true, state: PrState::Open,
        author: crate::data::pr::Author { login: "a".into() },
        created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        base_ref_name: "main".into(), head_ref_name: "f".into(),
        labels: vec![], status_check_rollup: vec![],
        review_decision: None, mergeable: None,
    };
    let line = row_for(&draft, false, now, 80);
    let circle = line.spans.iter().find(|s| s.content == "○").expect("draft shows ○");
    assert_eq!(circle.style.fg, Some(DRAFT_ACCENT), "draft ○ must use DRAFT_ACCENT");
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib view::pr_list`
Expected: FAIL to compile — `cannot find value DRAFT_ACCENT in this scope`. (This is the red state; Step 4 defines the token, then the asserts fail on behavior.)

- [ ] **Step 4: Add the `DRAFT_ACCENT` token**

In `src/render/style.rs`, add after the `OVERLAY1` line (the surfaces block):

```rust
// Draft accent — peach; marks draft PRs as a distinct band.
pub const DRAFT_ACCENT: Color = Color::Rgb(0xfa, 0xb3, 0x87);
```

- [ ] **Step 5: Run tests to confirm they now fail on behavior**

Run: `cargo test --lib view::pr_list`
Expected: FAIL — `draft_row_shows_peach_rail` (first span is `"  "`, not `"▎"`) and `draft_state_glyph_is_peach` (`○` is `OVERLAY0`, not `DRAFT_ACCENT`).

- [ ] **Step 6: Implement the rail**

In `src/view/pr_list.rs::row_for`, build a rail span just before the `Line::from(vec![...])`. Add:

```rust
// Draft rows lead with a peach rail; ready rows keep the blank indent.
// Either way it is 1 cell, so the row stays 2 cells before the state glyph.
let rail = if pr.is_draft {
    Span::styled("▎", row_bg.fg(DRAFT_ACCENT))
} else {
    Span::styled(" ", row_bg)
};
```

Then in the `Line::from(vec![...])`, replace the first element `Span::styled("  ", row_bg),` with:

```rust
        rail,
        Span::styled(" ", row_bg),
```

- [ ] **Step 7: Recolor the draft state glyph**

In `row_for`, the `state_glyph` match — change the draft arm from `OVERLAY0` to `DRAFT_ACCENT`:

```rust
    let state_glyph = match pr.state {
        _ if pr.is_draft => Span::styled("○", Style::default().fg(DRAFT_ACCENT)),
        PrState::Open => Span::styled("●", Style::default().fg(DIFF_ADD_FG)),
        PrState::Closed => Span::styled("●", Style::default().fg(DIFF_DEL_FG)),
        PrState::Merged => Span::styled("●", Style::default().fg(COMMIT_PALETTE[1])),
    };
```

- [ ] **Step 8: Recolor the draft word**

In the `Line::from(vec![...])`, change the draft badge span from `row_bg.fg(OVERLAY0)` to `row_bg.fg(DRAFT_ACCENT)`:

```rust
        Span::styled(draft_str, row_bg.fg(DRAFT_ACCENT)),
```

- [ ] **Step 9: Run the pr_list tests**

Run: `cargo test --lib view::pr_list`
Expected: PASS — including the new tests and the existing `draft_pr_shows_draft_badge` (badge text `"draft  "` is unchanged; only its color moved to peach).

- [ ] **Step 10: Run the full suite**

Run: `cargo test`
Expected: PASS (no regressions).

- [ ] **Step 11: Commit**

```bash
git add src/render/style.rs src/view/pr_list.rs
git commit -m "feat(view): peach gutter rail marks draft PRs in the list"
```

---

## Self-Review

**Spec coverage:**
- Rail glyph on draft rows → Steps 2, 6. ✓
- `○` recolor → Steps 2, 7. ✓
- `draft` word recolor → Step 8 (verified indirectly; existing badge test still green). ✓
- `DRAFT_ACCENT` token → Step 4. ✓
- Layout unchanged (2 cells) → rail is 1 cell + 1 space; constraint restated in Global Constraints. ✓
- Survives selection → `row_bg.fg(...)` carries the selection bg on the rail span. ✓
- Out of scope (legend, review header) → untouched. ✓

**Placeholder scan:** none — every step has exact code and commands.

**Type consistency:** `DRAFT_ACCENT: Color` defined in Step 4, referenced in Steps 2/6/7/8; `row_for` signature unchanged; `Span`, `Style`, `row_bg` all already in scope in `row_for`.
