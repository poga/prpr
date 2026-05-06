# Commits Modal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the horizontal commit strip at the top of the PR review view with a display-only vertical commits modal triggered by `c` (and remove the strip's code, config, and toggle entirely).

**Architecture:** A new `view::commits_modal` module mirrors `view::file_picker`: an overlay state struct, a `render` function, a new `FocusedView::CommitsModal`, a new `Action::OpenCommitsModal`, an `AppState.commits: Option<…>` field, and a small `handle_commits_modal` in `app.rs`. Per-commit `+adds −dels` stats are computed once in `data::worker::build_package` from the existing raw blame + delete-text maps and stored on `PrPackage.commit_stats`. Commit dates come from extending the gh `pr view` GraphQL field-list with `committedDate`.

**Tech Stack:** Rust 2024, ratatui (TUI), crossterm, chrono, serde + serde_json, anyhow, pretty_assertions for tests.

**Spec:** `docs/superpowers/specs/2026-05-06-commits-modal-design.md`

---

## File map

**Created**
- `src/view/commits_modal.rs` — `CommitsModalState`, `CommitRow`, `render`, `relative_date` helper.

**Modified**
- `src/data/pr.rs` — `Commit` gains `committed_date: Option<DateTime<Utc>>`.
- `src/data/gh.rs` — `PR_VIEW_FIELDS` gains `committedDate`.
- `src/render/attribution.rs` — new `CommitStats` struct + `commit_stats_for_file` helper.
- `src/data/cache.rs` — `PrPackage` gains `commit_stats: HashMap<String, CommitStats>`.
- `src/data/worker.rs` — `build_package` populates `commit_stats`.
- `src/view/mod.rs` — register the new module.
- `src/keys.rs` — add `FocusedView::CommitsModal`, `Action::OpenCommitsModal`; remove `Action::ToggleCommitStrip`; rebind `c` in review.
- `src/app.rs` — `AppState.commits`, action handler, `handle_commits_modal`, draw branch; remove `ToggleCommitStrip` arm + `show_commit_strip` plumbing on PrReviewState init.
- `src/view/pr_review.rs` — remove `render_commit_strip`, `show_commit_strip` field, strip layout slot, `c strip` hint token.
- `src/config.rs` — remove `show_commit_strip` from `Config` and `RawUi`; update tests.
- `src/view/help.rs` — update `HELP_TEXT`.
- `tests/fixtures/pr_view.json` — add `committedDate` strings to each commit.

---

## Test command reference

Run all tests: `cargo test --quiet`
Run a single test: `cargo test --quiet <test_name>`
Run tests in a module: `cargo test --quiet <module>::tests::`

---

### Task 1: Extend `Commit` with `committed_date`

**Files:**
- Modify: `src/data/pr.rs:140-146` (`Commit` struct)
- Modify: `tests/fixtures/pr_view.json` (add `committedDate` to each commit entry)
- Test: `src/data/pr.rs` tests module

- [ ] **Step 1: Add a failing test for the new field**

In `src/data/pr.rs`, append to the `tests` module at the bottom of the file:

```rust
    #[test]
    fn parses_committed_date_when_present() {
        use chrono::TimeZone;
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let detail: PrDetail = serde_json::from_str(json).unwrap();
        let first = &detail.commits[0];
        assert_eq!(
            first.committed_date,
            Some(Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap()),
        );
    }

    #[test]
    fn missing_committed_date_is_none() {
        // Older API responses or edge fixtures may omit the field.
        let json = r#"{"oid":"a","messageHeadline":"x","authors":[]}"#;
        let c: Commit = serde_json::from_str(json).unwrap();
        assert_eq!(c.committed_date, None);
    }
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo test --quiet parses_committed_date_when_present missing_committed_date_is_none`
Expected: FAIL — `committed_date` field doesn't exist on `Commit`.

- [ ] **Step 3: Add the field to `Commit`**

In `src/data/pr.rs`, change the `Commit` struct to:

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Commit {
    pub oid: String,
    #[serde(rename = "messageHeadline")]
    pub message_headline: String,
    pub authors: Vec<Author>,
    #[serde(rename = "committedDate", default)]
    pub committed_date: Option<DateTime<Utc>>,
}
```

`DateTime<Utc>` is already imported at the top of the file (line 1).

- [ ] **Step 4: Update the fixture**

Edit `tests/fixtures/pr_view.json` so each commit entry includes `committedDate`. Replace the `commits` array with:

```json
  "commits": [
    { "oid": "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0", "messageHeadline": "init structure", "authors": [{ "login": "alice" }], "committedDate": "2026-05-04T12:00:00Z" },
    { "oid": "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3", "messageHeadline": "enum dispatch", "authors": [{ "login": "alice" }], "committedDate": "2026-05-05T09:30:00Z" },
    { "oid": "789abcdef0123456789abcdef0123456789abcde", "messageHeadline": "add Wait variant", "authors": [{ "login": "alice" }], "committedDate": "2026-05-06T08:15:00Z" }
  ],
