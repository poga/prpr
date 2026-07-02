# PR list: draft badge + reliable mergeable status — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make draft PRs unmistakable in the list, and make mergeable status never silently wrong by rendering a `?` "checking" marker and re-polling GitHub until it resolves.

**Architecture:** Feature 1 is a pure render change in `row_for`. Feature 2 adds a tri-state `MergeState` model in `pr.rs`, renders `?`/`⚠` accordingly, and turns the worker's detached enrichment call into a bounded re-poll loop that re-fetches while any row is `"UNKNOWN"`.

**Tech Stack:** Rust, ratatui (TUI), `gh` CLI subprocess, single background worker thread with mpsc channels.

## Global Constraints

- Comments never exceed 1 line (80 chars); keep them minimal, focused on why/what.
- NO MOCKS. Tests exercise real behavior/system interactions (real worker threads, real render buffers).
- Run the full test suite (`cargo test`) as part of each task.
- Commit style mirrors the repo: `feat(scope): …` / `fix(scope): …`.
- `OVERLAY0`, `DIFF_DEL_FG`, `COMMIT_PALETTE`, `TEXT` etc. are already in scope in `pr_list.rs` via `use crate::render::style::*;`.

---

### Task 1: Draft text badge in the PR row

**Files:**
- Modify: `src/view/pr_list.rs` (`row_for`, ~lines 262-301; new test in `mod tests`)

**Interfaces:**
- Consumes: `Pr::is_draft: bool` (already fetched in the fast pass).
- Produces: nothing new; visual only.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/view/pr_list.rs`:

```rust
#[test]
fn draft_pr_shows_draft_badge() {
    let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
    let render_all = |st: &PrListState| {
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, st, now); }).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height).map(|y| buffer_line(buf, y)).collect::<Vec<_>>().join("\n")
    };

    let mut st = fixture_state();
    st.prs[0].is_draft = true;
    assert!(render_all(&st).contains("draft"), "draft PR should show the 'draft' badge");

    let mut st2 = fixture_state();
    for p in &mut st2.prs { p.is_draft = false; }
    assert!(!render_all(&st2).contains("draft"), "non-draft rows must not show the badge");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib draft_pr_shows_draft_badge`
Expected: FAIL — the assertion `render_all(&st).contains("draft")` is false (no badge rendered yet).

- [ ] **Step 3: Implement the badge in `row_for`**

In `src/view/pr_list.rs`, find the block that builds the right-hand spans (after `let author_str = ...`). Add the draft badge string:

```rust
    let author_str = format!("{} ", pr.author.login);
    // Muted draft badge, secondary to the [label] pill.
    let draft_str = if pr.is_draft { "draft  ".to_string() } else { String::new() };
    let age = format!(
        "c{} · u{}",
        humanize_age(pr.created_at, now),
        humanize_age(pr.updated_at, now),
    );
```

Update `right_cols` to include the badge width:

```rust
    let right_cols = label_str.chars().count()
        + draft_str.chars().count()
        + author_str.chars().count()
        + age.chars().count();
```

Insert the badge span between the label and author spans in the returned `Line`:

```rust
        Span::styled(label_str, row_bg.fg(COMMIT_PALETTE[4])),
        Span::styled(draft_str, row_bg.fg(OVERLAY0)),
        Span::styled(author_str, row_bg.fg(COMMIT_PALETTE[0])),
        Span::styled(age, row_bg.fg(OVERLAY0)),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS — including `draft_pr_shows_draft_badge`; no existing test regresses.

- [ ] **Step 5: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): add muted 'draft' badge to draft PR rows"
```

---

### Task 2: `MergeState` tri-state model

**Files:**
- Modify: `src/data/pr.rs` (add enum near `CiState`; rewrite `is_conflicting`; add `merge_state`; new test)

**Interfaces:**
- Produces:
  - `pub enum MergeState { Mergeable, Conflicting, Unknown }` (derives `Debug, Clone, Copy, PartialEq, Eq`).
  - `Pr::merge_state(&self) -> Option<MergeState>` — `None` = not fetched, `Some(Unknown)` = GitHub still computing.
  - `Pr::is_conflicting(&self) -> bool` — now delegates to `merge_state`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/data/pr.rs`:

```rust
#[test]
fn merge_state_maps_wire_values() {
    let pr_with = |m: Option<&str>| Pr {
        number: 1, title: "t".into(), is_draft: false, state: PrState::Open,
        author: Author { login: "a".into() },
        created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        base_ref_name: String::new(), head_ref_name: String::new(),
        labels: vec![], status_check_rollup: vec![],
        review_decision: None, mergeable: m.map(str::to_string),
    };
    assert_eq!(pr_with(None).merge_state(), None);
    assert_eq!(pr_with(Some("MERGEABLE")).merge_state(), Some(MergeState::Mergeable));
    assert_eq!(pr_with(Some("CONFLICTING")).merge_state(), Some(MergeState::Conflicting));
    assert_eq!(pr_with(Some("UNKNOWN")).merge_state(), Some(MergeState::Unknown));
    assert_eq!(pr_with(Some("WEIRD")).merge_state(), Some(MergeState::Unknown));
    assert!(pr_with(Some("CONFLICTING")).is_conflicting());
    assert!(!pr_with(Some("UNKNOWN")).is_conflicting());
    assert!(!pr_with(None).is_conflicting());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib merge_state_maps_wire_values`
Expected: FAIL to compile — `MergeState` and `Pr::merge_state` do not exist yet.

- [ ] **Step 3: Add the enum and methods**

