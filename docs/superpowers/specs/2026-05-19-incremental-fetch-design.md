# Incremental fetching: show important data first

## Problem

Both data-fetch paths in prpr are atomic and slow:

1. **PR list cold start.** A single `gh pr list --limit 200 --state all` with
   `statusCheckRollup`, `mergeable`, and `reviewDecision` blocks the UI until
   every field for every PR has been resolved server-side. The CI rollup is
   the expensive component — `gh` fans out per-PR sub-resource fetches.

2. **PR review open.** `build_package` runs `gh pr view + gh pr diff + git
   fetch` in parallel, then per-file `git blame + git log -p`, and only
   responds to the UI when every file's blame is done. On a typical PR this
   is several seconds of "loading…".

The user gets a stale-looking, unresponsive app even though large portions
of the data are available far earlier than the final response.

## Goals

- The PR list renders rows as soon as the cheap fields are available; CI/
  review/conflict glyphs fill in afterwards without disturbing the user's
  selection.
- Opening a PR renders the header and an interactive file list within
  ~200ms; the diff body renders as soon as parsing finishes; per-commit
  blame colors trickle in per file with the visible file prioritized.
- No regression in correctness: partial state always renders sensibly,
  errors are surfaced, generation skew (rapid refresh) never produces
  inconsistent state.

## Non-goals

- Pre-fetching neighboring PRs in the list view (deferred).
- Persistent on-disk cache across runs (deferred).
- Re-architecting the worker as multi-threaded request processing
  (still one worker; just streams more responses).

## Architecture

### Worker protocol

Today: one `Request::LoadPr(n)` produces one `Response::PrLoaded` with the
full `PrPackage`. After: the same single request triggers a sequence of
finer-grained responses.

```rust
pub enum Request {
    RefreshList,                  // unchanged; worker now emits two responses
    LoadPr(u32),                  // unchanged; worker now emits >1 responses
    Merge { number: u32, method: String },
}

pub enum Response {
    // PR list — two phases, both keyed by a generation counter.
    ListFast { gen: u32, result: Result<Vec<Pr>> },
    ListEnriched { gen: u32, result: Result<Vec<Pr>> },

    // PR review — granular events.
    PrDetail { number: u32, result: Result<PrDetail> },
    PrDiff   { number: u32, result: Result<Vec<FileDiff>> },
    PrFileColors {
        number: u32,
        head_oid: String,
        path: String,
        colors: LineColors,
        stats: HashMap<String, CommitStats>,
    },
    PrColorsDone { number: u32, head_oid: String },
    PrLoadError  { number: u32, error: String },

    MergeDone { number: u32, result: Result<()> },
}
```

### PR list: two-phase load

`RefreshList` runs two `gh pr list` calls sequentially on the worker. (Two
parallel gh calls contend for the same credentials/network; serial is
simpler and not slower in practice.)

1. **Fast pass** — `--json number,title,author,isDraft,state,createdAt,
   updatedAt,labels`. Emits `ListFast`. UI replaces rows immediately. CI /
   review / conflict glyphs render as "absent" variants (blank / `·` /
   blank — the same characters the renderer already produces for missing
   data).

2. **Enrichment pass** — `--json number,statusCheckRollup,reviewDecision,
   mergeable`. Emits `ListEnriched`. UI merges by `number`:
   `status_check_rollup`, `review_decision`, `mergeable` are written in
   place into each existing row. Selection and search results are
   preserved because rows are mutated, not replaced.

**Generation counter.** `AppState` carries `list_gen: u32`. Every
`send_refresh` increments it; the worker echoes it in both `ListFast` and
`ListEnriched`. UI drops responses whose `gen < current`. This also fixes
a latent race: rapid `r` presses today can interleave responses; with
generations, only the freshest cycle wins.

**Loading state.**
- Cold start (`prs.is_empty()`): the existing "loading PRs…" centered
  placeholder shows until `ListFast` arrives.
- Refresh with rows present: rows stay visible; footer carries the
  spinner. `list_refresh_in_flight` stays true until `ListEnriched`
  arrives or errors, so auto-refresh doesn't fire again mid-cycle.
- Between `ListFast` and `ListEnriched` on cold start, the footer shows
  `enriching…` so background work is never silent.

### PR review: staged load

`LoadPr(n)` runs three jobs concurrently on the worker:

1. `gh pr view n` → emits `PrDetail` on completion.
2. `gh pr diff n` → emits `PrDiff` on completion (after parsing).
3. `git fetch refs/pull/n/head` → no UI event; required for blame.

When `view + diff + fetch` are all done, the worker fans out per-file
blame. Each per-file worker emits `PrFileColors` directly to the
response channel as it finishes (no batch collection). After the last
file: `PrColorsDone`.

**Visible-file priority.** The worker handles `files[0]` synchronously
first, emits it, then fans out the rest with the existing atomic-counter
pool. Cost: one file's blame in serial. Benefit: the file the user
actually sees on open gets colors strictly before any other.

**Failure handling.** `gh pr view`, `gh pr diff`, or `git fetch` errors
emit `PrLoadError` and abort the load. Per-file blame failures are
already swallowed today and remain silent — that file renders without
colors.

**Cancellation.** None for streaming colors. If the user backs out of a
PR mid-load, late `PrFileColors` responses still arrive; `add_file_colors`
no-ops if the cache key is gone or doesn't match. Wasted CPU is bounded
and not user-visible.

### Cache: partial packages

`PrPackage` already tolerates a partial `colors` map (the renderer reads
with `colors.get(&path)` and the missing case renders uncolored). Two new
mutators promote and fill it:

```rust
impl Cache {
    /// Promote a partial: detail known, files & colors empty,
    /// commit_stats zero-filled for every PR commit.
    pub fn insert_partial(&mut self, detail: PrDetail);

    /// Swap in parsed FileDiff list. No-op if the (number, head_oid)
    /// entry has been replaced by a force-push.
    pub fn update_diff(&mut self, number: u32, head_oid: &str,
                       files: Vec<FileDiff>);

    /// Merge one file's colors and accumulate its per-commit stats.
    /// No-op if the entry is gone.
    pub fn add_file_colors(&mut self, number: u32, head_oid: &str,
                           path: String, colors: LineColors,
                           per_commit: HashMap<String, CommitStats>);
}
```

The existing `Cache::insert` (whole-package) stays for tests but is no
longer called from production code paths.

**Inflight buffer.** The window between `PrDetail` arriving and being
promotable to the cache is zero — promotion happens on `PrDetail` alone.
No separate inflight struct is needed in `AppState`. `PrDiff` and
`PrFileColors` directly mutate the cached partial.

### UI rendering

**PR review view (`view/pr_review.rs`).**

- **Header**: unchanged. Renders as soon as `pkg` exists.
- **File bar / file picker**: a new helper exposes the file list — paths
  from `pkg.files` if non-empty, otherwise from `pkg.detail.files`. The
  counter `"file i/N"` uses the same source.
- **Diff body**: if `pkg.files.is_empty()`, render
  `"  ⠋ loading diff…"`. Otherwise unchanged.
- **Status line**:
  - `"loading…"` — pre-promotion (very short window).
  - `"loading diff…"` (spinner prefix) — partial in cache, `pkg.files`
    still empty.
  - `"coloring {N} files…"` (spinner prefix) — diff ready, colors
    streaming.
  - `"{n} files"` — `PrColorsDone` received.

**Navigation in partial state (`app.rs`).** A small helper `file_count(pkg)`
returns `pkg.files.len()` if non-empty, else `pkg.detail.files.len()`.
`cycle_file` uses it as bound. `move_review` and `Bottom`/`Top` are
no-ops while `pkg.files` is empty (there are no scrollable lines yet).

**PR list view (`view/pr_list.rs`).**
- Add `enriching: bool` to `PrListState`.
- Footer shows `enriching…` (low-emphasis, spinner) when `enriching` and
  no error status is set.

## Data flow examples

**Cold open of a PR with 12 files:**

```
t=0     user presses Enter on a row
t=0     UI: focused=Review, review.status="loading…", request LoadPr(n)
t≈100   PrDetail arrives → insert_partial(detail)
        UI: header + file bar (paths from detail.files) +
            picker enabled + body shows "loading diff…"
t≈300   PrDiff arrives → update_diff(n, head, files)
        UI: diff body renders (no colors)
        status = "coloring 12 files…"
t≈800   PrFileColors {file 0} → add_file_colors(...)
        UI: file 0 (the visible one) gains colors
t≈800-2500  PrFileColors stream in for files 1..11
t≈2500  PrColorsDone → status = "12 files"
```

**Cold start of the list:**

```
t=0     `prpr` launches, send_refresh fires (gen=1)
t=0     UI: list.loading=true, body shows "loading PRs…"
t≈500   ListFast{gen=1} → list.prs populated, list.loading=false,
                            list.enriching=true
        UI: rows render with titles/authors/labels; CI/review glyphs blank
t≈2000  ListEnriched{gen=1} → glyphs filled in by merge-by-number
        UI: list.enriching=false
```

## Testing

Unit tests, all without mocks beyond the existing `FakeGh` / `FakeGit`:

- `Cache::insert_partial` zero-fills `commit_stats` for every commit in
  `detail.commits`.
- `Cache::update_diff` is a no-op when the entry's `head_oid` does not
  match (force-push scenario).
- `Cache::add_file_colors` accumulates `commit_stats` across multiple
  calls and is a no-op for missing entries.
- Worker streaming integration: spawn the worker with `FakeGh` + `FakeGit`
  fixtures, send `LoadPr(n)`, drain responses, assert the sequence is
  `PrDetail → PrDiff → PrFileColors*N → PrColorsDone` and that the first
  `PrFileColors` is for `files[0]`.
- Worker list two-phase: send `RefreshList`, assert two responses arrive
  with matching `gen`, and that merging the enriched payload into the
  fast payload produces the original full record.
- Generation drop: send two `RefreshList` calls back-to-back; assert the
  UI's `handle_response` discards the gen-1 enriched response when gen-2
  has already taken over.
- Rendering with skeletal `pkg.files=[]` and `detail.files` populated:
  file bar shows paths and the counter; diff body shows the loading
  placeholder; file picker is interactive.

No mocked subprocesses. All tests drive real `Cache`/`Worker`/`Pkg`
state machines through their public APIs.

## Risks & rollback

- **Risk: `ListEnriched` partial repos** — if `gh` returns fewer PRs in
  the enrichment pass than in the fast pass (state changed during the
  gap), `number`-keyed merge silently leaves the missing rows without
  glyphs. Acceptable: the next refresh corrects it. Worth a debug-log
  line but not a failure.
- **Risk: stale colors after force-push** — `add_file_colors` keys on
  `(number, head_oid)`. If a force-push lands between `PrDetail` and the
  user reopening the PR, the old entry stays in the cache (today's
  behavior) and the new entry gets its own colors. No regression.
- **Risk: many small worker messages** — channel volume goes from
  ~1/PR-load to ~(2 + N+1)/PR-load. Still negligible; the channel is
  unbounded and a typical PR has <50 files.

Rollback: revert is local to `data/worker.rs`, `data/cache.rs`,
`view/pr_review.rs`, `view/pr_list.rs`, and `app.rs`. No data
migration; no schema persisted.
