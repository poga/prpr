# Local-first PR review: drop the per-PR cache

## Problem

Today, `r` on the PR list runs `gh pr list` and `git fetch` for every open
PR's head ref. The fetched refs land in `refs/prpr/pr-N` and `origin/*`, so
the local copy of every PR branch is current. But if you're viewing PR #123
and its head has moved, the review pane keeps showing the old diff and
blame colors — the cached `PrPackage` is still keyed by the old head oid.

The root cause isn't the missing invalidation hook. It's the cache itself.
All PR-review data is derived from local git refs (`git diff`, `git blame`,
`git log -p`). Once refs are fetched, everything is reproducible in
sub-second time per file. The cache exists only to skip re-paying that cost
on re-open, and in return it introduces a `(number, head_oid)` keying
scheme, eviction questions, and the stale-`get(number)` bug at the heart of
the current complaint.

## Goals

- Refreshing the PR list also refreshes the data the user is actively
  looking at, without any explicit cache-invalidation step.
- Opening a huge PR doesn't blame files the user never scrolls to.
- The "is the cache fresh?" question disappears from the codebase.

## Non-goals

- Prefetching adjacent files on navigation (defer; easy to add later if
  navigation feels laggy).
- Persisting blame output across PR re-opens within a session (the cache
  was doing this implicitly; we're choosing to drop it).
- Changing how the PR list itself is cached. `app.cache.list` stays — it's
  the result of an actual network call.

## Design

### Principle

Only `gh pr list` is cached. Everything else is derived from local refs and
computed on demand for the file currently in view.

### Worker requests

| Request | Today | After |
|---|---|---|
| `RefreshList { generation }` | gh list + fetch + emit `ListFast`/`ListEnriched` | unchanged |
| `LoadPr(pr)` | resolve oids, diff, blame ALL files in parallel | **renamed to `OpenPr(pr)`; emits `PrDetail` + `PrDiff` only — no blame** |
| — | — | **new: `BlameFile { number, head_oid, base_oid, path, commits }` — blame one file, emit `PrFileColors`** |
| `Merge { … }` | gh pr merge | unchanged |

`OpenPr` is the fast part of today's `run_load` (steps 1–4: rev_parse, log
+ diff in parallel, emit `PrDetail`, emit `PrDiff`). The all-files blame
loop (steps 5–7) is removed. `PrColorsDone` is no longer emitted.

`BlameFile` is the per-file portion of today's blame loop, lifted into its
own request: run `git blame --porcelain` + `git log --reverse -p` for one
path, parse, emit `Response::PrFileColors`.

### App state

Drop `PrPackage` and `Cache.packages` entirely. `Cache` keeps only
`list: Option<Vec<Pr>>` and its `set_list` method.

`PrReviewState` becomes the single owner of the open PR's data:

```rust
pub struct PrReviewState {
    pub detail: Option<PrDetail>,           // arrives on PrDetail
    pub files: Vec<FileDiff>,               // arrives on PrDiff
    pub colors: HashMap<String, ColorState>,
    pub commit_stats: HashMap<String, CommitStats>,
    pub current_file: usize,
    pub status: String,
}

pub enum ColorState {
    Loading,
    Ready(LineColors),
}
```

Anything else the review pane reads from cache today moves into
`PrReviewState`.

### Flow

**Open a PR** (from the list, pressing Enter):

1. App sets `PrReviewState { detail: None, files: vec![], colors: empty, current_file: 0, status: "loading…" }`.
2. App dispatches `Request::OpenPr(pr)`.
3. Worker emits `Response::PrDetail { number, result }` → app populates
   `review.detail`.
4. Worker emits `Response::PrDiff { number, result }` → app populates
   `review.files`. App immediately dispatches
   `BlameFile { … path: files[0].path }` and inserts
   `colors[files[0].path] = Loading`.
5. Worker emits `Response::PrFileColors { … }` → app inserts
   `colors[path] = Ready(line_colors)` and merges per-commit stats.

**Navigate to another file** (next/prev/picker):

