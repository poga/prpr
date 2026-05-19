# Incremental fetching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make prpr feel fast by streaming PR-list and PR-review data in
stages so important content appears immediately and slower data fills in
without blocking the UI.

**Architecture:** Worker emits a sequence of fine-grained `Response`
events per request (two-phase list; staged PR-review with per-file
blame). The `Cache` gains mutators for partial PR packages. The UI
renders whatever is in the cache, naturally degrading when fields are
absent (file list from `detail.files` while diff is still loading,
uncolored diff while blame is still streaming).

**Tech Stack:** Rust 2024, ratatui, anyhow, chrono, serde — no new deps.

---

## File map

- `src/data/pr.rs` — add `PrEnrichment` type and `Pr::apply_enrichment`
- `src/data/gh.rs` — replace `list_prs` with `list_prs_fast` +
  `list_prs_enriched`
- `src/data/cache.rs` — add `insert_partial`, `update_diff`,
  `add_file_colors`
- `src/data/worker.rs` — new `Request::RefreshList { gen }`, replace
  `Response::ListLoaded`/`PrLoaded` with granular variants, rewrite
  worker loop to emit events
- `src/app.rs` — `list_gen`, generation-aware `handle_response`, file-count
  helpers, navigation bounds use `detail.files` when `pkg.files` is empty
- `src/view/pr_list.rs` — add `enriching` flag, footer surface
- `src/view/pr_review.rs` — file bar and diff body fall back to
  `pkg.detail.files` while `pkg.files` is empty; status-line states
- `src/data/cache.rs` (helpers) — `PrPackage::file_paths()`,
  `PrPackage::file_count()`

---

### Task 1: `PrEnrichment` type and `Pr::apply_enrichment`

**Files:**
- Modify: `src/data/pr.rs`

A small value type for the enrichment-pass payload, plus a method that
merges its fields into an existing `Pr`. Enrichment carries only the
heavy-fetch fields so the JSON request for the second `gh pr list` call
can be minimal.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/data/pr.rs`:

```rust
    #[test]
    fn parses_enrichment_with_minimal_fields() {
        let json = r#"[{
            "number": 7,
            "statusCheckRollup": [{"status":"COMPLETED","conclusion":"FAILURE"}],
            "reviewDecision": "APPROVED",
            "mergeable": "CONFLICTING"
        }]"#;
        let v: Vec<PrEnrichment> = serde_json::from_str(json).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].number, 7);
        assert_eq!(v[0].status_check_rollup.len(), 1);
        assert_eq!(v[0].review_decision, Some(ReviewDecision::Approved));
        assert_eq!(v[0].mergeable.as_deref(), Some("CONFLICTING"));
    }

    #[test]
    fn enrichment_empty_review_decision_is_none() {
        let json = r#"{"number":1,"reviewDecision":""}"#;
        let e: PrEnrichment = serde_json::from_str(json).unwrap();
        assert_eq!(e.review_decision, None);
    }

    #[test]
    fn apply_enrichment_overwrites_heavy_fields_only() {
        let mut p = Pr {
            number: 7,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![Label { name: "bug".into() }],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        };
        let e = PrEnrichment {
            number: 7,
            status_check_rollup: vec![StatusCheck {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
            }],
            review_decision: Some(ReviewDecision::Approved),
            mergeable: Some("MERGEABLE".into()),
        };
        p.apply_enrichment(&e);
        assert_eq!(p.status_check_rollup.len(), 1);
        assert_eq!(p.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(p.mergeable.as_deref(), Some("MERGEABLE"));
        // light fields untouched
        assert_eq!(p.title, "t");
        assert_eq!(p.labels.len(), 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib data::pr -- --nocapture`
Expected: compile error — `PrEnrichment` and `apply_enrichment` are undefined.

- [ ] **Step 3: Add `PrEnrichment` and `apply_enrichment`**

Append to `src/data/pr.rs`, after the `FileMeta` struct:

```rust
/// Heavy-fetch fields returned by the second `gh pr list` pass. Used to
/// enrich an existing `Pr` produced by the fast pass.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PrEnrichment {
    pub number: u32,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<StatusCheck>,
    #[serde(
        rename = "reviewDecision",
        default,
        deserialize_with = "deser_review_decision"
    )]
    pub review_decision: Option<ReviewDecision>,
    #[serde(default)]
    pub mergeable: Option<String>,
}

impl Pr {
    /// Copy the heavy-fetch fields from `e` into `self`. Light fields
    /// (title, author, dates, labels, state) are left untouched.
    pub fn apply_enrichment(&mut self, e: &PrEnrichment) {
        self.status_check_rollup = e.status_check_rollup.clone();
        self.review_decision = e.review_decision;
        self.mergeable = e.mergeable.clone();
    }
}
```

The existing `impl Pr { ... }` block already exists for `is_conflicting`
and `ci_state` — add `apply_enrichment` to that block rather than
opening a second one if you prefer. Either works; pick one.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib data::pr -- --nocapture`
Expected: all `data::pr` tests pass, including the 3 new ones.

- [ ] **Step 5: Commit**

```bash
git add src/data/pr.rs
git commit -m "feat(pr): add PrEnrichment type and apply_enrichment"
```

---

### Task 2: Replace `list_prs` with fast + enriched pair on `GhClient`

**Files:**
- Modify: `src/data/gh.rs`

Two trait methods replace the single existing one. The fast call asks
for the minimum fields needed to render rows; the enriched call asks for
just the heavy fields plus `number` (the merge key). `GhCli` implements
both via `gh pr list --json …`; `FakeGh` is updated to return canned
lists for each.

- [ ] **Step 1: Write the failing tests**

Replace the existing `#[cfg(test)] mod tests` block in `src/data/gh.rs`
(keep `fixture_view_round_trips_committed_date`) and add:

```rust
    #[test]
    fn fake_returns_separate_fast_and_enriched_payloads() {
        use super::fakes::FakeGh;
        use crate::data::pr::{Author, Label, Pr, PrEnrichment, PrState, StatusCheck};
        let mut fake = FakeGh::new();
        fake.prs_fast = vec![Pr {
            number: 7,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![Label { name: "bug".into() }],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }];
        fake.enrichments = vec![PrEnrichment {
            number: 7,
            status_check_rollup: vec![StatusCheck {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
            }],
            review_decision: None,
            mergeable: Some("MERGEABLE".into()),
        }];
        let fast = fake.list_prs_fast(std::path::Path::new("/x")).unwrap();
        assert_eq!(fast.len(), 1);
        assert!(fast[0].status_check_rollup.is_empty());
        let enriched = fake.list_prs_enriched(std::path::Path::new("/x")).unwrap();
        assert_eq!(enriched.len(), 1);
        assert_eq!(enriched[0].number, 7);
        assert_eq!(enriched[0].status_check_rollup.len(), 1);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib data::gh -- --nocapture`
Expected: compile error — `list_prs_fast`, `list_prs_enriched`, and the
`prs_fast` / `enrichments` fields don't exist.

- [ ] **Step 3: Update the trait and `GhCli` impl**

Replace the top of `src/data/gh.rs` (everything down to the end of
`impl GhClient for GhCli`) with:

```rust
//! `gh` CLI subprocess wrappers. The `GhClient` trait is what the cache
//! depends on; tests substitute a fake. The production binary uses
//! `GhCli`, which shells out to `gh`.

use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};

use crate::data::pr::{Pr, PrDetail, PrEnrichment};

pub trait GhClient: Send + Sync {
    /// First pass: light fields, no `statusCheckRollup`/`mergeable`/`reviewDecision`.
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>>;
    /// Second pass: only the heavy fields, keyed by `number` for merge.
    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>>;
    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail>;
    fn diff_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<String>;
    /// `method` is one of "merge", "squash", "rebase".
    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()>;
}

pub struct GhCli;

const PR_LIST_FAST_FIELDS: &str =
    "number,title,author,isDraft,state,createdAt,updatedAt,labels";
const PR_LIST_ENRICHED_FIELDS: &str =
    "number,statusCheckRollup,reviewDecision,mergeable";
const PR_VIEW_FIELDS: &str = "number,title,author,isDraft,state,createdAt,baseRefName,baseRefOid,headRefName,headRefOid,mergeable,labels,statusCheckRollup,reviewDecision,commits,files";

fn run(cmd: &mut Command) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn: {cmd:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(anyhow!("gh exited with {}: {}", out.status, stderr.trim()));
    }
    Ok(out)
}

impl GhClient for GhCli {
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "all",
            "--json",
            PR_LIST_FAST_FIELDS,
        ]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (fast) output")?;
        Ok(prs)
    }

    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "all",
            "--json",
            PR_LIST_ENRICHED_FIELDS,
        ]))?;
        let v: Vec<PrEnrichment> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (enriched) output")?;
        Ok(v)
    }

    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail> {
        let n = number.to_string();
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "view",
            &n,
            "--json",
            PR_VIEW_FIELDS,
        ]))?;
        let pr: PrDetail = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr view --json` output")?;
        Ok(pr)
    }

    fn diff_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<String> {
        let n = number.to_string();
        let out = run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "diff", &n]))?;
        let s = String::from_utf8(out.stdout)
            .with_context(|| "`gh pr diff` produced non-UTF-8 output")?;
        Ok(s)
    }

    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()> {
        let n = number.to_string();
        let flag = match method {
            "merge" => "--merge",
            "squash" => "--squash",
            "rebase" => "--rebase",
            other => return Err(anyhow!("unknown merge method: {other}")),
        };
        run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "merge", &n, flag]))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Update `FakeGh`**

Replace the existing `pub(crate) mod fakes` block in `src/data/gh.rs`
with:

```rust
#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use crate::data::pr::PrEnrichment;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory fake. Tests load JSON fixtures and stuff them into this.
    pub struct FakeGh {
        pub prs_fast: Vec<Pr>,
        pub enrichments: Vec<PrEnrichment>,
        pub views: HashMap<u32, PrDetail>,
        pub diffs: HashMap<u32, String>,
        pub merges: Mutex<Vec<(u32, String)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self {
                prs_fast: vec![],
                enrichments: vec![],
                views: HashMap::new(),
                diffs: HashMap::new(),
                merges: Mutex::new(vec![]),
            }
        }
    }

    impl GhClient for FakeGh {
        fn list_prs_fast(&self, _root: &std::path::Path) -> Result<Vec<Pr>> {
            Ok(self.prs_fast.clone())
        }
        fn list_prs_enriched(&self, _root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
            Ok(self.enrichments.clone())
        }
        fn view_pr(&self, _root: &std::path::Path, n: u32) -> Result<PrDetail> {
            self.views
                .get(&n)
                .cloned()
                .ok_or_else(|| anyhow!("no fake view for #{n}"))
        }
        fn diff_pr(&self, _root: &std::path::Path, n: u32) -> Result<String> {
            self.diffs
                .get(&n)
                .cloned()
                .ok_or_else(|| anyhow!("no fake diff for #{n}"))
        }
        fn merge_pr(&self, _root: &std::path::Path, n: u32, m: &str) -> Result<()> {
            self.merges.lock().unwrap().push((n, m.to_string()));
            Ok(())
        }
    }
}
```

- [ ] **Step 5: Build will fail in `worker.rs` and `worker_round_trip` test**

Run: `cargo build 2>&1 | head -50`
Expected: errors in `src/data/worker.rs` (calls `gh.list_prs(...)`) and
its test. These get fixed in Task 4. **Do not commit yet** — go straight
to step 6.

- [ ] **Step 6: Stub out the worker's old call site to keep the build green for this task**

To allow a clean commit per task, temporarily change the call in
`src/data/worker.rs::run_worker`:

```rust
Request::RefreshList => Response::ListLoaded(gh.list_prs_fast(&repo_root)),
```

(was `gh.list_prs(&repo_root)`). And in the `worker_round_trip` test
near the bottom of `src/data/worker.rs`, change the fixture setup from:

```rust
        gh.prs = {
            let json = include_str!("../../tests/fixtures/pr_list.json");
            serde_json::from_str(json).unwrap()
        };
```

to:

```rust
        gh.prs_fast = {
            let json = include_str!("../../tests/fixtures/pr_list.json");
            serde_json::from_str(json).unwrap()
        };
```