```

- [ ] **Step 5: Run the test and confirm it passes**

Run: `cargo test --quiet parses_committed_date_when_present missing_committed_date_is_none`
Expected: PASS — both new tests green.

- [ ] **Step 6: Run the full data-layer test set as a regression check**

Run: `cargo test --quiet data::pr::`
Expected: All existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/data/pr.rs tests/fixtures/pr_view.json
git commit -m "feat(data): add committed_date to Commit"
```

---

### Task 2: Verify gh returns `committedDate` and add a fixture guard

**Files:**
- Modify: `src/data/gh.rs` — append a tests module that asserts the fixture carries `committedDate`.

`gh pr view --json commits` returns each commit's full node, which includes `committedDate` for any reasonably modern `gh` (≥ 2.20, released 2022). `PR_VIEW_FIELDS` already requests `commits`, so no constant change is required. The serde `#[serde(default)]` from Task 1 already tolerates the field being missing (it deserializes to `None`).

This task adds a single contract test so a future fixture edit can't silently strip the field we now depend on for relative-date display.

- [ ] **Step 1: Add the failing test**

Append to the bottom of `src/data/gh.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn fixture_view_round_trips_committed_date() {
        // Guards that the shared fixture carries the field the modal uses.
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let pr: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        assert!(
            pr.commits.iter().all(|c| c.committed_date.is_some()),
            "every commit in the fixture must have committed_date set",
        );
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --quiet fixture_view_round_trips_committed_date`
Expected: PASS — the fixture was extended in Task 1.

- [ ] **Step 3: Commit**

```bash
git add src/data/gh.rs
git commit -m "test(data): guard pr_view fixture carries committed_date"
```

---

### Task 3: `CommitStats` + `commit_stats_for_file` helper

**Files:**
- Modify: `src/render/attribution.rs`
- Test: `src/render/attribution.rs` tests module

The helper sums adds/dels per OID using the same raw inputs `attribute_file` already takes (`Blame.line_shas` for additions; `delete_text_to_sha` map values for deletions). Only OIDs that appear in the PR's commit list are counted — older commits don't get a row in the modal so they don't get stats.

- [ ] **Step 1: Add a failing test**

Append to `src/render/attribution.rs` tests module:

```rust
    #[test]
    fn commit_stats_counts_adds_and_dels_for_pr_commits() {
        let commits = vec![sha('a'), sha('b')];
        let head_blame = Blame {
            line_shas: vec![sha('a'), sha('a'), sha('b'), sha('z'), String::new()],
        };
        let mut deletes = HashMap::new();
        deletes.insert("removed by a".to_string(), sha('a'));
        deletes.insert("removed by b".to_string(), sha('b'));
        deletes.insert("removed by z".to_string(), sha('z')); // not a PR commit

        let stats = commit_stats_for_file(&commits, &head_blame, &deletes);

        assert_eq!(stats.get(&sha('a')).copied(), Some(CommitStats { adds: 2, dels: 1 }));
        assert_eq!(stats.get(&sha('b')).copied(), Some(CommitStats { adds: 1, dels: 1 }));
        assert!(!stats.contains_key(&sha('z'))); // older commits excluded
    }

    #[test]
    fn commit_stats_includes_zero_entries_for_pr_commits_without_changes() {
        // A commit may exist in the PR but not appear in this file.
        let commits = vec![sha('a'), sha('b')];
        let head_blame = Blame { line_shas: vec![sha('a')] };
        let stats = commit_stats_for_file(&commits, &head_blame, &HashMap::new());
        assert_eq!(stats.get(&sha('a')).copied(), Some(CommitStats { adds: 1, dels: 0 }));
        assert_eq!(stats.get(&sha('b')).copied(), Some(CommitStats { adds: 0, dels: 0 }));
    }
```

- [ ] **Step 2: Run the test and confirm failure**

Run: `cargo test --quiet commit_stats_counts_adds_and_dels_for_pr_commits commit_stats_includes_zero_entries_for_pr_commits_without_changes`
Expected: FAIL — symbols `CommitStats` and `commit_stats_for_file` don't exist.

- [ ] **Step 3: Add the type and helper**

In `src/render/attribution.rs`, add at the end of the file (before `#[cfg(test)]`):

```rust
/// Per-commit add/delete counts for the modal display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitStats {
    pub adds: u32,
    pub dels: u32,
}

/// Count head additions + deletions per PR-commit OID for one file.
///
/// Only OIDs in `pr_commits` are counted — anything blamed to a pre-PR
/// commit is dropped (those don't get a modal row).
///
/// Every PR commit gets an entry, even if it has no changes in this file
/// (so the caller can sum across files without losing zero-change commits).
pub fn commit_stats_for_file(
    pr_commits: &[String],
    head_blame: &Blame,
    delete_text_to_sha: &HashMap<String, String>,
) -> HashMap<String, CommitStats> {
    let mut stats: HashMap<String, CommitStats> = pr_commits
        .iter()
        .map(|oid| (oid.clone(), CommitStats::default()))
        .collect();
    for sha in &head_blame.line_shas {
        if sha.is_empty() {
            continue;
        }
        if let Some(s) = stats.get_mut(sha) {
            s.adds += 1;
        }
    }
    for sha in delete_text_to_sha.values() {
        if let Some(s) = stats.get_mut(sha) {
            s.dels += 1;
        }
    }
    stats
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test --quiet commit_stats_counts_adds_and_dels_for_pr_commits commit_stats_includes_zero_entries_for_pr_commits_without_changes`
Expected: PASS.

