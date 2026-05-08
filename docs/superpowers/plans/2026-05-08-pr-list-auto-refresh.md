# PR List Auto-Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add silent auto-refresh of the PR list (60s interval while focused on List, plus a return-to-list refresh when data is older than 30s) without disturbing the user's selection or showing a refresh spinner.

**Architecture:** Two pure helpers (`should_auto_refresh`, `reselect_by_number`) drive the logic; one new piece of state (`last_refresh_at: Option<Instant>` on `AppState`); one helper (`send_refresh`) funnels every refresh path through a single point that updates the timer. All changes live in `src/app.rs`. No worker, view, or data-layer changes.

**Tech Stack:** Rust 2024, ratatui 0.30, crossterm 0.29, chrono 0.4. Existing test conventions: in-file `#[cfg(test)] mod tests` blocks with `pretty_assertions`.

**Spec:** `docs/superpowers/specs/2026-05-08-pr-list-auto-refresh-design.md`

---

## File Structure

All changes are in **`src/app.rs`**. Two new private functions, one new helper, one new state field, four call-site funnels, one handler tweak, and an in-file test module.

No new files. No changes to `src/data/worker.rs`, `src/view/pr_list.rs`, `src/keys.rs`, or any test fixture.

---

### Task 1: Pure gate function `should_auto_refresh` + constant + tests

**Files:**
- Modify: `src/app.rs` (add constant, function, and test module)

- [ ] **Step 1: Add the failing test module at the bottom of `src/app.rs`**

