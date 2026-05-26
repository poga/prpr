# PR list — inline files for the selected PR

## Goal

Show, in the PR list, what files each PR changes — without entering the PR detail view. The user scans PRs by stepping through the list with `j/k`; the currently selected row expands inline to show its file list with per-file `+adds -dels`.

This makes the most common scanning workflow (which PR touched which area of the code?) a single key away instead of an `Enter`-then-`Esc` round-trip.

## Non-goals

- The detail view (`pr_review`) is unchanged. The file picker, blame coloring, and diff rendering all still live there.
- No caching layer is introduced. File data is read directly from local refs every time selection lands on a PR.
- Closed and merged PRs are out of scope (see [Scope reduction](#scope-reduction)).

## UX

### Layout

The list keeps its current single-line-per-PR layout. The **selected** row gains a block of file rows directly underneath it:

```
● ✓ ✓   #482  Fix scheduler off-by-one              [bug]  alice  c2d · u1h
● ✓ ·   #479  Add /metrics endpoint                 [perf] bob    c4d · u3h   ← selected
  ├ src/metrics.rs              +120 -3
  ├ src/server.rs                +14 -2
  ├ tests/metrics_test.rs        +85
  └ docs/metrics.md              +30
○ … ·   #478  WIP: refactor cache                    [wip]  carol  c1w · u2d
```

### Selection model

- Moving up/down with `j/k`/arrows auto-expands the new row and collapses the previous one. There is no explicit toggle, no extra key.
- The expanded block is shown for **exactly the currently selected row**, never more.
- The footer keybinding legend gains no new entry — the behavior is implicit.

### File row format

Each file row is:

```
  ├ <path>                 <+adds -dels>
  └ <path>                 <+adds -dels>
```

- The tree glyph is `├` for every file except the last, which is `└`. Indented by 2 cells from the left edge to align under the PR row.
- The path is rendered in `TEXT`; `+N` in `DIFF_ADD_FG`, `-N` in `DIFF_DEL_FG`; the `+`/`-` block is right-aligned to the row width.
- Paths longer than the available width are truncated from the left with a leading `…` (a left-truncated path keeps the filename visible).
- Pure deletions show only `-N`, pure additions only `+N`. Zero-change files (rename only) show neither.

### Long file lists

All changed files are rendered, even if the block pushes following PR rows off the bottom of the viewport. There is no `… N more` cap.

The viewport scrolls when the selection moves: see [Scroll behavior](#scroll-behavior).

### Loading and error states

File data is computed on each selection change. Time budget: a local `git diff --numstat` on already-fetched refs is typically <50ms, so the loading state is brief.

- **Loading:** Under the selected row, show a single italic line:
  ```
    loading files…
  ```
  in `OVERLAY1`.
- **Error:** Show a single line:
  ```
    error: <short message>
  ```
  in `DIFF_DEL_FG`. Errors are local-only (e.g., a ref disappeared between refresh and selection). The user can press `r` to refresh.

### Scroll behavior

The list view does not scroll today — it relies on the terminal having enough height. With expanded file blocks, a sprawling PR can push later rows off-screen. The renderer keeps **the selected PR's first row** visible by introducing a vertical scroll offset:

- The renderer computes line positions for every row (PR row + its files when selected).
- The viewport offset is the smallest non-negative value that keeps the selected PR's row in view, preferring ≥2 lines of context above and below the selected PR row when space permits.
- Pure scrolling beyond the selection is out of scope; users navigate by moving selection.

## Data model

### `data::pr::FileMeta` (existing)

```rust
pub struct FileMeta {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}
```

Already used by `PrDetail`. The inline-files feature reuses it verbatim.

### `view::pr_list::PrListState` — new field

```rust
/// Files for the currently selected PR. Cleared on every selection
/// change and on refresh. Tagged with the PR number so a stale response
/// from a previous selection is dropped.
pub expanded: Option<ExpandedFiles>,

pub enum ExpandedFiles {
    Loading { number: u32 },
    Ready { number: u32, files: Vec<FileMeta> },
    Error { number: u32, message: String },
}
```

The `number` tag is the staleness key — when a `ListFiles` response arrives, the app only applies it if `expanded.number() == response.number` **and** the currently selected PR is still `number`.

### Removed fields

- `PrListState::filter_open_only: bool` — deleted.

## Worker

### New request

```rust
Request::ListFiles {
    number: u32,
    base_ref: String,   // e.g. "main"
}
```

The worker constructs both refs locally: head is `refs/prpr/pr-<number>`, base is `origin/<base_ref>`. Dispatched by the app whenever selection changes (and on initial load once the first PR list arrives).

### New response

```rust
Response::ListFiles {
    number: u32,
    result: anyhow::Result<Vec<FileMeta>>,
}
```

### Worker behavior

In `run_worker`, handle `ListFiles` by:

1. Resolve `head_oid = git.rev_parse(repo_root, &format!("refs/prpr/pr-{number}"))`.
2. Resolve `base_oid = git.rev_parse(repo_root, &format!("origin/{base_ref}"))`.
3. Call a new `git.diff_numstat(repo_root, &base_oid, &head_oid)` which runs `git diff --numstat <base>..<head>` and parses each line into a `FileMeta`.
4. Send `Response::ListFiles { number, result }`. If either ref resolution fails, the result is `Err`.

The worker request channel is FIFO, so a burst of selection changes (user holds `j`) queues file fetches. Stale ones complete but the app drops them based on the staleness key — no need to cancel.

### Worker thread sizing

A single-row dispatch per selection change means small bursts of work. Since each call is local-only and typically <50ms, no parallelism beyond the existing single worker thread is required. If profiling later shows hold-`j` produces a noticeable lag, we can switch to a detached scoped thread per `ListFiles` (like `list_prs_enriched` already does).

## Git client

### `GitClient` — new method

```rust
trait GitClient {
    // ... existing methods ...

    /// Returns one entry per changed file in `base..head`. Path renames
    /// expose the new path; `additions`/`deletions` are zero for pure
    /// renames or binary files (matches `git diff --numstat` semantics).
    fn diff_numstat(&self, repo_root: &Path, base: &str, head: &str)
        -> Result<Vec<FileMeta>>;
}
```

Production implementation runs `git diff --numstat <base>..<head>` and parses each line:

```
<additions>\t<deletions>\t<path>
```

- `-\t-` means binary; emit `additions = 0, deletions = 0`.
- For rename lines (`{old => new}` form), use the bare path that `--numstat` emits (no further parsing needed since we don't pass `-M`).

The fake (`data::git::fakes::FakeGit`) gets a parallel `numstats: HashMap<(String, String), Vec<FileMeta>>` map for tests.

## App wiring

### Selection-change hook

Both `Action::ListUp` and `Action::ListDown` (plus `ListTop`, `ListBottom`, search-driven jumps) call a new helper:

```rust
fn after_selection_change(app: &App, st: &mut AppState) {
    let Some(pr) = st.list.visible_prs().get(st.list.selected) else {
        st.list.expanded = None;
        return;
    };
    let n = pr.number;
    // Always re-issue — no cache. Loading state is brief on local refs.
    st.list.expanded = Some(ExpandedFiles::Loading { number: n });
    app.request(Request::ListFiles {
        number: n,
        base_ref: pr.base_ref_name.clone(),
    });
}
```

Called from every action that mutates `st.list.selected` or that loads a fresh list (`ListFast` arrival).

### Response handling

In `handle_response`, add:

```rust
Response::ListFiles { number, result } => {
    let Some(sel) = st.list.visible_prs().get(st.list.selected) else { return };
    if sel.number != number { return; }  // stale (user moved on)
    let exp = st.list.expanded.as_ref().map(ExpandedFiles::number);
    if exp != Some(number) { return; }   // also stale
    st.list.expanded = Some(match result {
        Ok(files) => ExpandedFiles::Ready { number, files },
        Err(e) => ExpandedFiles::Error { number, message: format!("{e:#}") },
    });
}
```

### Initial / refresh interaction

- On `ListFast` arrival, clear `st.list.expanded` and (if the cursor is on a row) call `after_selection_change` to kick a fresh file fetch. This applies to both manual and auto refreshes — refs may have moved, and the no-cache rule means we always re-read.

## Renderer

In `view::pr_list::render_rows`, change the row emit loop:

```rust
for (i, pr) in visible.iter().enumerate() {
    lines.push(row_for(pr, i == st.selected, now, area.width));
    if i == st.selected {
        match &st.expanded {
            Some(ExpandedFiles::Loading { number }) if *number == pr.number => {
                lines.push(loading_line(area.width));
            }
            Some(ExpandedFiles::Ready { number, files }) if *number == pr.number => {
                for (fi, f) in files.iter().enumerate() {
                    let last = fi + 1 == files.len();
                    lines.push(file_line(f, last, area.width));
                }
            }
            Some(ExpandedFiles::Error { number, message }) if *number == pr.number => {
                lines.push(error_line(message, area.width));
            }
            _ => {} // no expanded data, nothing extra
        }
    }
}
```

The viewport-keeping logic mentioned in [Scroll behavior](#scroll-behavior) is implemented by computing the absolute line index of the selected PR and slicing the `lines` vec before passing to `Paragraph::new`.

## Scope reduction — closed/merged PRs

The app no longer surfaces closed or merged PRs. This is a hard scope cut, not a default filter.

Changes:

- **`gh.rs`:** both `list_prs_fast` and `list_prs_enriched` change `--state all` to `--state open`.
- **`pr_list.rs`:**
  - `PrListState::filter_open_only` field removed.
  - `visible_prs()` no longer filters by state (now only by search).
  - Header drops the `filter: open` / `filter: all` segment.
- **`app.rs`:**
  - `Action::ListCycleFilter` arm removed.
  - `filter_open_only` removed from `AppState::new`.
- **`keys.rs`:**
  - `Action::ListCycleFilter` variant removed.
  - The `f` key in the PR list no longer maps to anything.
- **`help.rs`:** remove the `f cycle filter` line. `Esc clear filter` keeps working for search.
- **Defensive parse:** if `gh` somehow returns a non-`OPEN` PR (e.g., the user runs against an unexpectedly-stale account), the deserialized rows are filtered out at the data layer (in `list_prs_fast`/`list_prs_enriched` after parse).

`PrState::Closed` and `PrState::Merged` variants stay in the enum — they're a property of the gh schema — but the rendered list and any state.matches on them are removed.

## Testing

Tests are added in the relevant module — TUI tests use the `TestBackend` pattern already established in `view::pr_list::tests`.

### `data::git::fakes::FakeGit::diff_numstat`

Smoke: a `FakeGit` populated with one `(base, head) → vec![FileMeta]` entry returns it via the trait.

### Worker

- `Request::ListFiles` with both refs resolvable → emits `Response::ListFiles { number, result: Ok(files) }` with the populated `FileMeta` list.
- `Request::ListFiles` with a missing base ref (FakeGit refs empty) → emits `Response::ListFiles { number, result: Err(_) }`.

### App

- After the first `ListFast` lands, the app dispatches a `ListFiles` request for the row at `selected = 0`. (Asserted via a spy `App` or by draining requests.)
- `Action::ListDown` clears `st.list.expanded` and dispatches a new `ListFiles` for the new row's number.
- A `ListFiles` response with a number that doesn't match the current selection is dropped (state unchanged).
- A `ListFiles` response that matches transitions `expanded` from `Loading` to `Ready`.

### Renderer

- Given an `ExpandedFiles::Ready` on the selected row, the buffer contains every file path on its own line under the row, with `+N -N` aligned right.
- Given `ExpandedFiles::Loading`, the buffer shows `loading files…` under the selected row.
- Non-selected rows have no expanded block, regardless of `expanded` state (i.e. `expanded.number` not matching).
- Search filter that lands on a different PR: the previously expanded block disappears from the buffer.

### Scope reduction

- `view::pr_list::tests` lose any `filter_open_only` setup; replace with assertions that the header no longer shows `filter:` and that `f` is a no-op.
- A new test loads a fixture with both `OPEN` and `MERGED` entries; the `gh.rs` defensive filter ensures `list_prs_fast` returns only the open one. (Fixture file is added if not already present.)

### Test fixtures

A new fixture `tests/fixtures/diff_numstat.txt` contains a few representative lines (text file, binary file, rename) parsed by the new git client method.

## Error handling

- A `ListFiles` failure is non-fatal. The error message is shown inline under the selected row. The user can navigate away or press `r` to refresh.
- If `refs/prpr/pr-<N>` doesn't exist locally (e.g., a brand-new PR appeared between refresh and selection), the error message is the resolution error from `rev_parse`.
- The error message is single-line and bounded in length (`truncate` from the existing module) so it never expands the row beyond one line.

## Out of scope (explicit)

- Hover/peek on non-selected rows.
- Showing file changes for closed or merged PRs.
- Any new keybinding to expand multiple rows or pin a row.
- A `… N more` cap on long file lists.
- File-row-level interactions (clicking a file to jump straight into review). The detail view stays the way to open a file.

## Open questions

None.