1. Update `review.current_file`.
2. Read `review.colors.get(path)`:
   - `None` → insert `Loading`, dispatch `BlameFile`.
   - `Some(Loading)` → no-op (request is already in flight).
   - `Some(Ready)` → no-op.
3. Diff text renders immediately from `review.files[current_file]`. Color
   layer paints under it as soon as `Ready` lands (same render path as
   today).

**Refresh in list view (`r`):** unchanged. `gh pr list` + `git fetch`.

**Refresh in review view (`r`):** today this maps to `Action::Refresh` and
dispatches `LoadPr(current_pr)`. After this change it dispatches
`OpenPr(current_pr)` and resets `review.files = vec![]`,
`review.colors = empty`, `review.detail = None`, `review.status = "loading…"`.
The visible file's blame re-fires when `PrDiff` lands.

**Auto-refresh:** unchanged. Only fires while focused on the list (see
`should_auto_refresh` in `src/app.rs:39`), so the review pane is never
affected.

### What goes away

- `data::cache::PrPackage` struct.
- `data::cache::Cache::packages` HashMap and its `(number, head_oid)` key.
- `Cache::insert_partial`, `Cache::update_diff`, `Cache::add_file_colors`,
  `Cache::get`.
- `Response::PrColorsDone`.
- The parallel all-files blame loop in `run_worker::run_load` (the body of
  today's stages 2–3 — files-loop and parallel pool).
- The `commit_stats` zero-fill in `insert_partial`. Per-commit stats now
  arrive incrementally on `PrReviewState`; zero-fill is done when the
  commits list is first known (i.e. on `PrDetail`).

### Status-line behavior

Today the review status string transitions: `"loading…"` → `"loading diff…"`
(on `PrDetail`) → `"coloring N files…"` (on `PrDiff`) → `"N files"` (on
`PrColorsDone`). The last transition disappears with the all-files blame
loop.

New transitions: `"loading…"` → `"loading diff…"` (on `PrDetail`) →
`"N files"` (on `PrDiff`). The status becomes final as soon as the diff
parses. Per-file blame progress is conveyed through `colors[path]` state
(`Loading` vs `Ready`) — the view layer can show a marker next to a file
in the file bar/picker if desired, but the status string no longer tracks
blame progress.

### Tradeoffs

- **Win:** model is one-way. Worker emits responses; review state holds
  them; UI reads them. No cache, no keying scheme, no invalidation.
- **Win:** huge PRs (50+ files) don't pay for blames the user never views.
- **Cost:** revisiting a file in the same review session re-blames it
  (~50–200ms per file on typical files). Diff text is unaffected so the
  user sees the file immediately; only the color layer waits.
- **Cost:** closing a PR and re-opening it re-runs `OpenPr` + first-file
  blame. Today this is instant from the cache.

These costs are bounded, visible (streaming), and only paid on
re-interaction. The invalidation pain we're hitting today is paid on
every refresh whether the user does anything or not.

## Test plan

**Worker:**
- `OpenPr` emits exactly `PrDetail` then `PrDiff`. No `PrFileColors` and no
  `PrColorsDone`.
- `BlameFile` emits exactly one `PrFileColors` for the requested path.
- `BlameFile` with a missing ref / unreadable file emits a `PrLoadError`
  (or equivalent path-scoped error variant — pick one in implementation).

**App:**
- On `Response::PrDiff`, a `BlameFile` request is dispatched for
  `files[current_file]` and `colors[path]` becomes `Loading`.
- Navigating to a file with `colors[path] == None` dispatches `BlameFile`
  and marks `Loading`.
- Navigating to a file already `Loading` or `Ready` does NOT dispatch.
- `Action::Refresh` in review view dispatches `OpenPr` and clears
  `review.files`, `review.colors`, `review.detail`.

**Regression coverage:**
- The end-to-end PR load test (`6054775 test(app)`) must be adapted to the
  new response sequence: PrDetail → PrDiff → (per-file BlameFile dispatch)
  → PrFileColors for that one file.

## Migration

Single change set. The cache module shrinks to list-only; worker request
enum changes; review state grows. No backwards-compat shims — `LoadPr` is
renamed/replaced, not kept alongside.