Now run: `cargo test 2>&1 | tail -10`
Expected: all 114+3 tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/data/gh.rs src/data/worker.rs
git commit -m "feat(gh): split list_prs into fast and enriched passes"
```

---

### Task 3: `Cache::insert_partial`, `update_diff`, `add_file_colors`

**Files:**
- Modify: `src/data/cache.rs`

Three mutators that operate on the existing `PrPackage` shape. The
renderer tolerates partial `colors` already; these methods just keep
inserting/updating cleanly without throwing away earlier state.

- [ ] **Step 1: Write the failing tests**

Replace the `#[cfg(test)] mod tests` block in `src/data/cache.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::diff::{DiffLine, DiffOp, FileDiff};
    use crate::render::attribution::{CommitStats, LineColors};
    use pretty_assertions::assert_eq;

    fn fixture_detail(head_oid: &str) -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let mut d: PrDetail = serde_json::from_str(json).unwrap();
        d.head_ref_oid = head_oid.into();
        d
    }

    #[test]
    fn set_list_replaces_old_value() {
        let mut cache = Cache::new();
        assert!(cache.list.is_none());
        cache.set_list(vec![]);
        assert_eq!(cache.list.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn insert_partial_zero_fills_commit_stats_for_every_pr_commit() {
        let mut cache = Cache::new();
        let detail = fixture_detail("aaaa");
        let commit_count = detail.commits.len();
        let number = detail.number;
        cache.insert_partial(detail);
        let pkg = cache.get(number).unwrap();
        assert!(pkg.files.is_empty());
        assert!(pkg.colors.is_empty());
        assert_eq!(pkg.commit_stats.len(), commit_count);
        for s in pkg.commit_stats.values() {
            assert_eq!(s.adds, 0);
            assert_eq!(s.dels, 0);
        }
    }

    #[test]
    fn update_diff_swaps_files_when_head_oid_matches() {
        let mut cache = Cache::new();
        let detail = fixture_detail("head1");
        let number = detail.number;
        cache.insert_partial(detail);
        let files = vec![FileDiff {
            path: "a.rs".into(),
            lines: vec![],
            binary: false,
        }];
        cache.update_diff(number, "head1", files.clone());
        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.files.len(), 1);
        assert_eq!(pkg.files[0].path, "a.rs");
    }

    #[test]
    fn update_diff_is_noop_when_head_oid_does_not_match() {
        let mut cache = Cache::new();
        let detail = fixture_detail("head1");
        let number = detail.number;
        cache.insert_partial(detail);
        cache.update_diff(number, "different", vec![FileDiff {
            path: "a.rs".into(),
            lines: vec![],
            binary: false,
        }]);
        let pkg = cache.get(number).unwrap();
        assert!(pkg.files.is_empty());
    }

    #[test]
    fn add_file_colors_accumulates_stats_across_calls() {
        let mut cache = Cache::new();
        let detail = fixture_detail("h");
        let number = detail.number;
        let first_sha = detail.commits[0].oid.clone();
        cache.insert_partial(detail);

        let lc1 = LineColors {
            head: vec![],
            delete: HashMap::new(),
        };
        let mut s1 = HashMap::new();
        s1.insert(first_sha.clone(), CommitStats { adds: 3, dels: 1 });
        cache.add_file_colors(number, "h", "a.rs".into(), lc1, s1);

        let lc2 = LineColors {
            head: vec![],
            delete: HashMap::new(),
        };
        let mut s2 = HashMap::new();
        s2.insert(first_sha.clone(), CommitStats { adds: 5, dels: 2 });
        cache.add_file_colors(number, "h", "b.rs".into(), lc2, s2);

        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.colors.len(), 2);
        let stats = pkg.commit_stats.get(&first_sha).copied().unwrap();
        assert_eq!(stats.adds, 8);
        assert_eq!(stats.dels, 3);
    }

    #[test]
    fn add_file_colors_is_noop_when_entry_missing() {
        let mut cache = Cache::new();
        // No insert_partial called.
        let mut stats = HashMap::new();
        stats.insert("x".to_string(), CommitStats { adds: 1, dels: 0 });
        cache.add_file_colors(
            999,
            "h",
            "a.rs".into(),
            LineColors {
                head: vec![],
                delete: HashMap::new(),
            },
            stats,
        );
        assert!(cache.get(999).is_none());
    }

    #[test]
    fn get_returns_none_for_unknown_pr() {
        let cache = Cache::new();
        assert!(cache.get(999).is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib data::cache -- --nocapture`
Expected: compile errors — `insert_partial`, `update_diff`,
`add_file_colors` don't exist.

- [ ] **Step 3: Add the three mutators**

Replace the `impl Cache` block in `src/data/cache.rs` with:

```rust
impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_list(&mut self, prs: Vec<Pr>) {
        self.list = Some(prs);
    }

    /// Create a skeleton package: detail known, files & colors empty,
    /// `commit_stats` zero-filled for every PR commit.
    pub fn insert_partial(&mut self, detail: PrDetail) {
        let key = (detail.number, detail.head_ref_oid.clone());
        let commit_stats: HashMap<String, CommitStats> = detail
            .commits
            .iter()
            .map(|c| (c.oid.clone(), CommitStats::default()))
            .collect();
        let pkg = PrPackage {
            detail,
            files: vec![],
            colors: HashMap::new(),
            commit_stats,
        };
        self.packages.insert(key, pkg);
    }

    /// Replace the parsed `files` list on an existing partial. No-op if
    /// there is no entry for `(number, head_oid)` — a force-push moved the
    /// PR to a different slot since the diff was requested.
    pub fn update_diff(
        &mut self,
        number: u32,
        head_oid: &str,
        files: Vec<crate::data::diff::FileDiff>,
    ) {
        if let Some(pkg) = self.packages.get_mut(&(number, head_oid.to_string())) {
            pkg.files = files;
        }
    }

    /// Merge one file's colors and accumulate its per-commit stats into
    /// the existing entry. No-op if there is no entry for `(number, head_oid)`.
    pub fn add_file_colors(
        &mut self,
        number: u32,
        head_oid: &str,
        path: String,
        colors: LineColors,
        per_commit: HashMap<String, CommitStats>,
    ) {
        let Some(pkg) = self.packages.get_mut(&(number, head_oid.to_string())) else {
            return;
        };
        pkg.colors.insert(path, colors);
        for (oid, s) in per_commit {
            let entry = pkg.commit_stats.entry(oid).or_default();
            entry.adds += s.adds;
            entry.dels += s.dels;
        }
    }

    /// Look up the most recently-cached entry for `number`, regardless of
    /// `head_sha`. Returns `None` if nothing is cached yet.
    pub fn get(&self, number: u32) -> Option<&PrPackage> {
        self.packages
            .iter()
            .find(|((n, _), _)| *n == number)
            .map(|(_, v)| v)
    }
}
```

Note: this **removes** the existing `pub fn insert(&mut self, pkg:
PrPackage)`. Production callers move to `insert_partial` + the two
mutators (in Task 8). The two prior unit tests that exercised `insert`
(`insert_and_get_round_trip`, `force_push_lands_in_a_fresh_slot`) are
removed in the test block above — their semantics are now covered by
`insert_partial_zero_fills_commit_stats_for_every_pr_commit` and the
behavior is otherwise identical (the `(number, head_oid)` key
preservation is the same).

- [ ] **Step 4: Worker won't compile (it calls `Cache::insert`)**

Run: `cargo build 2>&1 | head -20`
Expected: error in `src/data/worker.rs` or `src/app.rs` if `insert` is
still called. Looking at the codebase, `Cache::insert` is called in
`src/app.rs::handle_response` for `PrLoaded`. Temporarily stub it:

In `src/app.rs`, find the `Response::PrLoaded { number, result: Ok(pkg) }`
arm and change `app.cache.insert(pkg);` to:

```rust
            app.cache.insert_partial(pkg.detail.clone());
            app.cache.update_diff(number, &pkg.detail.head_ref_oid, pkg.files);
            for (path, lc) in pkg.colors {
                app.cache.add_file_colors(
                    number,
                    &pkg.detail.head_ref_oid,
                    path,
                    lc,
                    std::collections::HashMap::new(),
                );
            }
            // commit_stats accumulation will be threaded in Task 8;
            // for now, the renderer renders zeros which is acceptable
            // because LoadPr still completes atomically.
```

This is a temporary shim — Task 7/8 replace it with the streaming
counterparts.

- [ ] **Step 5: Run tests to verify everything passes**

Run: `cargo test 2>&1 | tail -10`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/data/cache.rs src/app.rs
git commit -m "feat(cache): partial PR packages with incremental mutators"
```

---

### Task 4: Worker request/response shape — list two-phase

**Files:**
- Modify: `src/data/worker.rs`
- Modify: `src/app.rs`

Reshape `Request::RefreshList` to carry a generation counter; replace
`Response::ListLoaded` with `ListFast` + `ListEnriched`. The worker
calls fast + enriched sequentially and emits both responses with the
same generation.

- [ ] **Step 1: Write the failing test**

Replace the existing `worker_round_trip` test in `src/data/worker.rs`
with:

```rust
    #[test]
    fn worker_emits_list_fast_then_enriched_with_matching_gen() {
        use crate::data::pr::{Author, Label, Pr, PrEnrichment, PrState, StatusCheck};

        let mut gh = FakeGh::new();
        gh.prs_fast = vec![Pr {
            number: 7,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![Label { name: "bug".into() }],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }];
        gh.enrichments = vec![PrEnrichment {
            number: 7,
            status_check_rollup: vec![StatusCheck {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
            }],
            review_decision: None,
            mergeable: Some("MERGEABLE".into()),
        }];
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);

        worker.send(Request::RefreshList { gen: 42 });

        let resp1 = worker.rx.recv().unwrap();
        match resp1 {
            Response::ListFast { gen: 42, result: Ok(prs) } => {
                assert_eq!(prs.len(), 1);
                assert_eq!(prs[0].number, 7);
            }
            other => panic!("expected ListFast{{gen:42}}, got {:?}", other),
        }

        let resp2 = worker.rx.recv().unwrap();
        match resp2 {
            Response::ListEnriched { gen: 42, result: Ok(e) } => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].number, 7);
                assert_eq!(e[0].status_check_rollup.len(), 1);
            }
            other => panic!("expected ListEnriched{{gen:42}}, got {:?}", other),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib data::worker::tests::worker_emits_list_fast_then_enriched_with_matching_gen -- --nocapture`
Expected: compile error — `Request::RefreshList { gen }` and
`Response::ListFast`/`ListEnriched` don't exist yet.

- [ ] **Step 3: Update `Request` and `Response`**

In `src/data/worker.rs`, replace the `enum Request` and `enum Response`
blocks (and their `#[allow(...)]` attrs) with:

```rust
#[derive(Debug)]
pub enum Request {
    /// Refresh the PR list. `gen` is echoed in both responses so the UI
    /// can drop stale results from a superseded refresh cycle.
    RefreshList { gen: u32 },
    /// Build the streaming PR data set for one PR.
    LoadPr(u32),
    /// Run `gh pr merge <number> --<method>`.
    Merge { number: u32, method: String },
}

// PrPackage-derived variants are larger than the others. The channel
// is low-volume per cycle so the size disparity isn't worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Response {
    ListFast {
        gen: u32,
        result: anyhow::Result<Vec<crate::data::pr::Pr>>,
    },
    ListEnriched {
        gen: u32,
        result: anyhow::Result<Vec<crate::data::pr::PrEnrichment>>,
    },
    /// Granular PR-load events (see worker pipeline).
    PrDetail {
        number: u32,
        result: anyhow::Result<crate::data::pr::PrDetail>,
    },
    PrDiff {
        number: u32,
        result: anyhow::Result<Vec<crate::data::diff::FileDiff>>,
    },
    PrFileColors {
        number: u32,
        head_oid: String,
        path: String,
        colors: crate::render::attribution::LineColors,
        stats: HashMap<String, crate::render::attribution::CommitStats>,
    },
    PrColorsDone {
        number: u32,
        head_oid: String,
    },
    PrLoadError {
        number: u32,
        error: String,
    },
    MergeDone {
        number: u32,
        result: Result<()>,
    },
}
```

- [ ] **Step 4: Update the worker loop for `RefreshList`**

In `src/data/worker.rs::run_worker`, replace the `match req` arms with:

```rust
        match req {
            Request::RefreshList { gen } => {
                let fast = gh.list_prs_fast(&repo_root);
                if res_tx
                    .send(Response::ListFast { gen, result: fast })
                    .is_err()
                {
                    break;
                }
                let enriched = gh.list_prs_enriched(&repo_root);
                if res_tx
                    .send(Response::ListEnriched {
                        gen,
                        result: enriched,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Request::LoadPr(number) => {
                // Streaming pipeline added in Task 7. For now keep the
                // atomic build_package shape so the UI still works:
                let result = build_package(&*gh, &*git, &repo_root, number, window_size);
                match result {
                    Ok(pkg) => {
                        let head = pkg.detail.head_ref_oid.clone();
                        let _ = res_tx.send(Response::PrDetail {
                            number,
                            result: Ok(pkg.detail.clone()),
                        });
                        let _ = res_tx.send(Response::PrDiff {
                            number,
                            result: Ok(pkg.files.clone()),
                        });
                        for (path, lc) in pkg.colors {
                            let _ = res_tx.send(Response::PrFileColors {
                                number,
                                head_oid: head.clone(),
                                path,
                                colors: lc,
                                stats: HashMap::new(),
                            });
                        }
                        let _ = res_tx.send(Response::PrColorsDone {
                            number,
                            head_oid: head,
                        });
                    }
                    Err(e) => {
                        let _ = res_tx.send(Response::PrLoadError {
                            number,
                            error: e.to_string(),
                        });
                    }
                }
            }
            Request::Merge { number, method } => {
                let result = gh.merge_pr(&repo_root, number, &method);
                if res_tx
                    .send(Response::MergeDone { number, result })
                    .is_err()
                {
                    break;
                }
            }
        }
```

Drop the old single `let response = match req { ... };` → `res_tx.send(response)` plumbing — each arm now sends directly.

The proper streaming pipeline replaces the `LoadPr` arm in Task 7.

- [ ] **Step 5: Update `app.rs::handle_response` to consume the new variants**

In `src/app.rs::handle_response`, **replace** the `Response::ListLoaded`
arms with stubs that mirror the old behavior so the rest of the app
keeps working (gen filter is added in Task 5). Drop the old shim from
Task 3 step 4 — `PrDetail` + `PrDiff` + `PrFileColors` now arrive
separately.

Replace the entire body of `handle_response` with:

```rust
fn handle_response(app: &mut App, st: &mut AppState, resp: Response) {
    match resp {
        Response::ListFast { gen: _, result: Ok(prs) } => {
            st.list_refresh_in_flight = false;
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
        Response::ListFast { gen: _, result: Err(e) } => {
            st.list_refresh_in_flight = false;
            st.list.loading = false;
            st.list.status = format!("refresh failed: {e}");
        }
        Response::ListEnriched { gen: _, result: Ok(es) } => {
            // Merge by number. Selection is preserved because rows are
            // mutated, not replaced.
            for e in &es {
                if let Some(p) = st.list.prs.iter_mut().find(|p| p.number == e.number) {
                    p.apply_enrichment(e);
                }
            }
        }
        Response::ListEnriched { gen: _, result: Err(_) } => {
            // Enrichment failure is non-fatal: rows already render with
            // light-fields-only glyphs. Keep silent for now; Task 5 may
            // surface this if we decide it's user-visible.
        }
        Response::PrDetail { number, result: Ok(detail) } => {
            app.cache.insert_partial(detail);
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = "loading diff…".into();
            }
        }
        Response::PrDetail { number, result: Err(e) } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("load failed: {e}");
            }
            st.list.status = format!("load #{number} failed: {e}");
        }
        Response::PrDiff { number, result: Ok(files) } => {
            let head_oid = app
                .cache
                .get(number)
                .map(|p| p.detail.head_ref_oid.clone());
            if let Some(head) = head_oid {
                app.cache.update_diff(number, &head, files);
            }
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
                && let Some(pkg) = app.cache.get(number)
            {
                r.status = format!("coloring {} files…", pkg.files.len());
            }
        }
        Response::PrDiff { number, result: Err(e) } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("diff failed: {e}");
            }
        }
        Response::PrFileColors {
            number,
            head_oid,
            path,
            colors,
            stats,
        } => {
            app.cache.add_file_colors(number, &head_oid, path, colors, stats);
        }
        Response::PrColorsDone { number, head_oid: _ } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
                && let Some(pkg) = app.cache.get(number)
            {
                r.status = format!("{} files", pkg.files.len());
            }
        }
        Response::PrLoadError { number, error } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("load failed: {error}");
            }
            st.list.status = format!("load #{number} failed: {error}");
        }
        Response::MergeDone { number, result: Ok(()) } => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
            st.merge = None;
            st.merging = None;
            st.picker = None;
            st.list.status = format!("merged #{number}");
            st.list.prs.clear();
            st.list.selected = 0;
            send_refresh(app, st, false);
        }
        Response::MergeDone { number, result: Err(e) } => {
            st.merging = None;
            st.list.status = format!("merge #{number} failed: {e}");
        }
    }
}
```

Update `send_refresh` to include a gen — but `list_gen` hasn't been
added to `AppState` yet (Task 5). For now, pass a constant:

```rust
fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    st.list_refresh_in_flight = true;
    if !silent {
        st.list.loading = true;
    }
    app.request(Request::RefreshList { gen: 0 });
}
```

`gen: 0` is a temporary placeholder; Task 5 replaces it with the real
counter.

- [ ] **Step 6: Run all tests**

Run: `cargo test 2>&1 | tail -10`
Expected: all tests pass, including the new
`worker_emits_list_fast_then_enriched_with_matching_gen`. The other
build_package-using tests remain valid because `LoadPr` still goes
through `build_package` and then re-emits the data as granular events.

- [ ] **Step 7: Commit**

```bash
git add src/data/worker.rs src/app.rs
git commit -m "feat(worker): two-phase list + granular PrLoad responses"
```

---

### Task 5: Generation counter and enrichment merge in `AppState`

**Files:**
- Modify: `src/app.rs`

Adds the real `list_gen` field, increments on every refresh, drops stale
responses, and stops flipping `list_refresh_in_flight` to false until
`ListEnriched` arrives.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/app.rs`:

```rust
    use crate::data::cache::Cache;
    use crate::data::pr::{Author, Label, Pr, PrEnrichment, PrState, StatusCheck};
    use crate::data::worker::Response;

    fn dummy_app_state() -> AppState {
        AppState::new("repo".into(), "main".into())
    }

    fn open_pr(n: u32) -> Pr {
        Pr {
            number: n,
            title: format!("#{n}"),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    #[test]
    fn fresh_app_state_has_zero_list_gen() {
        let st = dummy_app_state();
        assert_eq!(st.list_gen, 0);
    }

    #[test]
    fn stale_list_fast_is_dropped() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 5;
        // A response from a much older generation arrives.
        let stale = Response::ListFast {
            gen: 1,
            result: Ok(vec![open_pr(1)]),
        };
        handle_response(&mut app, &mut st, stale);
        // Nothing applied: rows still empty.
        assert!(st.list.prs.is_empty());
    }

    #[test]
    fn enrichment_merges_by_number() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                gen: 1,
                result: Ok(vec![open_pr(7), open_pr(8)]),
            },
        );
        handle_response(
            &mut app,
            &mut st,
            Response::ListEnriched {
                gen: 1,
                result: Ok(vec![PrEnrichment {
                    number: 7,
                    status_check_rollup: vec![StatusCheck {
                        status: Some("COMPLETED".into()),
                        conclusion: Some("FAILURE".into()),
                    }],
                    review_decision: None,
                    mergeable: Some("CONFLICTING".into()),
                }]),
            },
        );
        let by_num: std::collections::HashMap<u32, &Pr> = st
            .list
            .prs
            .iter()
            .map(|p| (p.number, p))
            .collect();
        assert_eq!(by_num[&7].status_check_rollup.len(), 1);
        assert_eq!(by_num[&7].mergeable.as_deref(), Some("CONFLICTING"));
        assert!(by_num[&8].status_check_rollup.is_empty());
    }

    #[test]
    fn list_refresh_in_flight_clears_only_after_enriched() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        st.list_refresh_in_flight = true;
        st.list.enriching = true;
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                gen: 1,
                result: Ok(vec![]),
            },
        );
        // After fast, still in flight, still enriching.
        assert!(st.list_refresh_in_flight);
        assert!(st.list.enriching);
        handle_response(
            &mut app,
            &mut st,
            Response::ListEnriched {
                gen: 1,
                result: Ok(vec![]),
            },
        );
        assert!(!st.list_refresh_in_flight);
        assert!(!st.list.enriching);
    }