- [ ] **Step 5: Run the full attribution test set**

Run: `cargo test --quiet render::attribution::`
Expected: All existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/render/attribution.rs
git commit -m "feat(render): add per-commit add/del stats helper"
```

---

### Task 4: Store `commit_stats` on `PrPackage` and populate in `build_package`

**Files:**
- Modify: `src/data/cache.rs:11-17` (`PrPackage` struct)
- Modify: `src/data/cache.rs:62-68` (`empty_pkg` helper in tests)
- Modify: `src/data/worker.rs:144-180` (`build_package`)
- Test: `src/data/worker.rs` tests module
- Test: `src/view/pr_review.rs` tests module (fixture builder needs the new field)

- [ ] **Step 1: Add a failing test in `worker.rs`**

In `src/data/worker.rs` tests module, append:

```rust
    #[test]
    fn build_package_populates_commit_stats() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();

        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(
            detail.number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha, "src/sched.rs".into()), porcelain);

        let pkg = build_package(&gh, &git, Path::new("/tmp/repo"), detail.number, 7).unwrap();

        // Every PR commit gets an entry, even if it didn't touch any tracked file.
        for c in &detail.commits {
            assert!(
                pkg.commit_stats.contains_key(&c.oid),
                "missing stats entry for commit {}",
                c.oid,
            );
        }
        // Sanity: at least one commit has nonzero adds (the basic fixture
        // includes head-blame entries).
        assert!(
            pkg.commit_stats.values().any(|s| s.adds > 0),
            "expected at least one commit with adds > 0",
        );
    }
```

- [ ] **Step 2: Run the test and confirm failure**

Run: `cargo test --quiet build_package_populates_commit_stats`
Expected: FAIL — `pkg.commit_stats` doesn't exist.

- [ ] **Step 3: Add the field to `PrPackage`**

In `src/data/cache.rs`, change the struct and the test helper:

```rust
use std::collections::HashMap;

use crate::data::diff::FileDiff;
use crate::data::pr::{Pr, PrDetail};
use crate::render::attribution::{CommitStats, LineColors};

#[derive(Debug, Clone)]
pub struct PrPackage {
    pub detail: PrDetail,
    pub files: Vec<FileDiff>,
    /// Indexed by file path.
    pub colors: HashMap<String, LineColors>,
    /// Per-commit-OID add/delete counts, summed across all files. One
    /// entry per PR commit, even commits that touched no files.
    pub commit_stats: HashMap<String, CommitStats>,
}
```

In the same file, find `empty_pkg` in the `#[cfg(test)]` block and add `commit_stats: HashMap::new()`:

```rust
    fn empty_pkg(detail: PrDetail) -> PrPackage {
        PrPackage {
            detail,
            files: vec![],
            colors: HashMap::new(),
            commit_stats: HashMap::new(),
        }
    }
```

- [ ] **Step 4: Populate `commit_stats` in `build_package`**

In `src/data/worker.rs`, change the imports near the top:

```rust
use crate::render::attribution::{attribute_file, commit_stats_for_file, CommitStats};
```

Then change `build_package` (lines 134-180) so that after the per-file loop, it accumulates stats. The full updated function:

```rust
pub fn build_package(
    gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    number: u32,
    window_size: usize,
) -> Result<PrPackage> {
    let detail = gh.view_pr(repo_root, number)?;
    git.fetch_pr(repo_root, number)
        .with_context(|| format!("fetching PR #{number}"))?;
    let raw = gh.diff_pr(repo_root, number)?;
    let files = parse_diff(&raw)?;

    let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();
    let mut colors = HashMap::new();
    let mut commit_stats: HashMap<String, CommitStats> = commits
        .iter()
        .map(|oid| (oid.clone(), CommitStats::default()))
        .collect();
    for f in &files {
        if f.binary {
            continue;
        }
        let head = git
            .blame(repo_root, &detail.head_ref_oid, &f.path)
            .map(|s| parse_blame(&s))
            .unwrap_or_else(|_| Blame { line_shas: vec![] });
        let log_out = git
            .log_patches(
                repo_root,
                &detail.base_ref_oid,
                &detail.head_ref_oid,
                &f.path,
            )
            .unwrap_or_default();
        let deletes = parse_deletions(&log_out);
        let lc = attribute_file(&commits, window_size, &head, &deletes);
        colors.insert(f.path.clone(), lc);

        let per_file = commit_stats_for_file(&commits, &head, &deletes);
        for (oid, s) in per_file {
            let entry = commit_stats.entry(oid).or_default();
            entry.adds += s.adds;
            entry.dels += s.dels;
        }
    }

    Ok(PrPackage {
        detail,
        files,
        colors,
        commit_stats,
    })
}
```

