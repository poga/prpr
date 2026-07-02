# PR list: draft badge + reliable mergeable status

Two independent PR-list fixes.

## Feature 1 — Draft badge

### Goal

Draft PRs are currently distinguished only by a dim hollow `○` state glyph
(vs filled `●`), which is easy to miss. Add an explicit, muted `draft` text
badge so draft status is unmistakable while scanning.

### Change

In `row_for` (`src/view/pr_list.rs`):

- Keep the existing `○` state glyph unchanged.
- When `pr.is_draft`, add a `draft` text span styled `OVERLAY0` (muted),
  placed immediately before the author span. Non-draft PRs get nothing.
- Add the badge's rendered width to `right_cols` so the title budget stays
  correct and long titles still truncate cleanly.

The badge reads as secondary (dim), not a label — it must not be styled like
the `[label]` pill.

### Testing

One new test in the `mod tests` block of `src/view/pr_list.rs`:

`draft_pr_shows_draft_badge`:
- Render a row for a `Pr` with `is_draft: true` and assert the buffer
  contains `draft`.
- Render a row for a `Pr` with `is_draft: false` and assert it does not.

## Feature 2 — Reliable mergeable status

### Goal

The only mergeable signal today is the `⚠` conflict marker, driven by
`is_conflicting()`, which is true **only** for the exact string
`"CONFLICTING"`. GitHub computes mergeability lazily and often returns
`"UNKNOWN"` on the first query (and again on later queries if nothing
retriggered computation). We silently treat `"UNKNOWN"` — and `None` before
enrichment lands — as "no conflict", so a genuinely-conflicting PR can render
a clean row, and nothing ever re-polls to resolve the unknown.

Fix: represent mergeability as a three-state value, render a distinct
`?` "checking" marker while unknown, and re-poll until GitHub resolves it —
mirroring GitHub's own "Checking mergeability…" behavior.

### Model (`src/data/pr.rs`)

Keep `mergeable: Option<String>` as the raw wire value (serde untouched).
Add:

```rust
pub enum MergeState { Mergeable, Conflicting, Unknown }

impl Pr {
    pub fn merge_state(&self) -> Option<MergeState> {
        match self.mergeable.as_deref() {
            Some("MERGEABLE")   => Some(MergeState::Mergeable),
            Some("CONFLICTING") => Some(MergeState::Conflicting),
            Some(_)             => Some(MergeState::Unknown), // "UNKNOWN" / unexpected
            None                => None,                      // not fetched yet
        }
    }
}
```

`None` means "not fetched yet" → renders blank (no flash of `?` on cold load).
`Some(Unknown)` means "GitHub asked, doesn't know yet" → renders `?`.
`is_conflicting()` is rewritten to delegate:
`matches!(self.merge_state(), Some(MergeState::Conflicting))`.

### Render (`row_for`)

The conflict marker slot becomes, for OPEN PRs only:

- `Some(Conflicting)` → `⚠` (red, unchanged).
- `Some(Unknown)` → `?` (dim, e.g. `OVERLAY0`).
- `Some(Mergeable)` / `None` → blank.

Non-open PRs keep a blank slot (stale mergeability isn't actionable).

Extend the footer legend to document the new glyph, e.g. `⚠conflict ?checking`.

### Re-poll (`src/data/worker.rs`)

The `RefreshList` handler already fires enrichment on a detached thread. Turn
that single call into a bounded retry loop:

1. Fetch `list_prs_enriched`; emit `ListEnriched { generation, result }`
   immediately (so CI / review glyphs land fast, unknown rows show `?`).
2. If the result is `Ok` and any row's `mergeable` is `"UNKNOWN"`, sleep the
   retry delay, re-fetch, and emit another `ListEnriched` with the **same**
   `generation`. Repeat until no row is `"UNKNOWN"` or a max round count is
   reached.

Each emission merges via the existing `apply_enrichment` in the `ListEnriched`
handler, so `?` rows resolve to `⚠`/blank in place. Emitting multiple
`ListEnriched` for one generation is idempotent for the `enriching` /
`in_flight` flags (they're set false each time). Rounds finish well within the
30s auto-refresh window, so no generation overlap in practice; if GitHub stays
slow past the round budget, `?` simply persists until the next refresh —
honest, never false-clean.

### Plumbing

- `Worker::spawn` gains a retry `Duration` parameter (alongside `window_size`).
  Production passes ~2s; the max round count is a small module constant (~3).
  Tests pass a tiny delay (~50ms) so they stay fast.
- `FakeGh` (`src/data/gh.rs`) gains an optional enrichment **sequence**:
  successive `list_prs_enriched` calls pop successive payloads, falling back to
  the existing single `enrichments` vec when the sequence is empty/exhausted.
  This lets a test return `UNKNOWN` first and `CONFLICTING` next.

### Testing

`src/data/pr.rs`:
- `merge_state_maps_wire_values`: assert `None`→`None`,
  `"MERGEABLE"`→`Some(Mergeable)`, `"CONFLICTING"`→`Some(Conflicting)`,
  `"UNKNOWN"`→`Some(Unknown)`. Assert `is_conflicting()` matches.

`src/view/pr_list.rs`:
- `unknown_mergeable_open_pr_shows_checking_marker`: an OPEN PR with
  `mergeable: Some("UNKNOWN")` renders `?`; with `Some("CONFLICTING")` renders
  `⚠`; with `Some("MERGEABLE")` renders neither.

`src/data/worker.rs`:
- `enrichment_repolls_until_mergeable_resolves`: drive a `Worker` with a
  `FakeGh` whose enrichment sequence is `[UNKNOWN, CONFLICTING]` and a ~50ms
  retry delay. Poll the response channel until deadline; assert **two**
  `ListEnriched` responses arrive for the generation, the first carrying
  `"UNKNOWN"` and a later one carrying `"CONFLICTING"`. Verifies the re-poll
  loop end-to-end against real worker threading (no mocked logic).

Update existing `Worker::spawn` call sites (prod + tests) for the new
parameter.

## Out of scope

- A positive "mergeable ✓" marker — only conflict/checking are actionable;
  a clean row already communicates "fine".
- Per-PR `gh pr view` mergeable fetches — the batch re-poll reuses the
  existing enriched call; per-PR is a fallback only if batch proves
  insufficient in practice.
- Changing the footer legend beyond adding the `?` glyph meaning.