In `src/data/pr.rs`, add the enum next to `CiState`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeState {
    Mergeable,
    Conflicting,
    Unknown,
}
```

Replace the existing `is_conflicting` method (in `impl Pr`) with:

```rust
    /// Tri-state mergeability from the raw wire value. `None` = not yet
    /// fetched; `Unknown` = GitHub is still computing.
    pub fn merge_state(&self) -> Option<MergeState> {
        match self.mergeable.as_deref() {
            Some("MERGEABLE") => Some(MergeState::Mergeable),
            Some("CONFLICTING") => Some(MergeState::Conflicting),
            Some(_) => Some(MergeState::Unknown),
            None => None,
        }
    }

    /// True only when GitHub reports a definite conflict.
    pub fn is_conflicting(&self) -> bool {
        matches!(self.merge_state(), Some(MergeState::Conflicting))
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS — `merge_state_maps_wire_values` passes; the existing `parses_pr_list_fixture` (which asserts `is_conflicting()`) still passes.

- [ ] **Step 5: Commit**

```bash
git add src/data/pr.rs
git commit -m "feat(pr): add tri-state MergeState and merge_state accessor"
```

---

### Task 3: Render `?` checking / `⚠` conflict markers + legend

**Files:**
- Modify: `src/view/pr_list.rs` (import, `conflict_glyph` in `row_for`, footer legend; new test)

**Interfaces:**
- Consumes: `Pr::merge_state()` and `MergeState` from Task 2.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/view/pr_list.rs`:

```rust
#[test]
fn unknown_mergeable_open_pr_shows_checking_marker() {
    let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
    let mk = |m: &str| Pr {
        number: 1, title: "t".into(), is_draft: false, state: PrState::Open,
        author: crate::data::pr::Author { login: "a".into() },
        created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
        base_ref_name: "main".into(), head_ref_name: "f".into(),
        labels: vec![], status_check_rollup: vec![],
        review_decision: None, mergeable: Some(m.into()),
    };
    let has = |line: &Line, glyph: &str| line.spans.iter().any(|s| s.content == glyph);

    let unknown = row_for(&mk("UNKNOWN"), false, now, 80);
    assert!(has(&unknown, "?"), "UNKNOWN row should show '?'");
    assert!(!has(&unknown, "⚠"), "UNKNOWN row must not show '⚠'");

    let conflicting = row_for(&mk("CONFLICTING"), false, now, 80);
    assert!(has(&conflicting, "⚠"), "CONFLICTING row should show '⚠'");

    let mergeable = row_for(&mk("MERGEABLE"), false, now, 80);
    assert!(!has(&mergeable, "?"), "MERGEABLE row must not show '?'");
    assert!(!has(&mergeable, "⚠"), "MERGEABLE row must not show '⚠'");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib unknown_mergeable_open_pr_shows_checking_marker`
Expected: FAIL — the `UNKNOWN` row shows a blank slot, so `has(&unknown, "?")` is false.

- [ ] **Step 3: Implement the render change**

In `src/view/pr_list.rs`, extend the pr import:

```rust
use crate::data::pr::{CiState, MergeState, Pr, PrState, ReviewDecision};
```

Replace the `conflict_glyph` block in `row_for`:

```rust
    // Merge marker for OPEN PRs only; stale mergeability isn't actionable.
    let conflict_glyph = if pr.state == PrState::Open {
        match pr.merge_state() {
            Some(MergeState::Conflicting) => Span::styled("⚠", Style::default().fg(DIFF_DEL_FG)),
            Some(MergeState::Unknown) => Span::styled("?", Style::default().fg(OVERLAY0)),
            _ => Span::styled(" ", Style::default()),
        }
    } else {
        Span::styled(" ", Style::default())
    };
```

In `render_footer`, extend the legend string to document the new glyph:

```rust
                "  state ●open ○draft   ci ✓pass ✗fail …pend   review ✓approved !changes ·pending   ⚠conflict ?checking",
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS — new test passes; existing render tests unaffected.

- [ ] **Step 5: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): show '?' checking marker for unknown mergeability"
```

---

### Task 4: Worker re-polls until mergeable resolves

**Files:**
- Modify: `src/data/gh.rs` (`FakeGh`: enrichment sequence + setter + sequential `list_prs_enriched`)
- Modify: `src/data/worker.rs` (import `Duration`; retry consts; `Worker::spawn_with_retry`; `run_worker` param; re-poll loop; new test)

**Interfaces:**
- Consumes: `GhClient::list_prs_enriched` (existing).
- Produces:
  - `Worker::spawn_with_retry(repo_root, gh, git, window_size, enrich_retry_delay: Duration) -> Worker`.
  - `Worker::spawn(...)` keeps its 4-arg signature and delegates with the prod default, so the 8 existing call sites stay unchanged.
  - `FakeGh::set_enrichment_sequence(&self, seq: Vec<Vec<PrEnrichment>>)` — successive `list_prs_enriched` calls pop successive payloads; falls back to `enrichments` when exhausted.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/data/worker.rs`:

```rust
#[test]
fn enrichment_repolls_until_mergeable_resolves() {
    use crate::data::pr::{Author, Pr, PrEnrichment, PrState};

    let mk_enr = |m: &str| PrEnrichment {
        number: 7, status_check_rollup: vec![], review_decision: None,
        mergeable: Some(m.into()),
    };
    let gh = FakeGh::new();
    // First enriched fetch is UNKNOWN → worker must re-poll; second resolves.
    gh.set_enrichment_sequence(vec![vec![mk_enr("UNKNOWN")], vec![mk_enr("CONFLICTING")]]);

    let git = FakeGit::new("/tmp/repo");
    let worker = Worker::spawn_with_retry(
        "/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7, std::time::Duration::from_millis(50),
    );
    worker.send(Request::RefreshList { generation: 1 });

    let mut mergeables: Vec<Option<String>> = vec![];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Response::ListEnriched { generation: 1, result: Ok(es) }) => {
                mergeables.push(es.first().and_then(|e| e.mergeable.clone()));
                if mergeables.iter().any(|m| m.as_deref() == Some("CONFLICTING")) { break; }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert_eq!(
        mergeables.first(), Some(&Some("UNKNOWN".into())),
        "first ListEnriched should carry UNKNOWN; got {mergeables:?}"
    );
    assert!(
        mergeables.iter().any(|m| m.as_deref() == Some("CONFLICTING")),
        "re-poll should eventually deliver CONFLICTING; got {mergeables:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib enrichment_repolls_until_mergeable_resolves`
Expected: FAIL to compile — `FakeGh::set_enrichment_sequence` and `Worker::spawn_with_retry` do not exist yet.

- [ ] **Step 3: Extend `FakeGh` with an enrichment sequence**

In `src/data/gh.rs`, in the `fakes` module, add `use std::collections::VecDeque;` alongside the existing `use std::sync::Mutex;`. Update the struct, `new`, add a setter, and make `list_prs_enriched` sequential:

```rust
    pub struct FakeGh {
        pub prs_fast: Vec<Pr>,
        pub enrichments: Vec<PrEnrichment>,
        pub enrichment_sequence: Mutex<VecDeque<Vec<PrEnrichment>>>,
        pub merges: Mutex<Vec<(u32, String)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self {
                prs_fast: vec![],
                enrichments: vec![],
                enrichment_sequence: Mutex::new(VecDeque::new()),
                merges: Mutex::new(vec![]),
            }
        }
        /// Queue successive enriched payloads; each call pops the next one.
        pub fn set_enrichment_sequence(&self, seq: Vec<Vec<PrEnrichment>>) {
            *self.enrichment_sequence.lock().unwrap() = seq.into();
        }
    }
```

In `impl GhClient for FakeGh`, replace `list_prs_enriched`:

```rust
        fn list_prs_enriched(&self, _root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
            if let Some(next) = self.enrichment_sequence.lock().unwrap().pop_front() {
                return Ok(next);
            }
            Ok(self.enrichments.clone())
        }
```

- [ ] **Step 4: Add retry plumbing + re-poll loop in the worker**

In `src/data/worker.rs`, add `use std::time::Duration;` to the imports, and the retry policy consts near the top of the module:

```rust
/// GitHub computes mergeability lazily; the enriched pass often returns
/// "UNKNOWN" until a background job finishes. Re-poll to resolve it.
const ENRICH_MAX_ROUNDS: usize = 3;
const ENRICH_RETRY_DELAY: Duration = Duration::from_secs(2);
```

Refactor `Worker::spawn` to delegate, adding `spawn_with_retry`:

```rust
    pub fn spawn(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
    ) -> Self {
        Self::spawn_with_retry(repo_root, gh, git, window_size, ENRICH_RETRY_DELAY)
    }

    pub fn spawn_with_retry(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
        enrich_retry_delay: Duration,
    ) -> Self {
        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        let handle = thread::spawn(move || {
            run_worker(req_rx, res_tx, repo_root, gh, git, window_size, enrich_retry_delay);
        });
        Self { tx: Some(req_tx), rx: res_rx, handle: Some(handle) }
    }
```

Add the `enrich_retry_delay: Duration` parameter to the `run_worker` signature:

```rust
fn run_worker(
    req_rx: Receiver<Request>,
    res_tx: Sender<Response>,
    repo_root: PathBuf,
    gh: Arc<dyn GhClient>,
    git: Arc<dyn GitClient>,
    window_size: usize,
    enrich_retry_delay: Duration,
) {
```

Replace the detached enrichment thread inside `Request::RefreshList { generation }` with the bounded re-poll loop:

```rust
                let gh_enr = Arc::clone(&gh);
                let repo_enr = repo_root.clone();
                let tx_enr = res_tx.clone();
                let gen_enr = generation;
                let retry_delay = enrich_retry_delay;
                thread::spawn(move || {
                    let mut round = 0usize;
                    loop {
                        let result = gh_enr.list_prs_enriched(&repo_enr);
                        let has_unknown = matches!(
                            &result,
                            Ok(es) if es.iter().any(|e| e.mergeable.as_deref() == Some("UNKNOWN"))
                        );
                        if tx_enr
                            .send(Response::ListEnriched { generation: gen_enr, result })
                            .is_err()
                        {
                            return;
                        }
                        round += 1;
                        if !has_unknown || round == ENRICH_MAX_ROUNDS {
                            break;
                        }
                        thread::sleep(retry_delay);
                    }
                });
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS — `enrichment_repolls_until_mergeable_resolves` passes; existing worker tests (e.g. `worker_emits_list_fast_then_enriched_with_matching_gen`) still pass and stay fast (their enrichment has no `"UNKNOWN"`, so no sleep).

- [ ] **Step 6: Commit**

```bash
git add src/data/gh.rs src/data/worker.rs
git commit -m "feat(worker): re-poll enriched pass until mergeability resolves"
```

---

## Final verification

- [ ] Run the full suite once more: `cargo test`
- [ ] Run `cargo clippy --all-targets` and confirm no new warnings.
- [ ] Confirm all four commits are on the branch.

## Notes / deliberate refinements vs the spec

- The spec said "update existing `Worker::spawn` call sites for the new parameter." Instead, `spawn` keeps its signature and delegates to `spawn_with_retry` with the prod default. This avoids churning 8 call sites (7 tests + `App::new`) and keeps the prod path untouched — the retry-injection point exists solely for the new test.