- [ ] **Step 5: Update the `pr_review.rs` test fixture builder**

In `src/view/pr_review.rs`, find `fixture_pkg()` in the `#[cfg(test)]` block and add `commit_stats: HashMap::new()`:

```rust
    fn fixture_pkg() -> PrPackage {
        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let files = parse_diff(include_str!("../../tests/fixtures/diff_basic.patch")).unwrap();
        PrPackage {
            detail,
            files,
            colors: HashMap::new(),
            commit_stats: HashMap::new(),
        }
    }
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test --quiet`
Expected: All tests pass, including `build_package_populates_commit_stats`.

- [ ] **Step 7: Commit**

```bash
git add src/data/cache.rs src/data/worker.rs src/view/pr_review.rs
git commit -m "feat(data): compute and cache per-commit stats in PrPackage"
```

---

### Task 5: New `commits_modal` module — state, rows, `relative_date`

**Files:**
- Create: `src/view/commits_modal.rs`
- Modify: `src/view/mod.rs`
- Test: `src/view/commits_modal.rs` tests module

This task only adds the module skeleton, the state types, and the `relative_date` pure helper. The `render` function is added in Task 6 so each commit stays small.

- [ ] **Step 1: Register the module**

In `src/view/mod.rs`, add the new module declaration alphabetically:

```rust
pub mod commits_modal;
pub mod file_picker;
pub mod help;
pub mod merge_modal;
pub mod pr_list;
pub mod pr_review;
```

- [ ] **Step 2: Write the failing tests for the new module**

Create `src/view/commits_modal.rs` with **only** the test module first (so we can confirm the symbols are missing):

```rust
//! Commits modal: read-only vertical list of the PR's commits.
//!
//! Triggered by `c` from the review view. Display-only — selection is
//! visual; Enter/Esc/c just close.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::style::Color;

use crate::render::attribution::CommitStats;

#[derive(Debug, Clone)]
pub struct CommitRow {
    pub color: Color,
    pub short_sha: String,
    pub headline: String,
    pub author: String,
    pub relative_date: String,
    pub adds: u32,
    pub dels: u32,
}

#[derive(Debug, Default)]
pub struct CommitsModalState {
    pub rows: Vec<CommitRow>,
    pub selected: usize,
}

impl CommitsModalState {
    pub fn move_down(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        if self.selected < last {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Build modal rows from PR detail + cached stats. The palette is built
/// the same way the diff body does (`assign_commit_colors`).
pub fn build_rows(
    pr_commits: &[crate::data::pr::Commit],
    stats: &HashMap<String, CommitStats>,
    palette_window: usize,
    now: DateTime<Utc>,
) -> Vec<CommitRow> {
    let oids: Vec<String> = pr_commits.iter().map(|c| c.oid.clone()).collect();
    let palette = crate::render::color::assign_commit_colors(&oids, palette_window);
    pr_commits
        .iter()
        .map(|c| {
            let s = stats.get(&c.oid).copied().unwrap_or_default();
            CommitRow {
                color: palette
                    .get(&c.oid)
                    .copied()
                    .unwrap_or(crate::render::style::OLDER_COMMIT),
                short_sha: c.oid.chars().take(6).collect(),
                headline: c.message_headline.clone(),
                author: c
                    .authors
                    .first()
                    .map(|a| a.login.clone())
                    .unwrap_or_default(),
                relative_date: relative_date(now, c.committed_date),
                adds: s.adds,
                dels: s.dels,
            }
        })
        .collect()
}

/// Format a commit date as a short relative string. Returns "—" for None.
pub fn relative_date(now: DateTime<Utc>, then: Option<DateTime<Utc>>) -> String {
    let Some(then) = then else {
        return "—".into();
    };
    let secs = now.signed_duration_since(then).num_seconds();
    if secs < 60 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format!("{weeks}w");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo");
    }
    let years = days / 365;
    format!("{years}y")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn t(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[test]
    fn relative_date_buckets() {
        let now = t(2026, 5, 6, 12);
        assert_eq!(relative_date(now, None), "—");
        assert_eq!(relative_date(now, Some(now)), "just now");
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::minutes(5))),
            "5m"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::hours(2))),
            "2h"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(3))),
            "3d"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(14))),
            "2w"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(60))),
            "2mo"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(800))),
            "2y"
        );
    }

    #[test]
    fn move_down_clamps_at_bottom() {
        let mut st = CommitsModalState {
            rows: vec![
                dummy_row(),
                dummy_row(),
                dummy_row(),
            ],
            selected: 2,
        };
        st.move_down();
        assert_eq!(st.selected, 2);
    }

    #[test]
    fn move_up_clamps_at_top() {
        let mut st = CommitsModalState {
            rows: vec![dummy_row()],
            selected: 0,
        };
        st.move_up();
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn move_up_and_down_in_middle() {
        let mut st = CommitsModalState {
            rows: vec![dummy_row(), dummy_row(), dummy_row()],
            selected: 1,
        };
        st.move_down();
        assert_eq!(st.selected, 2);
        st.move_up();
        st.move_up();
        assert_eq!(st.selected, 0);
    }

    fn dummy_row() -> CommitRow {
        CommitRow {
            color: Color::White,
            short_sha: "abc123".into(),
            headline: "x".into(),
            author: "a".into(),
            relative_date: "1d".into(),
            adds: 0,
            dels: 0,
        }
    }
}
```