```

You'll also need a helper to make a test `App` without a worker thread.
Add to the test module:

```rust
    fn test_app_for_state(cache: &mut Cache) -> App {
        use crate::data::gh::fakes::FakeGh;
        use crate::data::git::fakes::FakeGit;
        use crate::config::Config;
        let gh: std::sync::Arc<dyn crate::data::gh::GhClient> = std::sync::Arc::new(FakeGh::new());
        let git: std::sync::Arc<dyn crate::data::git::GitClient> =
            std::sync::Arc::new(FakeGit::new("/tmp/repo"));
        let mut app = App::new("/tmp/repo".into(), gh, git, Config { window_size: 7, show_sha_margin: false });
        // Replace the empty cache with the test's cache by re-assigning fields.
        std::mem::swap(&mut app.cache, cache);
        app
    }
```

The above test_app_for_state may need adjustment depending on
`Config` fields — check `src/config.rs` and use defaults that compile.
If `Config` has private fields, expose a `Config::for_test()` helper in
`config.rs` returning a default config.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app -- --nocapture`
Expected: compile error — `list_gen` and `list.enriching` don't exist.

- [ ] **Step 3: Add fields and wire generation**

In `src/app.rs`, modify `AppState`:

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
    pub list_refresh_in_flight: bool,
    /// Monotonically-incrementing refresh cycle id. Used to drop stale
    /// `ListFast`/`ListEnriched` responses from a superseded refresh.
    pub list_gen: u32,
}
```

…and in `AppState::new`:

```rust
            list_gen: 0,
