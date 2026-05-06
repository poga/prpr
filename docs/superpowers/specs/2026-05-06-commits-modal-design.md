# Commits modal — design

**Status:** approved · **Date:** 2026-05-06

## Goal

Replace the horizontal commit strip at the top of the review view with a vertical commits list shown in a modal overlay (mirroring the file picker). The strip itself is removed entirely — it isn't readable in its current form, and the diff body's per-line commit coloring already conveys per-line attribution.

## Non-goals

- No filtering or jumping. Selecting a commit does nothing beyond highlighting it. The modal is display-only.
- No fuzzy search inside the modal. Most PRs have ≤ 30 commits; j/k scrolling is enough.
- No change to the per-line color attribution in the diff body or to the SHA-margin (`s`) feature.

## Architecture

A new view module `src/view/commits_modal.rs` is added alongside `file_picker.rs`, exposing `CommitsModalState` and a `render` function. A new `FocusedView::CommitsModal` variant is added in `keys.rs`. `AppState` gains `commits: Option<CommitsModalState>`. The app's draw loop renders the modal as an overlay over the review view, mirroring the existing `file_picker` and `merge_modal` pattern. While `CommitsModal` is focused, a small `handle_commits_modal` in `app.rs` consumes keys (`j/k`, `↑/↓`, `Esc`/`Enter`/`c` to close).

The existing top-of-screen commit strip is removed in the same change: `render_commit_strip` deleted, `show_commit_strip` removed from both `PrReviewState` and `Config`, the `ToggleCommitStrip` action and its `c` binding removed, the strip's height pulled out of the vertical `Layout`, and the help / hint rows updated.

## Components

**`PrPackage` (existing, extended)**
- New field `commit_stats: HashMap<String, CommitStats>` keyed by commit OID.
- `CommitStats { adds: u32, dels: u32 }`. Computed once in `build_package`.

**`Commit` in `data/pr.rs` (existing, extended)**
- New field `committed_date: Option<DateTime<Utc>>` (gh `committedDate`). Optional so missing/malformed values don't fail package load.

**`data/gh.rs` GraphQL**
- The `gh pr view` field-list for commits is extended to include `committedDate`.

**`CommitsModalState` (new, in `src/view/commits_modal.rs`)**
- `rows: Vec<CommitRow>` — pre-built display rows for the current PR.
- `selected: usize`.
- `CommitRow { color: Color, short_sha: String, headline: String, author: String, relative_date: String, adds: u32, dels: u32 }`.

**`commits_modal::render(f, area, state)`**
- Centered ~60% × 60% overlay (same `centered` helper pattern as `file_picker`).
- `Block` border with title `" commits "`.
- Each row: ` █ ab12cd  fix race in worker  poga · 2d  +12 −3 `, with the selected row using `bg(SURFACE0)` + `Modifier::BOLD`.

**`AppState` / `keys.rs` deltas**
- New `FocusedView::CommitsModal`.
- New `Action::OpenCommitsModal` in the review view; `c` is rebound from `ToggleCommitStrip` to `OpenCommitsModal`.
- `Action::ToggleCommitStrip` and `show_commit_strip` are deleted.

## Data flow

**On PR load** — when `build_package` runs in the worker:
1. `gh pr view` returns commits with `committedDate`; the field deserializes into `Commit.committed_date: Option<DateTime<Utc>>`.
2. The existing per-file blame walk already produces `Blame.line_shas` (raw OIDs) and `delete_text_to_sha` (raw OIDs). A new pass reuses those: for each file, increment `commit_stats[oid].adds` once per non-empty entry in `line_shas` whose OID is in `detail.commits`, and `commit_stats[oid].dels` once per entry in `delete_text_to_sha` whose value is in `detail.commits`. Older / pre-PR commits are excluded — they're never in the modal.