- [ ] **Step 3: Run tests for the new module**

Run: `cargo test --quiet view::commits_modal::`
Expected: PASS — relative_date and selection clamping tests pass.

- [ ] **Step 4: Run full suite to confirm no regressions**

Run: `cargo test --quiet`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/view/mod.rs src/view/commits_modal.rs
git commit -m "feat(view): add commits modal state and row builder"
```

---

### Task 6: `commits_modal::render`

**Files:**
- Modify: `src/view/commits_modal.rs` — add the `render` function and a TestBackend test.

- [ ] **Step 1: Write a failing test for `render`**

Append to the `tests` module in `src/view/commits_modal.rs`:

```rust
    #[test]
    fn render_draws_one_row_per_commit() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let st = CommitsModalState {
            rows: vec![
                CommitRow {
                    color: Color::Red,
                    short_sha: "abc123".into(),
                    headline: "first commit".into(),
                    author: "alice".into(),
                    relative_date: "3d".into(),
                    adds: 5,
                    dels: 1,
                },
                CommitRow {
                    color: Color::Green,
                    short_sha: "def456".into(),
                    headline: "second commit".into(),
                    author: "bob".into(),
                    relative_date: "2d".into(),
                    adds: 12,
                    dels: 0,
                },
            ],
            selected: 1,
        };

        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();

        let dump: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();

        assert!(dump.contains("abc123"), "missing first sha:\n{dump}");
        assert!(dump.contains("def456"), "missing second sha:\n{dump}");
        assert!(dump.contains("first commit"), "missing first headline:\n{dump}");
        assert!(dump.contains("second commit"), "missing second headline:\n{dump}");
        assert!(dump.contains("alice"), "missing author:\n{dump}");
        assert!(dump.contains("+5"), "missing adds:\n{dump}");
        assert!(dump.contains("commits"), "missing title:\n{dump}");
    }

    #[test]
    fn render_highlights_selected_row() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use crate::render::style::SURFACE0;

        let st = CommitsModalState {
            rows: vec![dummy_row(), dummy_row()],
            selected: 1,
        };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();

        // Find a row that contains the selected highlight bg. We don't
        // hard-code the row index because the modal is centered.
        let mut found_highlighted = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].style().bg == Some(SURFACE0) {
                    found_highlighted = true;
                }
            }
        }
        assert!(found_highlighted, "no cell with SURFACE0 bg found");
    }
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test --quiet render_draws_one_row_per_commit render_highlights_selected_row`
Expected: FAIL — `render` doesn't exist.

- [ ] **Step 3: Add the `render` function**

In `src/view/commits_modal.rs`, add the imports near the top of the file (alongside existing imports):

```rust
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::style::*;
```

Then add the function (place it before `pub fn build_rows`):

```rust
/// Centered ~60% × 60% overlay listing the PR's commits, one per row.
pub fn render(f: &mut Frame, area: Rect, st: &CommitsModalState) {
    let modal = centered(area, 60, 60);
    f.render_widget(Clear, modal);

    let lines: Vec<Line> = st
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let row_style = if i == st.selected {
                Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(vec![
                Span::styled(" █ ", Style::default().fg(r.color)),
                Span::styled(format!("{}  ", r.short_sha), Style::default().fg(SUBTEXT0)),
                Span::styled(truncate(&r.headline, 36), row_style),
                Span::styled(
                    format!("  {} · {}  ", r.author, r.relative_date),
                    Style::default().fg(OVERLAY1),
                ),
                Span::styled(format!("+{}", r.adds), Style::default().fg(GREEN)),
                Span::raw(" "),
                Span::styled(format!("−{}", r.dels), Style::default().fg(RED)),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(" commits ");
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = (area.width - w) / 2 + area.x;
    let y = (area.height - h) / 2 + area.y;
    Rect::new(x, y, w, h)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut)
    }
}
```

Note: `GREEN`, `RED`, `SUBTEXT0`, `SURFACE0`, `SURFACE2`, `OVERLAY1`, `TEXT` come from `crate::render::style`. Verify they exist by running:

`grep -E "pub const (GREEN|RED|SUBTEXT0|SURFACE0|SURFACE2|OVERLAY1|TEXT) " src/render/style.rs`

If any are missing, look in the same file for the equivalent constant (the palette uses Catppuccin names) and substitute. Do not invent new constants.

- [ ] **Step 4: Run tests to confirm pass**

Run: `cargo test --quiet view::commits_modal::`
Expected: PASS — all four tests in the module pass.

- [ ] **Step 5: Run the full suite**

Run: `cargo test --quiet`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/view/commits_modal.rs
git commit -m "feat(view): render commits modal overlay"
```

---

### Task 7: Wire the modal into key dispatch + app state (modal viable; strip still in place)

This task makes `c` open the modal, but **leaves the strip rendering in place** for one commit so each diff stays focused. After this commit, `c` will both open the new modal **and** the strip will continue to render at the top — that's intentional and only true for the duration of one commit, removed in Task 8.

Wait — actually, since `c` was already bound to `ToggleCommitStrip`, we need to either pick one or temporarily route `c` to the new modal. Cleanest: this task introduces a fresh key for testing (`C` shift) which exercises the wiring; Task 8 then rebinds lowercase `c` and deletes the strip in one shot.

**Files:**
- Modify: `src/keys.rs` — add `Action::OpenCommitsModal`, `FocusedView::CommitsModal`; bind `C` (capital) temporarily; keep `ToggleCommitStrip` and lowercase `c` binding for now.
- Modify: `src/app.rs` — add `commits: Option<CommitsModalState>` to `AppState`, action handler, `handle_commits_modal`, draw branch.

- [ ] **Step 1: Add the test harness for the new key**

In `src/keys.rs` `tests` module, append:

```rust
    #[test]
    fn review_capital_c_opens_commits_modal() {
        assert_eq!(
            dispatch(FocusedView::Review, k('C')),
            Action::OpenCommitsModal,
        );
    }
```

- [ ] **Step 2: Confirm failure**

Run: `cargo test --quiet review_capital_c_opens_commits_modal`
Expected: FAIL — variant doesn't exist.

- [ ] **Step 3: Add the variants and binding**

In `src/keys.rs`:

1. Add to the `Action` enum (in the `// PR review` group):
   ```rust
       OpenCommitsModal,
   ```

2. Add to the `FocusedView` enum:
   ```rust
       CommitsModal,
   ```

3. In `dispatch`, treat the new variant the same as `FilePicker | MergeModal` (overlay's own handler):
   ```rust
       FocusedView::FilePicker | FocusedView::MergeModal | FocusedView::CommitsModal => Action::Nothing,
   ```

4. In the `review` function, before the existing `KeyCode::Char('c')` arm, add:
   ```rust
       KeyCode::Char('C') => Action::OpenCommitsModal,
   ```

- [ ] **Step 4: Confirm the keys test passes**

Run: `cargo test --quiet review_capital_c_opens_commits_modal`
Expected: PASS.

- [ ] **Step 5: Wire `AppState` and the handler in `app.rs`**

In `src/app.rs`:

1. Add the import at the top:
   ```rust
   use crate::view::commits_modal::{self, CommitsModalState};
   ```

2. Add a field to `AppState` (line 64-74):
   ```rust
   pub commits: Option<CommitsModalState>,
   ```
   And initialize it in `AppState::new`:
   ```rust
   commits: None,
   ```

3. In `handle_key`, immediately after the `if st.focused == FocusedView::MergeModal` block, add:
   ```rust
   if st.focused == FocusedView::CommitsModal {
       handle_commits_modal(st, ev);
       return;
   }
   ```

4. Add the new `Action::OpenCommitsModal` arm inside the action match (place it next to `OpenFilePicker`):
   ```rust
   Action::OpenCommitsModal => {
       if let (Some(num), Some(_)) = (st.current_pr, st.review.as_ref())
           && let Some(pkg) = app.cache.get(num)
       {
           let rows = commits_modal::build_rows(
               &pkg.detail.commits,
               &pkg.commit_stats,
               app.config.window_size,
               Utc::now(),
           );
           st.commits = Some(CommitsModalState { rows, selected: 0 });
           st.focused = FocusedView::CommitsModal;
       }
   }
   ```

5. Add the modal-key handler at the bottom of the file (next to `handle_file_picker`):
   ```rust
   fn handle_commits_modal(st: &mut AppState, ev: crossterm::event::KeyEvent) {
       use crossterm::event::KeyCode;
       let Some(modal) = st.commits.as_mut() else {
           return;
       };
       match ev.code {
           KeyCode::Esc | KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('C') => {
               st.commits = None;
               st.focused = FocusedView::Review;
           }
           KeyCode::Down | KeyCode::Char('j') => modal.move_down(),
           KeyCode::Up | KeyCode::Char('k') => modal.move_up(),
           _ => {}
       }
   }
   ```

6. In the `draw` function, add a branch after the merge/picker overlays (before the help-overlay branch):
   ```rust
   if let Some(c) = &st.commits {
       crate::view::commits_modal::render(f, area, c);
   }
   ```

- [ ] **Step 6: Run all tests + build**

Run: `cargo build` and `cargo test --quiet`
Expected: Build succeeds; all tests pass.

- [ ] **Step 7: Manual smoke test**

Build and run: `cargo run` against a real repo (or any prpr-friendly working dir). Open a PR, press **`C`** (capital), confirm the modal appears with one row per commit, j/k moves the selection, Esc/Enter/c closes. The horizontal strip at the top should still be present (we haven't removed it yet).

If the modal doesn't render because the cache hasn't loaded the package yet, wait a beat and try again.

- [ ] **Step 8: Commit**

```bash
git add src/keys.rs src/app.rs
git commit -m "feat(app): wire commits modal under temporary C binding"
```

---

### Task 8: Remove the strip; rebind lowercase `c`; delete `ToggleCommitStrip`

**Files:**
- Modify: `src/keys.rs` — remove `Action::ToggleCommitStrip`; replace temporary `C` binding by binding lowercase `c` to `OpenCommitsModal`.
- Modify: `src/view/pr_review.rs` — delete `render_commit_strip`, `show_commit_strip` field, strip layout slot, `c strip` hint token, helpers that became dead.
- Modify: `src/app.rs` — drop the `Action::ToggleCommitStrip` match arm and the `show_commit_strip: app.config.show_commit_strip` PrReviewState init.

- [ ] **Step 1: Update the keys test**

In `src/keys.rs` `tests` module, change `review_capital_c_opens_commits_modal` to use lowercase, and add a regression test:

```rust
    #[test]
    fn review_c_opens_commits_modal() {
        assert_eq!(
            dispatch(FocusedView::Review, k('c')),
            Action::OpenCommitsModal,
        );
    }
```

Delete the old `review_capital_c_opens_commits_modal` test (or change `'C'` to `'c'`).

- [ ] **Step 2: Add a failing pr_review regression test**

In `src/view/pr_review.rs` tests module, append:

```rust
    #[test]
    fn renders_no_commit_strip() {
        let pkg = fixture_pkg();
        let st = PrReviewState::default();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();
        // No row should render the old "commits  " label.
        for y in 0..buf.area.height {
            let row = buffer_line(buf, y);
            assert!(
                !row.starts_with("  commits  "),
                "row {y} unexpectedly rendered the commit strip: {row:?}",
            );
        }
    }
```

- [ ] **Step 3: Confirm the new tests fail**

Run: `cargo test --quiet review_c_opens_commits_modal renders_no_commit_strip`
Expected: FAIL on `review_c_opens_commits_modal` (binding still says `Action::ToggleCommitStrip`); FAIL on `renders_no_commit_strip` (strip still renders when `show_commit_strip` is true on default).

- [ ] **Step 4: Rebind `c` and delete `ToggleCommitStrip`**

In `src/keys.rs`:

1. Remove `ToggleCommitStrip` from the `Action` enum.
2. In the `review` function, change:
   ```rust
       KeyCode::Char('c') => Action::ToggleCommitStrip,
   ```
   to:
   ```rust
       KeyCode::Char('c') => Action::OpenCommitsModal,
   ```
3. Delete the temporary `KeyCode::Char('C') => Action::OpenCommitsModal` line added in Task 7 (lowercase now covers it; Shift-c is unbound and falls through to `Action::Nothing`).

In `src/app.rs`:
1. Delete the `Action::ToggleCommitStrip => { … }` match arm.
2. In the `Action::ListOpen` handler, delete the line:
   ```rust
   show_commit_strip: app.config.show_commit_strip,
   ```
   (Coming up empty in PrReviewState init is fine — the field is removed in the next step.)

- [ ] **Step 5: Delete strip rendering from `pr_review.rs`**

In `src/view/pr_review.rs`:

1. Remove the `show_commit_strip: bool` field from `PrReviewState`:
   ```rust
   #[derive(Debug, Default)]
   pub struct PrReviewState {
       pub file_index: usize,
       pub cursor_line: usize,
       pub scroll: u16,
       pub show_sha_margin: bool,
       pub status: String,
   }
   ```

2. In `pub fn render`, remove the `strip_h` calculation and the strip slot from the layout:
   ```rust
   pub fn render(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
       let chunks = Layout::default()
           .direction(Direction::Vertical)
           .constraints([
               Constraint::Length(1), // header
               Constraint::Length(2), // file bar (title + divider)
               Constraint::Min(1),    // diff body
               Constraint::Length(3), // status (cursor + 2 hint rows)
           ])
           .split(area);

       render_header(f, chunks[0], pkg);
       render_file_bar(f, chunks[1], pkg, st);
       render_diff_body(f, chunks[2], pkg, st);
       render_status(f, chunks[3], pkg, st);
   }
   ```

3. Delete the `render_commit_strip` function entirely.

4. In `render_status`, change the second hint row text from:
   ```
   "  Tab/↵ next file   Shift-Tab prev   f files   m merge   c strip   s sha   ? help   q back",
   ```
   to:
   ```
   "  Tab/↵ next file   Shift-Tab prev   f files   c commits   m merge   s sha   ? help   q back",
   ```

5. The `truncate` and `short_sha` helpers in this file are now unused — delete both (they're recreated locally in `commits_modal.rs`).

6. Update the existing tests:
   - `renders_pr_number_in_header` — delete the `show_commit_strip: false` line (no longer a field).
   - `binary_file_renders_placeholder` — same. The body row index stays at row 3 (header 1 + file_bar 2 → body starts at 3), so the existing assertion still holds.

7. Remove the now-unused imports in `src/view/pr_review.rs`. Specifically:
   ```rust
   use ratatui::text::{Line, Span};
   ```
   — `Line` is still needed by `body_lines`, but `Span` was only used by the strip. Run the build; the compiler will tell you which imports to drop.

   Also drop:
   ```rust
   use crate::render::color::assign_commit_colors;
   ```
   — only used by `render_commit_strip`.

- [ ] **Step 6: Run tests + build**

Run: `cargo build`
Expected: Builds clean. If the compiler reports unused imports, remove them.

Run: `cargo test --quiet`
Expected: All tests pass, including the new `renders_no_commit_strip` and `review_c_opens_commits_modal`.

- [ ] **Step 7: Manual verification**

Run: `cargo run`
- Open a PR; the top of the screen should no longer show a commit strip — the file bar should be immediately under the header.
- Press `c`; the commits modal opens.
- Press j/k to navigate; Esc/Enter/c to close.
- Press `s`; the SHA-margin toggle should still work (it's not affected).

- [ ] **Step 8: Commit**

```bash
git add src/keys.rs src/app.rs src/view/pr_review.rs
git commit -m "feat(app): bind c to commits modal, remove commit strip"
```

---

### Task 9: Remove `show_commit_strip` from `Config` and update help text

**Files:**
- Modify: `src/config.rs` — remove `show_commit_strip` from `Config` and `RawUi`; update tests.
- Modify: `src/view/help.rs` — update `HELP_TEXT`.

- [ ] **Step 1: Remove the field from `Config`**

In `src/config.rs`:

1. In `pub struct Config`, delete:
   ```rust
       pub show_commit_strip: bool,
   ```

2. In `impl Default for Config`, delete:
   ```rust
       show_commit_strip: true,
   ```

3. In `struct RawUi`, delete:
   ```rust
       #[serde(default)]
       show_commit_strip: Option<bool>,
   ```

4. In `fn merge`, delete the `if let Some(b) = raw.ui.show_commit_strip { ... }` block.

- [ ] **Step 2: Update the tests**

In `src/config.rs` tests:

1. In `parses_full_toml`, drop the `show_commit_strip = false` line from the toml literal and the matching `assert_eq!(cfg.show_commit_strip, false);`.

2. In `partial_toml_only_overrides_present_keys`, no change needed unless it referenced `show_commit_strip`.

3. Add (just before `empty_toml_yields_defaults`):
   ```rust
       #[test]
       fn unknown_show_commit_strip_key_is_ignored() {
           // Stale configs from before the strip removal must keep parsing.
           let toml = "[ui]\nshow_commit_strip = false\n";
           let raw: RawConfig = toml::from_str(toml).unwrap();
           let cfg = merge(Config::default(), raw);
           assert_eq!(cfg, Config::default());
       }
   ```

   This test guards the user's existing config files against breakage. `serde` ignores unknown keys by default, so removing the field doesn't reject old TOML.

- [ ] **Step 3: Update `help.rs`**

In `src/view/help.rs`, replace the line:
```rust
    "    c            toggle commit strip",
```
with:
```rust
    "    c            commits modal",
```

- [ ] **Step 4: Run tests + build**

Run: `cargo build`
Expected: Builds clean.

Run: `cargo test --quiet`
Expected: All tests pass.

- [ ] **Step 5: Final manual verification**

Run: `cargo run`
- Press `?` in the review view. The help overlay should list `c · commits modal` (and no longer `c · toggle commit strip`).
- If you have a `~/.config/prpr/config.toml` with `show_commit_strip = ...`, confirm the app still starts cleanly (the unknown key is ignored).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/view/help.rs
git commit -m "feat(config): drop show_commit_strip; update help"
```

---

## Self-review checklist

Before declaring the plan complete, walk back through the spec and confirm every requirement maps to a task:

- Modal state, rows, render → Tasks 5, 6.
- Trigger key `c` opens modal → Task 8.
- Display-only (Esc/Enter/c close) → Task 7 step 5 (the handler).
- Per-commit `+adds −dels` from raw blame + delete-map → Tasks 3, 4.
- `committedDate` from gh + Optional fallback → Tasks 1, 2.
- Modal contents: color square + short SHA + headline + author + relative date + stats → Task 5 (`build_rows`) + Task 6 (`render`).
- Commit order = `detail.commits` order → Task 5 `build_rows` (preserves input order).
- No-op when PR not loaded yet → Task 7 step 5 (`Action::OpenCommitsModal` arm gates on `app.cache.get(num)`).
- Removal of `render_commit_strip`, `show_commit_strip`, `ToggleCommitStrip`, layout slot, `c strip` token → Task 8.
- `Config::show_commit_strip` removed → Task 9.
- Help text updated → Task 9.
- Existing test `binary_file_renders_placeholder` body row stays at 3 → Task 8 step 5 verifies the new layout.
- Stale TOML keys keep parsing → Task 9 step 2.

If executing in subagent-driven mode, dispatch one subagent per task with the spec attached.