```

In `send_refresh`, increment and pass:

```rust
fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    st.list_refresh_in_flight = true;
    st.list.enriching = false;
    if !silent {
        st.list.loading = true;
    }
    st.list_gen = st.list_gen.wrapping_add(1);
    let gen = st.list_gen;
    app.request(Request::RefreshList { gen });
}
```

Update `handle_response` for the two list arms to apply the generation
filter and the `enriching` flag (replace just those four arms):

```rust
        Response::ListFast { gen, result } if gen == st.list_gen => match result {
            Ok(prs) => {
                let prev_selected = st
                    .list
                    .visible_prs()
                    .get(st.list.selected)
                    .map(|p| p.number);
                st.list.prs = prs.clone();
                app.cache.set_list(prs);
                st.list.loading = false;
                st.list.enriching = true;
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
            Err(e) => {
                st.list_refresh_in_flight = false;
                st.list.enriching = false;
                st.list.loading = false;
                st.list.status = format!("refresh failed: {e}");
            }
        },
        Response::ListFast { .. } => { /* stale; drop */ }
        Response::ListEnriched { gen, result } if gen == st.list_gen => {
            st.list_refresh_in_flight = false;
            st.list.enriching = false;
            if let Ok(es) = result {
                for e in &es {
                    if let Some(p) =
                        st.list.prs.iter_mut().find(|p| p.number == e.number)
                    {
                        p.apply_enrichment(e);
                    }
                }
            }
            // Enrichment errors are non-fatal: rows already render with
            // light-fields-only glyphs.
        }
        Response::ListEnriched { .. } => { /* stale; drop */ }
```

Add `enriching` to `PrListState` — see Task 6 step 3 for the definitive
field block. For this task, just append `pub enriching: bool,` to the
struct and `enriching: false,` to its `Default` impl (which is
`#[derive(Default)]` — `bool` defaults to `false` automatically, so
no Default-impl change needed).

In `src/view/pr_list.rs`, add to `PrListState`:

```rust
    /// True between `ListFast` and `ListEnriched` arrivals. Footer shows
    /// `enriching…` so background work is never silent.
    pub enriching: bool,
```

