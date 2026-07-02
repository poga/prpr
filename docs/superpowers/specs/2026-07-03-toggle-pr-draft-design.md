# Toggle PR draft ↔ ready

## Goal

Let the user flip a PR between **draft** and **ready-for-review** with a
single keypress, from both the PR list and the review view.

## Interaction

- Key: **`d`** — immediate toggle, no modal. Unbound in both contexts today.
- Acts on the selected PR (list) or the open PR (review).
- State-dependent:
  - ready PR → `gh pr ready <n> --undo` (convert to draft)
  - draft PR → `gh pr ready <n>` (mark ready for review)
- Status line shows an in-flight message, then the result.
- On success, the local `is_draft` flag flips. **No network refresh** —
  fresh data only arrives via startup, manual refresh, or auto-refresh.
  The list's existing `draft` badge and the review header marker reflect
  the new state on the next draw.

## Components

Mirrors the merge feature's path: trait method → worker request/response →
action → local state mutation.

| File | Change |
|---|---|
| `src/data/gh.rs` | `GhClient::set_pr_draft(repo_root, number, draft: bool)`. `GhCli` runs `gh pr ready <n>` (draft=false) or `gh pr ready <n> --undo` (draft=true). `FakeGh` records calls in a `Mutex<Vec<(u32, bool)>>`, like `merges`. |
| `src/data/worker.rs` | `Request::SetDraft { number, draft }` and `Response::SetDraftDone { number, draft, result }`. Worker calls `set_pr_draft` and emits the response. |
| `src/keys.rs` | `Action::ToggleDraft`, bound to `d` in both `list()` and `review()`. |
| `src/app.rs` | Action handler resolves the target PR + desired `draft = !is_draft`, sends `Request::SetDraft`, sets a pending status. `SetDraftDone { Ok }` flips `is_draft` on the list row and on `review.detail` if present. `SetDraftDone { Err }` leaves state unchanged and shows an error status. |
| `src/view/pr_review.rs` | Header gains a `· draft` marker when `is_draft` — confirmation after toggling in the review view (the list already has its badge). |
| `src/view/help.rs` | Add `d  toggle draft` to the PR list and PR review sections. |

## No in-flight lock

Unlike merge — which locks the UI via `merging` because it is
irreversible — the toggle is quick and reversible, so there is no
dedicated in-flight flag or modal. A stray double-press is harmless: it
re-sends the same desired state before the flag flips, then a later press
toggles back.

## Data flow

```
d  →  Action::ToggleDraft
   →  app resolves target PR + desired draft state
   →  Request::SetDraft { number, draft }
   →  worker: gh pr ready <n> [--undo]
   →  Response::SetDraftDone { number, draft, result }
   →  app flips local is_draft + updates status
   →  list badge / review header reflect it on next draw
```

## Testing

Real behavior, boundaries and invariants — no mocks.

- `keys.rs`: `d` → `Action::ToggleDraft` in List and Review.
- `gh.rs`: `FakeGh` records `set_pr_draft(true)` / `(false)` with correct args.
- `worker.rs`: `Request::SetDraft` yields `Response::SetDraftDone { number, draft, Ok }` and the fake recorded the call.
- `app.rs`:
  - toggling a ready PR sends `draft:true`; a draft PR sends `draft:false`;
  - `SetDraftDone { Ok }` flips the local row's flag with no `RefreshList`;
  - `SetDraftDone { Err }` leaves the flag unchanged and sets an error status.
- `pr_review.rs`: header shows the draft marker iff `is_draft`.