Append to `src/app.rs` (file currently ends at the closing `}` of `handle_commits_modal` around line 707; add the following after that):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn auto_refresh_blocked_when_not_on_list() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::Review,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_merging() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::List,
            true,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_last_refresh_unset() {
        let now = Instant::now();
        assert!(!should_auto_refresh(
            FocusedView::List,
            false,
            None,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_interval_not_elapsed() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(30));
        assert!(!should_auto_refresh(
            FocusedView::List,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_fires_when_interval_elapsed() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(61));
        assert!(should_auto_refresh(
            FocusedView::List,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }
}
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test -p prpr --lib app::tests`
Expected: compilation error — `cannot find function 'should_auto_refresh' in this scope`.

- [ ] **Step 3: Add the constant and `should_auto_refresh` function**

In `src/app.rs`, change line 10:

```rust
use std::time::Duration;
```

to:

```rust
use std::time::{Duration, Instant};
```

Then directly above `pub type Term = ...` (currently line 37), insert:

```rust
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

fn should_auto_refresh(
    focused: FocusedView,
    merging: bool,
    last_refresh_at: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> bool {
    if focused != FocusedView::List {
        return false;
    }
    if merging {
        return false;
    }
    match last_refresh_at {
        None => false,
        Some(t) => now.duration_since(t) >= interval,
    }
}
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test -p prpr --lib app::tests`
Expected: all 5 tests pass.

- [ ] **Step 5: Verify the whole crate still builds cleanly**

Run: `cargo build`
Expected: build succeeds with no new warnings (function is referenced from `mod tests`, so no dead_code lint).

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): add should_auto_refresh gate + interval constant"
```

---

### Task 2: Pure helper `reselect_by_number` + tests

**Files:**
- Modify: `src/app.rs` (add function, extend test module)

- [ ] **Step 1: Add the failing tests inside the existing `mod tests` block**

In `src/app.rs`, inside the `#[cfg(test)] mod tests` block added in Task 1, append (before the closing `}` of the test module):

```rust
    #[test]
    fn reselect_keeps_position_when_pr_still_present() {
        let new = [101u32, 99, 42, 7];
        // prev = 42, was at index 1; now at index 2
        assert_eq!(reselect_by_number(Some(42), &new, 1), 2);
    }

    #[test]
    fn reselect_falls_back_to_clamped_old_idx_when_pr_gone() {
        let new = [101u32, 99, 7];
        // prev = 42 no longer in the list; old_idx 1 stays valid
        assert_eq!(reselect_by_number(Some(42), &new, 1), 1);
    }

    #[test]
    fn reselect_clamps_old_idx_when_list_shrinks() {
        let new = [101u32, 99];
        // prev = 42 gone, old_idx 5 clamped to len-1 = 1
        assert_eq!(reselect_by_number(Some(42), &new, 5), 1);
    }

    #[test]
    fn reselect_handles_empty_list() {
        let new: [u32; 0] = [];
        assert_eq!(reselect_by_number(Some(42), &new, 3), 0);
    }

    #[test]
    fn reselect_with_no_prev_clamps_old_idx() {
        let new = [101u32, 99, 7];
        assert_eq!(reselect_by_number(None, &new, 5), 2);
    }
```

- [ ] **Step 2: Run the new tests to confirm they fail**

Run: `cargo test -p prpr --lib app::tests::reselect`
Expected: compilation error — `cannot find function 'reselect_by_number' in this scope`.

- [ ] **Step 3: Add `reselect_by_number`**

In `src/app.rs`, directly below the `should_auto_refresh` function added in Task 1, insert:

```rust
fn reselect_by_number(prev: Option<u32>, new_numbers: &[u32], old_idx: usize) -> usize {
    if let Some(n) = prev
        && let Some(i) = new_numbers.iter().position(|m| *m == n)
    {
        return i;
    }
    old_idx.min(new_numbers.len().saturating_sub(1))
}
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test -p prpr --lib app::tests`
Expected: all 10 tests pass (5 from Task 1 + 5 new).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): add reselect_by_number helper"
```

---

### Task 3: `last_refresh_at` state + `send_refresh` helper + funnel existing call sites

This task introduces the state and helper, and routes the three existing refresh paths (cold start, manual `r`, post-merge) through the helper. No new behavior is observable yet — this is a pure refactor that makes Tasks 4 and 5 trivial.

**Files:**
- Modify: `src/app.rs` (state field, helper, three call sites)

- [ ] **Step 1: Add `last_refresh_at` to `AppState`**

In `src/app.rs`, find the `pub struct AppState` definition (currently around line 65) and add the field at the end:

```rust
pub struct AppState {
    pub focused: FocusedView,
    pub list: PrListState,
    pub review: Option<PrReviewState>,
    pub current_pr: Option<u32>,
    pub picker: Option<FilePickerState>,
    pub merge: Option<MergeModalState>,
    pub merging: Option<MergingState>,
    pub commits: Option<CommitsModalState>,
    pub pending_g: bool,
    pub running: bool,
    pub last_refresh_at: Option<Instant>,
}
```

In the `impl AppState { pub fn new(...) }` block (currently around line 78), add the field initializer at the end of the struct literal:

```rust
        Self {
            focused: FocusedView::List,
            list: PrListState {
                repo_name,
                branch,
                prs: vec![],
                selected: 0,
                filter_open_only: true,
                search: None,
                status: String::new(),
                loading: false,
            },
            review: None,
            current_pr: None,
            picker: None,
            merge: None,
            merging: None,
            commits: None,
            pending_g: false,
            running: true,
            last_refresh_at: None,
        }
```

- [ ] **Step 2: Add the `send_refresh` helper**

In `src/app.rs`, directly above `pub fn run(...)` (currently around line 136), insert:

```rust
fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    if !silent {
        st.list.loading = true;
    }
    app.request(Request::RefreshList);
}
```

- [ ] **Step 3: Funnel the cold-start refresh through `send_refresh`**

In `pub fn run(...)`, replace the current body's first two statements (currently lines 139–140):

```rust
    // Kick off the initial PR list load. The first draw will show
    // "loading PRs…" while the worker thread does the gh subprocess.
    st.list.loading = true;
    app.request(Request::RefreshList);
```

with:

```rust
    // Kick off the initial PR list load. The first draw will show
    // "loading PRs…" while the worker thread does the gh subprocess.
    send_refresh(app, st, false);
```

- [ ] **Step 4: Funnel the manual `r` refresh through `send_refresh`**

In `handle_key`, find the `Action::ListRefresh` arm (currently around line 404):

```rust
        Action::ListRefresh => {
            st.list.loading = true;
            app.request(Request::RefreshList);
        }
```

Replace with:

```rust
        Action::ListRefresh => {
            send_refresh(app, st, false);
        }
```

- [ ] **Step 5: Funnel the post-merge refresh through `send_refresh`**

In `handle_response`, find the `Response::MergeDone { number, result: Ok(()) }` arm (currently around line 203). It currently ends:

```rust
            st.list.status = format!("merged #{number}");
            st.list.loading = true;
            st.list.prs.clear();
            st.list.selected = 0;
            app.request(Request::RefreshList);
        }
```

Replace those last four lines with:

```rust
            st.list.status = format!("merged #{number}");
            st.list.prs.clear();
            st.list.selected = 0;
            send_refresh(app, st, false);
        }
```

- [ ] **Step 6: Build and run all tests**

Run: `cargo test`
Expected: all tests pass (existing pr_list snapshot tests + new app tests). No behavioral change.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs
git commit -m "refactor(app): funnel all refreshes through send_refresh + track last_refresh_at"
```

---

### Task 4: Wire the interval gate into the main loop

**Files:**
- Modify: `src/app.rs` (`run` event loop)

- [ ] **Step 1: Add the auto-refresh check to the main loop**

In `pub fn run(...)`, find the main `while st.running` loop (currently around line 142). The current body is:

```rust
    while st.running {
        // Drain any worker responses before drawing.
        while let Ok(resp) = app.worker.rx.try_recv() {
            handle_response(app, st, resp);
        }

        term.draw(|f| draw(f, app, st))?;

        // Short timeout so we pick up worker responses promptly.
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(k) => handle_key(app, st, k),
                Event::Mouse(m) => handle_mouse(app, st, m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
```

Replace with:

```rust
    while st.running {
        // Drain any worker responses before drawing.
        while let Ok(resp) = app.worker.rx.try_recv() {
            handle_response(app, st, resp);
        }

        // Silent auto-refresh: while the user is on the list and not in
        // the middle of a merge, re-fetch every AUTO_REFRESH_INTERVAL so
        // CI / review / merge-by-others changes show up without pressing r.
        if should_auto_refresh(
            st.focused,
            st.merging.is_some(),
            st.last_refresh_at,
            Instant::now(),
            AUTO_REFRESH_INTERVAL,
        ) {
            send_refresh(app, st, true);
        }

        term.draw(|f| draw(f, app, st))?;

        // Short timeout so we pick up worker responses promptly.
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(k) => handle_key(app, st, k),
                Event::Mouse(m) => handle_mouse(app, st, m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
```

- [ ] **Step 2: Build and run all tests**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 3: Manual smoke test**

Run: `cargo run`
Watch the PR list for ~70 seconds. The footer should NOT show a "refreshing…" spinner during the auto tick (silent), but a fresh `gh pr list` should fire after 60s. Verify by checking that PR ages tick forward (e.g. a "1m" row becomes "2m" after the refresh redraws it).
Quit with `q`.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): silent 60s auto-refresh of PR list while focused"
```

---

### Task 5: Wire the return-to-list stale gate

**Files:**
- Modify: `src/app.rs` (`Action::BackToList` handler, add second constant)

- [ ] **Step 1: Add the staleness threshold constant**

In `src/app.rs`, directly below the `AUTO_REFRESH_INTERVAL` constant added in Task 1, add:

```rust
const RETURN_REFRESH_STALE_AFTER: Duration = Duration::from_secs(30);
```

- [ ] **Step 2: Update the `Action::BackToList` arm**

In `handle_key`, find the `Action::BackToList` arm (currently around line 472):

```rust
        Action::BackToList => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
        }
```

Replace with:

```rust
        Action::BackToList => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
            // If the cached list is older than RETURN_REFRESH_STALE_AFTER,
            // kick off a silent refresh so the user lands on fresh data.
            // Bouncing in/out of a PR review within the threshold reuses
            // the existing rows.
            let stale = st
                .last_refresh_at
                .is_none_or(|t| t.elapsed() >= RETURN_REFRESH_STALE_AFTER);
            if stale {
                send_refresh(app, st, true);
            }
        }
```

- [ ] **Step 3: Build and run all tests**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 4: Manual smoke test**

Run: `cargo run`. Open a PR (`Enter`), wait ~35 seconds, press `Esc` to return to the list. The list should re-fetch silently (rows update in place; no "refreshing…" spinner). Open a different PR and immediately `Esc` back — no extra fetch should fire (less than 30s since the previous refresh).
Quit with `q`.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): silent refresh on return-to-list when data is stale"
```

---

### Task 6: Selection preservation in `Response::ListLoaded(Ok)`

Without this, silent refreshes that reorder or insert rows cause the highlighted row to drift under the cursor.

**Files:**
- Modify: `src/app.rs` (`handle_response` `ListLoaded(Ok)` arm)

- [ ] **Step 1: Update the `Response::ListLoaded(Ok(prs))` arm**

In `src/app.rs`, find the `Response::ListLoaded(Ok(prs))` arm in `handle_response` (currently around line 165):

```rust
        Response::ListLoaded(Ok(prs)) => {
            st.list.prs = prs.clone();
            app.cache.set_list(prs);
            st.list.loading = false;
            st.list.status = String::new();
            // Clamp selection in case the list shrank.
            let n = st.list.visible_prs().len();
            if st.list.selected >= n {
                st.list.selected = n.saturating_sub(1);
            }
        }
```

Replace with:

```rust
        Response::ListLoaded(Ok(prs)) => {
            // Preserve the user's selected PR across refreshes: capture the
            // previously-selected PR's number, replace rows, then re-find the
            // same number in the new visible list. Falls back to a clamped
            // index if the PR is gone (e.g. closed/merged out of the filter).
            let prev_selected = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number);
            st.list.prs = prs.clone();
            app.cache.set_list(prs);
            st.list.loading = false;
            st.list.status = String::new();
            let new_numbers: Vec<u32> = st
                .list
                .visible_prs()
                .iter()
                .map(|p| p.number)
                .collect();
            st.list.selected =
                reselect_by_number(prev_selected, &new_numbers, st.list.selected);
        }
```

- [ ] **Step 2: Build and run all tests**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 3: Manual smoke test**

Run: `cargo run`. Use `j`/`k` to highlight a PR partway down the list. Wait ~70s for an auto-refresh. The highlight should remain on the same PR (not drift to a different row even if the list reorders). Quit with `q`.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): preserve selected PR across list refreshes"
```

---

## Acceptance criteria

- [ ] `cargo test` passes (all existing tests + 10 new tests in `app::tests`).
- [ ] `cargo build` succeeds with no new warnings.
- [ ] Manual smoke: PR list silently re-fetches every ~60s while focused; manual `r` and post-merge still show the existing spinner/loading placeholder.
- [ ] Manual smoke: returning to the list from a PR review re-fetches if more than 30s have passed; no extra fetch on quick in-and-out.
- [ ] Manual smoke: highlighted PR stays on the same row across an auto-refresh (selection preserved by PR number).
- [ ] Errors during silent refresh surface in the footer status line (no silent failures).