(Renderer changes for `enriching` come in Task 6.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib app -- --nocapture` and then `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/view/pr_list.rs
git commit -m "feat(app): generation counter + enrichment merge"
```

---

### Task 6: PR list footer — surface `enriching…`

**Files:**
- Modify: `src/view/pr_list.rs`

The footer should show the new `enriching…` state when no error/loading
status is in play. This is a small change to `render_footer`.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `src/view/pr_list.rs`:

```rust
    #[test]
    fn footer_shows_enriching_when_flag_set() {
        let mut st = fixture_state();
        st.enriching = true;
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bottom = buffer_line(buf, 9);
        assert!(bottom.contains("enriching"), "footer was: {bottom:?}");
    }

    #[test]
    fn footer_omits_enriching_when_flag_clear() {
        let st = fixture_state();
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bottom = buffer_line(buf, 9);
        assert!(!bottom.contains("enriching"), "footer was: {bottom:?}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib view::pr_list::tests::footer_shows_enriching_when_flag_set view::pr_list::tests::footer_omits_enriching_when_flag_clear -- --nocapture`
Expected: failures — footer doesn't show `enriching` yet.

- [ ] **Step 3: Modify `render_footer`**

In `src/view/pr_list.rs::render_footer`, update the conditional ladder:

```rust
    if !st.status.is_empty() {
        let (prefix, color) = if spinner::looks_in_progress(&st.status) {
            (format!("{} ", spinner::glyph()), OVERLAY1)
        } else {
            (String::new(), DIFF_DEL_FG)
        };
        f.render_widget(
            Paragraph::new(format!("  {prefix}{}", st.status)).style(Style::default().fg(color)),
            chunks[1],
        );
    } else if st.loading {
        f.render_widget(
            Paragraph::new(format!("  {} refreshing…", spinner::glyph()))
                .style(Style::default().fg(OVERLAY1)),
            chunks[1],
        );
    } else if st.enriching {
        f.render_widget(
            Paragraph::new(format!("  {} enriching…", spinner::glyph()))
                .style(Style::default().fg(OVERLAY1)),
            chunks[1],
        );
    } else {
        f.render_widget(
            Paragraph::new(
                "  state ●open ○draft   ci ✓pass ✗fail …pend   review ✓approved !changes ·pending   ⚠conflict",
            )
            .style(Style::default().fg(OVERLAY0)),
            chunks[1],
        );
    }
```

Also update the `fixture_state` test helper at the top of the test
block to initialize `enriching: false`:

```rust
        PrListState {
            repo_name: "prpr".into(),
            branch: "main".into(),
            prs,
            selected: 0,
            filter_open_only: true,
            search: None,
            loading: false,
            status: String::new(),
            enriching: false,
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib view::pr_list -- --nocapture` and then `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(view): pr_list footer surfaces enriching state"
```

---

### Task 7: Worker — PR review streaming pipeline

**Files:**
- Modify: `src/data/worker.rs`

Replace the temporary `LoadPr` shim from Task 4 with the real streaming
pipeline:
- Spawn `gh pr view`, `gh pr diff`, `git fetch` in parallel.
- Emit `PrDetail` and `PrDiff` events as they complete (any order).
- After detail+diff+fetch all succeed, blame `files[0]` synchronously,
  emit its `PrFileColors`, then fan out the rest via the existing
  atomic-counter pool, emitting per-file events from each worker.
- Emit `PrColorsDone` after the last file.
- On any failure that aborts the load (`view`/`diff`/`fetch` error),
  emit `PrLoadError` and stop.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `src/data/worker.rs`:

```rust
    #[test]
    fn load_pr_streams_detail_diff_then_per_file_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let number = detail.number;

        let mut gh = FakeGh::new();
        gh.views.insert(number, detail.clone());
        gh.diffs.insert(
            number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames
            .insert((head_sha.clone(), "README.md".into()), porcelain);

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::LoadPr(number));

        // Drain everything until PrColorsDone, with a deadline so the test
        // can't hang. Order between PrDetail and PrDiff is "whoever finishes
        // first" but in the fake both are instant, so we accept either
        // order. We assert by counting.
        let mut got_detail = false;
        let mut got_diff = false;
        let mut color_paths: Vec<String> = vec![];
        let mut done = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !done {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrDiff { number: n, result: Ok(_) }) if n == number => {
                    got_diff = true;
                }
                Ok(Response::PrFileColors {
                    number: n,
                    head_oid,
                    path,
                    ..
                }) if n == number => {
                    assert_eq!(head_oid, head_sha);
                    color_paths.push(path);
                }
                Ok(Response::PrColorsDone { number: n, .. }) if n == number => {
                    done = true;
                }
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) | Err(_) => {}
            }
        }
        assert!(got_detail, "never received PrDetail");
        assert!(got_diff, "never received PrDiff");
        assert!(done, "never received PrColorsDone");
        // First color event is for files[0] (the visible file).
        assert_eq!(color_paths.first().map(String::as_str), Some("src/sched.rs"));
    }

    #[test]
    fn load_pr_emits_load_error_when_view_fails() {
        let mut gh = FakeGh::new();
        // No fixture inserted → fake returns an error.
        gh.diffs.insert(
            1,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::LoadPr(1));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_error = false;
        while std::time::Instant::now() < deadline && !saw_error {
            if let Ok(Response::PrLoadError { number: 1, .. }) =
                worker.rx.recv_timeout(std::time::Duration::from_millis(500))
            {
                saw_error = true;
            }
        }
        assert!(saw_error, "did not receive PrLoadError");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib data::worker -- --nocapture`
Expected:
- `load_pr_streams_detail_diff_then_per_file_colors` passes the
  current shim if it happens to emit events in the right order — but
  it asserts `color_paths.first() == "src/sched.rs"` which the current
  shim cannot guarantee (it iterates a HashMap). Should fail.
- `load_pr_emits_load_error_when_view_fails` fails because the shim's
  `build_package` call returns `Err`, the worker only sends
  `PrLoadError` for that — actually this might already pass under the
  shim. If it does, that's fine.

- [ ] **Step 3: Replace the `LoadPr` arm with the streaming pipeline**

In `src/data/worker.rs::run_worker`, replace the entire `Request::LoadPr(number)`
arm with:

```rust
            Request::LoadPr(number) => {
                run_load(&*gh, &*git, &repo_root, &res_tx, number, window_size);
            }
```

Add `run_load` and supporting helpers at module scope (after
`run_worker`):

```rust
fn run_load(
    gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    number: u32,
    window_size: usize,
) {
    // Stage 1: kick off view, diff, fetch in parallel; emit detail and
    // diff events as they complete.
    let (detail_res, files_res, fetch_res) = thread::scope(|s| {
        let view_tx = res_tx.clone();
        let diff_tx = res_tx.clone();
        let detail_h = s.spawn(move || {
            let r = gh.view_pr(repo_root, number);
            match &r {
                Ok(d) => {
                    let _ = view_tx.send(Response::PrDetail {
                        number,
                        result: Ok(d.clone()),
                    });
                }
                Err(e) => {
                    let _ = view_tx.send(Response::PrDetail {
                        number,
                        result: Err(anyhow!(e.to_string())),
                    });
                }
            }
            r
        });
        let diff_h = s.spawn(move || {
            let raw = gh.diff_pr(repo_root, number);
            let parsed = raw.and_then(|s| parse_diff(&s));
            match &parsed {
                Ok(f) => {
                    let _ = diff_tx.send(Response::PrDiff {
                        number,
                        result: Ok(f.clone()),
                    });
                }
                Err(e) => {
                    let _ = diff_tx.send(Response::PrDiff {
                        number,
                        result: Err(anyhow!(e.to_string())),
                    });
                }
            }
            parsed
        });
        let fetch_h = s.spawn(|| git.fetch_pr(repo_root, number));
        (detail_h.join().unwrap(), diff_h.join().unwrap(), fetch_h.join().unwrap())
    });

    // If any prerequisite failed, surface PrLoadError and stop. The
    // already-emitted PrDetail/PrDiff (if any) is harmless — the cache
    // ignores stragglers.
    let detail = match detail_res {
        Ok(d) => d,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: e.to_string(),
            });
            return;
        }
    };
    let files = match files_res {
        Ok(f) => f,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: e.to_string(),
            });
            return;
        }
    };
    if let Err(e) = fetch_res {
        let _ = res_tx.send(Response::PrLoadError {
            number,
            error: format!("fetching PR #{number}: {e}"),
        });
        return;
    }

    let head_oid = detail.head_ref_oid.clone();
    let base_oid = detail.base_ref_oid.clone();
    let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();

    // Stage 2: blame files[0] synchronously and emit its colors first.
    if let Some(f) = files.first() {
        if !f.binary {
            let (lc, per) = blame_file(git, repo_root, &commits, &head_oid, &base_oid, f, window_size);
            let _ = res_tx.send(Response::PrFileColors {
                number,
                head_oid: head_oid.clone(),
                path: f.path.clone(),
                colors: lc,
                stats: per,
            });
        }
    }

    // Stage 3: parallel pool for the remainder.
    let remainder: Vec<&FileDiff> = files.iter().skip(1).filter(|f| !f.binary).collect();
    let n = remainder.len();
    if n > 0 {
        let n_workers = thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4)
            .min(n);
        let next_idx = AtomicUsize::new(0);
        thread::scope(|s| {
            for _ in 0..n_workers {
                let tx = res_tx.clone();
                let head_oid = head_oid.clone();
                let base_oid = base_oid.clone();
                let commits = commits.clone();
                let remainder = &remainder;
                let next_idx = &next_idx;
                s.spawn(move || {
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= remainder.len() {
                            break;
                        }
                        let f = remainder[i];
                        let (lc, per) = blame_file(
                            git, repo_root, &commits, &head_oid, &base_oid, f, window_size,
                        );
                        let _ = tx.send(Response::PrFileColors {
                            number,
                            head_oid: head_oid.clone(),
                            path: f.path.clone(),
                            colors: lc,
                            stats: per,
                        });
                    }
                });
            }
        });
    }

    let _ = res_tx.send(Response::PrColorsDone {
        number,
        head_oid,
    });
}

/// Blame + log-patches for one file. Mirrors the inner loop of the old
/// `parallel_per_file` worker. Returns the file's `LineColors` and its
/// per-commit `CommitStats` contribution.
fn blame_file(
    git: &dyn GitClient,
    repo_root: &Path,
    commits: &[String],
    head_oid: &str,
    base_oid: &str,
    f: &FileDiff,
    window_size: usize,
) -> (LineColors, HashMap<String, CommitStats>) {
    let head = git
        .blame(repo_root, head_oid, &f.path)
        .map(|s| parse_blame(&s))
        .unwrap_or_else(|_| Blame { line_shas: vec![] });
    let log_out = git
        .log_patches(repo_root, base_oid, head_oid, &f.path)
        .unwrap_or_default();
    let deletes = parse_deletions(&log_out);
    let lc = attribute_file(commits, window_size, &head, &deletes);
    let per = commit_stats_for_file(commits, &head, &deletes);
    (lc, per)
}
```

Required imports in `src/data/worker.rs` (verify they're present, add
if missing):

```rust
use crate::data::diff::FileDiff;
use crate::data::diff::parse_diff;
```

Drop or keep the existing `build_package` and `parallel_per_file`
fns — they're no longer called from production code paths. If you
delete them, also delete the `build_package_assembles_diff_and_colors`
and `build_package_populates_commit_stats` tests (they test a function
that no longer exists). The streaming pipeline covers their behavior
end-to-end through `load_pr_streams_detail_diff_then_per_file_colors`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib data::worker -- --nocapture` and then `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/data/worker.rs
git commit -m "feat(worker): stream PR review load (detail/diff/per-file colors)"
```

---

### Task 8: Cache-aware `handle_response` cleanup

**Files:**
- Modify: `src/app.rs`

Task 4 wrote temporary `handle_response` arms for PR-review events. Task
7's pipeline now feeds them real per-file data, including `stats`. Audit
each arm and remove anything left over from the Task 3 shim.

- [ ] **Step 1: Read the current `handle_response`**

Open `src/app.rs::handle_response`. The arms for `PrDetail`, `PrDiff`,
`PrFileColors`, `PrColorsDone`, and `PrLoadError` should already match
Task 4's final shape. **Specifically verify:** the Task 3 shim that
called `insert_partial` + `update_diff` + a loop of `add_file_colors`
inside the (long-gone) `PrLoaded` arm has been removed. If you see any
`Response::PrLoaded` reference, delete it.

- [ ] **Step 2: Run all tests**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 3: Commit (if any changes were needed)**

If no changes were made, skip the commit. Otherwise:

```bash
git add src/app.rs
git commit -m "chore(app): drop residual PR-load shim from handle_response"
```

---

### Task 9: PR review rendering — file list / body fallback to `detail.files`

**Files:**
- Modify: `src/data/cache.rs` (or wherever `PrPackage` lives — currently
  `src/data/cache.rs`)
- Modify: `src/view/pr_review.rs`

Two small helpers on `PrPackage` and renderer fallbacks. When `pkg.files`
is empty (PrDetail in cache, PrDiff not yet arrived), the file bar and
file picker read paths from `pkg.detail.files`, the counter uses
`detail.files.len()`, and the diff body shows a "loading diff…"
placeholder.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/data/cache.rs`:

```rust
    #[test]
    fn pr_package_file_count_uses_detail_when_files_empty() {
        let detail = fixture_detail("h");
        let n = detail.files.len();
        let mut cache = Cache::new();
        let number = detail.number;
        cache.insert_partial(detail);
        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.file_count(), n);
        assert!(pkg.file_paths().len() == n);
    }

    #[test]
    fn pr_package_file_count_uses_files_when_populated() {
        let detail = fixture_detail("h");
        let mut cache = Cache::new();
        let number = detail.number;
        let head = detail.head_ref_oid.clone();
        cache.insert_partial(detail);
        cache.update_diff(
            number,
            &head,
            vec![FileDiff {
                path: "only.rs".into(),
                lines: vec![],
                binary: false,
            }],
        );
        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.file_count(), 1);
        assert_eq!(pkg.file_paths(), vec!["only.rs"]);
    }
```

Append to the `#[cfg(test)] mod tests` block in `src/view/pr_review.rs`:

```rust
    #[test]
    fn file_bar_uses_detail_files_when_pkg_files_empty() {
        let mut pkg = fixture_pkg();
        pkg.files = vec![];
        let st = PrReviewState::default();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();
        // File bar is at row 2 (header row 0; spacer row 1; file bar rows 2-3).
        let bar = buffer_line(buf, 2);
        // First detail.files entry from fixture is "src/sched.rs".
        assert!(bar.contains("src/sched.rs"), "bar was: {bar:?}");
        // Counter shows detail.files count.
        assert!(bar.contains(&format!("file 1/{}", pkg.detail.files.len())), "bar was: {bar:?}");
    }

    #[test]
    fn diff_body_shows_loading_when_pkg_files_empty() {
        let mut pkg = fixture_pkg();
        pkg.files = vec![];
        let st = PrReviewState::default();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let body = buffer_line(buf, 4);
        assert!(body.contains("loading diff"), "body was: {body:?}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib data::cache view::pr_review -- --nocapture`
Expected: compile errors and assertion failures.

- [ ] **Step 3: Add helpers on `PrPackage`**

In `src/data/cache.rs`, add an `impl PrPackage` block after the struct
definition:

```rust
impl PrPackage {
    /// Path list with fallback. While `gh pr diff` is still in flight,
    /// `files` is empty and we use `detail.files` so the file bar and
    /// picker can render immediately.
    pub fn file_paths(&self) -> Vec<&str> {
        if self.files.is_empty() {
            self.detail.files.iter().map(|f| f.path.as_str()).collect()
        } else {
            self.files.iter().map(|f| f.path.as_str()).collect()
        }
    }

    /// Total file count with the same fallback as `file_paths`.
    pub fn file_count(&self) -> usize {
        if self.files.is_empty() {
            self.detail.files.len()
        } else {
            self.files.len()
        }
    }
}
```

- [ ] **Step 4: Update `render_file_bar` and `render_diff_body`**

In `src/view/pr_review.rs`, replace `render_file_bar`:

```rust
fn render_file_bar(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let paths = pkg.file_paths();
    let total = paths.len();
    let path = paths.get(st.file_index).copied().unwrap_or("");
    let counter = format!("file {}/{}", st.file_index + 1, total.max(1));
    let pad = 40_usize.saturating_sub(path.len()) + 46;
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            path.to_string(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(counter, Style::default().fg(SUBTEXT0)),
    ]);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(line), chunks[0]);
    f.render_widget(
        Paragraph::new("  ".to_string() + &"─".repeat((area.width as usize).saturating_sub(2)))
            .style(Style::default().fg(SURFACE2)),
        chunks[1],
    );
}
```

And `render_diff_body`:

```rust
fn render_diff_body(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    if pkg.files.is_empty() {
        f.render_widget(
            Paragraph::new(format!(
                "  {} loading diff…",
                crate::render::spinner::glyph()
            ))
            .style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let Some(file) = pkg.files.get(st.file_index) else {
        return;
    };
    if file.binary {
        f.render_widget(
            Paragraph::new("  binary file, not displayed").style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let lines = body_lines(file, &pkg.colors);
    f.render_widget(Paragraph::new(lines).scroll((st.scroll, 0)), area);
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/data/cache.rs src/view/pr_review.rs
git commit -m "feat(view): PR review file bar + body fall back to detail.files"
```

---

### Task 10: Navigation guards while `pkg.files` is empty

**Files:**
- Modify: `src/app.rs`

`cycle_file`, `move_review`, `Bottom`, `Top`, and `OpenFilePicker` all
poke into `pkg.files` today. They need to either use the new
`file_count`/`file_paths` helpers or no-op when the diff isn't ready
(no lines to scroll).

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `src/app.rs`:

```rust
    use crate::data::diff::{DiffLine, DiffOp, FileDiff};

    #[test]
    fn cycle_file_uses_detail_files_count_when_files_empty() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let detail: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        let n_detail_files = detail.files.len();
        let number = detail.number;
        cache.insert_partial(detail);
        let mut app = test_app_for_state(&mut cache);
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
        });

        cycle_file(&app, &mut st, 1);
        assert_eq!(st.review.as_ref().unwrap().file_index, 1 % n_detail_files);

        // Wrap to last.
        cycle_file(&app, &mut st, -2);
        let expected = ((1i32 - 2).rem_euclid(n_detail_files as i32)) as usize;
        assert_eq!(st.review.as_ref().unwrap().file_index, expected);
    }

    #[test]
    fn move_review_is_noop_when_pkg_files_empty() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let detail: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        let number = detail.number;
        cache.insert_partial(detail);
        let mut app = test_app_for_state(&mut cache);
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
        });
        move_review(&app, &mut st, 10);
        let r = st.review.as_ref().unwrap();
        assert_eq!(r.cursor_line, 0);
        assert_eq!(r.scroll, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app -- --nocapture`
Expected: failures — `cycle_file` panics or no-ops based on
`pkg.files.len() == 0`.

- [ ] **Step 3: Update `cycle_file` and `move_review`**

In `src/app.rs`, replace:

```rust
fn move_review(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let Some(r) = st.review.as_mut() else { return };
    let Some(file) = pkg.files.get(r.file_index) else {
        return;
    };
    let max_scr = max_scroll(file.lines.len()) as i64;
    let max_cur = max_cursor_line(file) as i64;
    let new_scroll = (r.scroll as i64 + delta as i64).clamp(0, max_scr);
    let new_cursor = (r.cursor_line as i64 + delta as i64).clamp(0, max_cur);
    r.scroll = new_scroll as u16;
    r.cursor_line = new_cursor as usize;
}

fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let n = pkg.files.len() as i32;
    if n == 0 {
        return;
    }
    if let Some(r) = st.review.as_mut() {
        let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
        r.file_index = new_idx;
        r.cursor_line = 0;
        r.scroll = 0;
    }
}
```

With:

```rust
fn move_review(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let Some(r) = st.review.as_mut() else { return };
    // No scrollable content yet — pkg.files is empty until PrDiff arrives.
    let Some(file) = pkg.files.get(r.file_index) else {
        return;
    };
    let max_scr = max_scroll(file.lines.len()) as i64;
    let max_cur = max_cursor_line(file) as i64;
    let new_scroll = (r.scroll as i64 + delta as i64).clamp(0, max_scr);
    let new_cursor = (r.cursor_line as i64 + delta as i64).clamp(0, max_cur);
    r.scroll = new_scroll as u16;
    r.cursor_line = new_cursor as usize;
}

fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let n = pkg.file_count() as i32;
    if n == 0 {
        return;
    }
    if let Some(r) = st.review.as_mut() {
        let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
        r.file_index = new_idx;
        r.cursor_line = 0;
        r.scroll = 0;
    }
}
```

Also update the `Action::Bottom` arm in `handle_key` so it uses the
`file_paths` fallback for the "no diff yet" case (it already early-exits
when `pkg.files.get(r.file_index)` is None — leave as-is, no change
needed). Likewise `Action::Top` (just resets scroll, no change).

Update `Action::OpenFilePicker`:

```rust
        Action::OpenFilePicker => {
            if let (Some(num), Some(r)) = (st.current_pr, st.review.as_ref())
                && let Some(pkg) = app.cache.get(num)
            {
                let paths: Vec<String> = pkg.file_paths().into_iter().map(String::from).collect();
                let current = pkg.file_paths().get(r.file_index).copied();
                st.picker = Some(FilePickerState::new(paths, current));
                st.focused = FocusedView::FilePicker;
            }
        }
```

And the file picker's `Enter` handler:

```rust
        KeyCode::Enter => {
            let chosen = picker.matches().get(picker.selected).map(|s| (*s).clone());
            if let (Some(path), Some(num)) = (chosen, st.current_pr)
                && let Some(pkg) = app.cache.get(num)
            {
                // Look up the chosen path against the same source the
                // picker rendered from.
                let idx = pkg.file_paths().iter().position(|p| *p == path.as_str());
                if let (Some(idx), Some(r)) = (idx, st.review.as_mut()) {
                    r.file_index = idx;
                    r.cursor_line = 0;
                    r.scroll = 0;
                }
            }
            st.picker = None;
            st.focused = FocusedView::Review;
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): navigation and picker use file_count fallback"
```

---

### Task 11: End-to-end smoke through `handle_response`

**Files:**
- Modify: `src/app.rs`

A test that drives the worker through `FakeGh`/`FakeGit`, drains
responses, feeds them into `handle_response`, and asserts the UI state
after each milestone (partial promotion → diff arrived → colors done).

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `src/app.rs`:

```rust
    use crate::data::worker::{Request, Worker};

    #[test]
    fn end_to_end_load_pr_progresses_through_partial_states() {
        use crate::data::gh::fakes::FakeGh;
        use crate::data::git::fakes::FakeGit;
        use crate::data::pr::PrDetail;

        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let number = detail.number;
        let head_sha = detail.head_ref_oid.clone();

        let mut gh = FakeGh::new();
        gh.views.insert(number, detail.clone());
        gh.diffs.insert(
            number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames
            .insert((head_sha.clone(), "README.md".into()), porcelain);

        let mut app = App::new(
            "/tmp/repo".into(),
            std::sync::Arc::new(gh),
            std::sync::Arc::new(git),
            crate::config::Config {
                window_size: 7,
                show_sha_margin: false,
            },
        );
        let mut st = AppState::new("repo".into(), "main".into());
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: "loading…".into(),
        });

        app.request(Request::LoadPr(number));

        // Drain until we see PrColorsDone, feeding events through.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_detail = false;
        let mut saw_diff = false;
        let mut done = false;
        while std::time::Instant::now() < deadline && !done {
            match app
                .worker
                .rx
                .recv_timeout(std::time::Duration::from_millis(500))
            {
                Ok(resp) => {
                    let is_detail = matches!(resp, crate::data::worker::Response::PrDetail { .. });
                    let is_diff = matches!(resp, crate::data::worker::Response::PrDiff { .. });
                    let is_done =
                        matches!(resp, crate::data::worker::Response::PrColorsDone { .. });
                    handle_response(&mut app, &mut st, resp);
                    if is_detail {
                        saw_detail = true;
                        assert!(app.cache.get(number).is_some(), "cache should have partial");
                        assert_eq!(st.review.as_ref().unwrap().status, "loading diff…");
                    }
                    if is_diff {
                        saw_diff = true;
                        assert!(!app.cache.get(number).unwrap().files.is_empty());
                        assert!(
                            st.review.as_ref().unwrap().status.starts_with("coloring "),
                            "status was: {}",
                            st.review.as_ref().unwrap().status,
                        );
                    }
                    if is_done {
                        done = true;
                        let n = app.cache.get(number).unwrap().files.len();
                        assert_eq!(st.review.as_ref().unwrap().status, format!("{n} files"));
                    }
                }
                Err(_) => continue,
            }
        }
        assert!(saw_detail && saw_diff && done, "missed an event");
    }
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --lib app::tests::end_to_end_load_pr_progresses_through_partial_states -- --nocapture`
Expected: pass. If it fails, the diagnostic will point to which
milestone has the wrong state — fix the corresponding `handle_response`
arm and re-run.

- [ ] **Step 3: Run the full suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "test(app): end-to-end PR load progresses through partial states"
```

---

### Task 12: Final cleanup

**Files:**
- Modify: `src/data/cache.rs`, `src/data/worker.rs`, `src/app.rs` (as
  needed)

Remove dead code that the streaming pipeline obsoletes.

- [ ] **Step 1: Audit `Cache::insert`**

Search for production callers:

Run: `grep -rn "cache.insert(" src/ --include='*.rs' | grep -v "_partial\|insert_partial"`
Expected: no matches outside of tests. (`set_list`, `insert_partial`,
`update_diff`, `add_file_colors` are the live API.)

If `Cache::insert` is unused, remove it from `impl Cache`. Update or
remove any tests that called it (the new tests already exercise the
equivalent semantics via `insert_partial` + `update_diff`).

- [ ] **Step 2: Audit `build_package` and `parallel_per_file`**

Run: `grep -rn "build_package\|parallel_per_file" src/`
Expected: only the test references in `src/data/worker.rs`, if any. If
Task 7 already deleted them, you'll see nothing — skip to step 3.

If they're still defined and unused, delete them. Also delete the two
old tests `build_package_assembles_diff_and_colors` and
`build_package_populates_commit_stats` — the streaming test in Task 7
covers the same behavior.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 4: Build a release binary as a final sanity check**

Run: `cargo build --release 2>&1 | tail -10`
Expected: `Finished release [optimized] target(s)`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "chore: drop dead Cache::insert and build_package after streaming"
```

---

## Self-review checklist (run after writing)

- **Spec coverage:** §1 worker protocol → Tasks 4, 7. §2 cache partials
  → Tasks 3, 9. §3 list two-phase → Tasks 2, 4, 5, 6. §4 per-file
  streaming + visible-file priority → Task 7. §5 UI rendering →
  Tasks 6, 9, 10. End-to-end smoke → Task 11. ✓
- **Placeholders:** none — every code step is fully spelled out.
- **Type consistency:** `Response` variant names match across worker
  and app (`ListFast`/`ListEnriched`/`PrDetail`/`PrDiff`/`PrFileColors`/
  `PrColorsDone`/`PrLoadError`); `insert_partial(detail)` signature
  consistent in cache and call sites; `file_paths()`/`file_count()`
  added in Task 9 and used in Task 10; `enriching` field added in Task
  5 and consumed in Task 6.
- **Test-first ordering:** every task writes the failing test before
  the implementation.
