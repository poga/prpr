# PR List Auto-Refresh — Design

## Goal

Keep the PR list view fresh without requiring the user to press `r`. The list should reflect upstream PR state (new PRs, CI changes, review changes, merges by others) within roughly a minute while the user is looking at it, and immediately re-sync when the user returns to the list from a PR review.

## Non-goals

- Real-time updates from GitHub (no webhooks/streaming).
- Auto-refreshing the PR review view itself.
- Reducing latency below the worker's `gh pr list` round-trip.

## Behavior

| Trigger | When | Visibility |
| --- | --- | --- |
| Interval tick | `focused == List`, no merge in flight, ≥ 60s since the last refresh was *sent* | Silent — no spinner, rows stay visible, errors still surface in the status line |
| Return to list | `Action::BackToList`, ≥ 30s since the last refresh was sent | Silent (same as above) |
| Manual `r` | User presses `r` on the list | Visible — current behavior unchanged (footer spinner, "refreshing…") |
| Post-merge | Existing path | Visible — current behavior unchanged (rows cleared, "loading PRs…" centered) |

"Silent" means `st.list.loading` stays `false`. `pr_list::render_rows` already preserves rows whenever loading is false, so no rendering changes are needed.

## Architecture

### State

Add to `AppState` (`src/app.rs`):

```rust
last_refresh_at: Option<std::time::Instant>,
```

Initialized to `None`. Set whenever a `Request::RefreshList` is sent, regardless of trigger.

### Constants

In `src/app.rs`:

```rust
const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const RETURN_REFRESH_STALE_AFTER: Duration = Duration::from_secs(30);
```

### Helper

```rust
fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    if !silent {
        st.list.loading = true;
    }
    app.request(Request::RefreshList);
}
```

All four refresh entry points funnel through this helper:
- Cold start at the top of `run()` — `silent = false`
- Manual `r` (`Action::ListRefresh`) — `silent = false`
- Post-merge in `Response::MergeDone(Ok)` — `silent = false` (the existing code clears rows and sets loading explicitly; the helper subsumes setting loading)
- Auto interval / return-to-list — `silent = true`

### Trigger sites

**Interval tick** — in `run()`, after draining worker responses and before `term.draw`:

```rust
if should_auto_refresh(st.focused, st.merging.is_some(), st.last_refresh_at, Instant::now()) {
    send_refresh(app, st, /*silent=*/true);
}
```

`should_auto_refresh` is a pure function:

```rust
fn should_auto_refresh(
    focused: FocusedView,
    merging: bool,
    last_refresh_at: Option<Instant>,
    now: Instant,
) -> bool {
    if focused != FocusedView::List { return false; }
    if merging { return false; }
    match last_refresh_at {
        None => false, // cold-start path handles the first fetch
        Some(t) => now.duration_since(t) >= AUTO_REFRESH_INTERVAL,
    }
}
```

**Return-to-list** — in the `Action::BackToList` branch, after the existing focus/state reset:

```rust
let stale = st
    .last_refresh_at
    .is_none_or(|t| t.elapsed() >= RETURN_REFRESH_STALE_AFTER);
if stale {
    send_refresh(app, st, /*silent=*/true);
}
```

### Selection preservation

Silent refresh that shifts rows under the cursor is jarring. In `handle_response` for `Response::ListLoaded(Ok(prs))`:

1. Capture the currently selected PR's number from `st.list.visible_prs().get(st.list.selected).map(|p| p.number)`.
2. Replace `st.list.prs`.
3. Compute the new visible list and reselect by number when possible:
   ```rust
   fn reselect_by_number(prev: Option<u32>, new_visible: &[&Pr], old_idx: usize) -> usize {
       if let Some(n) = prev {
           if let Some(i) = new_visible.iter().position(|p| p.number == n) {
               return i;
           }
       }
       old_idx.min(new_visible.len().saturating_sub(1))
   }
   ```
4. Apply the result to `st.list.selected`.

This replaces the existing index-clamp at the same site. Post-merge keeps its explicit `st.list.selected = 0` *before* sending the request, so the captured "previous number" is the just-merged PR — which is gone from the new list, so we fall through to the clamp at index 0. Behavior unchanged.

## Data flow

```
main loop iteration
  └─ drain worker responses → handle_response
  └─ should_auto_refresh? → send_refresh(silent=true)
  └─ term.draw
  └─ event::poll(100ms)
       └─ key event → handle_key
            └─ Action::BackToList → if stale, send_refresh(silent=true)
            └─ Action::ListRefresh → send_refresh(silent=false)
            └─ ...
```

The 100ms poll cadence already lets us check the gate ~10×/second, which is more than enough resolution for a 60s interval.

## Error handling

Failed silent refreshes write to `st.list.status` via the existing `Response::ListLoaded(Err(_))` arm. The footer already prefers status over legend, so the user sees the error. `last_refresh_at` is set at send time, so a failed refresh doesn't retry-storm — the next attempt is 60s later. Manual `r` is always available to retry sooner.

## Edge cases

- **Search input.** `st.list.search.is_some()` does not block auto-refresh — typing and updates coexist. Selection-by-number keeps the highlighted PR stable across the update.
- **Filter toggle / `f`.** Doesn't reset `last_refresh_at`. Refresh only re-fetches; filtering is local.
- **Cold start.** `last_refresh_at` is `None` initially. The interval gate's `None` arm returns `false`, so auto-refresh never fires before the explicit cold-start `RefreshList` is sent. Once that response (or any refresh) lands the timer is established.
- **Manual `r` while a previous refresh is in flight.** The existing `r` handler already sets `loading = true` and sends another request. With the helper, both manual presses also update `last_refresh_at`, so the auto-tick will be 60s after the last manual press too — desirable.
- **Merge in flight.** `merging.is_some()` blocks both auto-triggers, matching how it blocks input.

## Testing

Two pure functions are factored out specifically so they can be unit-tested:

- `should_auto_refresh(focused, merging, last_refresh_at, now)` — verify gates: review focus → false, merging → false, list focus + None → false, list focus + 30s elapsed → false, list focus + 61s elapsed → true.
- `reselect_by_number(prev, new_visible, old_idx)` — verify: prev still present at new index, prev removed → fall back to clamped old_idx, empty new list → 0.

Existing `pr_list.rs` render snapshot tests are unaffected; no template change.

## Files touched

- `src/app.rs` — state field, constants, helper, trigger sites, response handler tweak, two pure helpers + their tests.
- No changes to `src/data/worker.rs`, `src/view/pr_list.rs`, or any other view/data module.