**On `c` keypress in Review view**:
1. `dispatch` returns `Action::OpenCommitsModal`.
2. If `current_pr` is `None` or the cache has no `PrPackage` for that number yet, the action is a no-op (the modal needs the commit list to render). Otherwise the handler builds `Vec<CommitRow>` by zipping `detail.commits` with `commit_stats` and the palette (`assign_commit_colors(commits, window_size)`), and formats `relative_date` from `committed_date` against `Utc::now()` via a helper that emits `"just now"`, `"5m"`, `"2h"`, `"3d"`, `"2w"`, `"1mo"`, or `"—"` if the date is missing.
3. Rows are listed in `detail.commits` order (the order returned by `gh pr view`, which is chronological — oldest first, same as the existing strip).
4. Sets `st.commits = Some(CommitsModalState { rows, selected: 0 })` and `st.focused = FocusedView::CommitsModal`.

**While the modal is focused**:
- `j` / `↓` → `selected = min(selected + 1, rows.len() - 1)`.
- `k` / `↑` → `selected = selected.saturating_sub(1)`.
- `Esc` / `Enter` / `c` → `st.commits = None; st.focused = FocusedView::Review`.
- `Ctrl-C` continues to quit globally via `dispatch`.

**Drawing**: `app::draw` adds a branch — if `st.commits.is_some()`, render `commits_modal::render` after the review view. Order with respect to other overlays follows the existing pattern (last-painted wins; in practice only one overlay is open at a time).

## Edge cases

- **Empty commits.** Modal opens with zero rows; selection is 0; j/k clamp safely.
- **Single commit.** j/k are no-ops via clamping.
- **Older / pre-PR commits.** Excluded from per-commit stats, just as they're already painted with `OLDER_COMMIT` gray rather than a palette color.
- **Missing `committedDate`.** `Option<DateTime<Utc>>` deserialization tolerates absence; the formatted date falls back to `"—"`.
- **Cache hit after force-push.** `Cache` keys by `(pr_number, head_sha)`, so a force-push lands in a fresh slot and `commit_stats` is rebuilt for the new head.

## Removals

- `render_commit_strip` and the `strip_h` constraint in `pr_review::render`.
- `show_commit_strip: bool` on `PrReviewState`; the second hint row drops the `c strip` token.
- `Action::ToggleCommitStrip` and its `c` arm in `keys::review`.
- `Config::show_commit_strip` and the associated config doc / tests.
- Strip-related test fixtures or assertions (existing `pr_review.rs` tests already pass with `show_commit_strip: false`; the body row index in `binary_file_renders_placeholder` stays at 3 since the new layout is header(1) + file_bar(2) + body(min) + status(3)).

## Testing

**Unit tests in `commits_modal.rs`**
- `selection_clamps_at_top_and_bottom` — j/k at boundaries don't go out of bounds.
- `renders_one_row_per_commit` — given 3 fake `CommitRow`s, the rendered buffer contains 3 commit lines with the expected SHA strings.
- `selected_row_has_highlight_style` — the row at `state.selected` has `SURFACE0` bg / bold; others don't.
- `relative_date_formatting` — pure helper test for `relative_date(now, then)` covering "just now", minutes, hours, days, weeks, months, and the missing-date `"—"` case.

**Tests in `pr_review.rs`**
- `commit_strip_is_gone` — render with the existing fixture and assert no row contains `commits  █`.
- Existing `binary_file_renders_placeholder` continues to assert body at row 3.

**`build_package` test in `worker.rs`**
- `commit_stats_counts_adds_and_dels` — using the existing `diff_basic.patch` + `pr_view.json` fixtures and a stubbed blame, assert `pkg.commit_stats[oid].adds` matches head lines attributed to that commit, and `dels` matches delete-text entries pointing at it.

**Fixtures**
- `tests/fixtures/pr_view.json` is extended to include `committedDate` on each commit (ISO-8601, e.g. `"2026-05-04T12:00:00Z"`). Existing tests that deserialize this fixture keep working because `committed_date` is `Option`.

**Manual verification**
- Open a PR with several commits, press `c`, navigate with j/k, close with Esc / Enter / `c`. Confirm the per-line diff colors and SHA-margin behavior are unchanged. Confirm `?` help no longer mentions `c strip`.
