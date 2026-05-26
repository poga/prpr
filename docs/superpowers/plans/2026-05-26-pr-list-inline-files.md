# PR list — inline files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the currently-selected PR's changed files inline in the PR list (path + per-file `+adds -dels`), read from local refs on every selection. Drop closed/merged PR support along the way.

**Architecture:** A new `GitClient::diff_numstat` reads file deltas from the locally-fetched PR ref against `origin/<base>`. The worker exposes it via `Request::ListFiles`/`Response::ListFiles`. `PrListState` gains an `expanded` field tagged with the PR number; the renderer slots the expanded block under the selected row, and a small viewport offset keeps the selected row visible when the block is tall.

**Tech Stack:** Rust, ratatui (TestBackend), `git diff --numstat`, existing `Worker`/`GhClient`/`GitClient` trait pattern.

**Spec:** `docs/superpowers/specs/2026-05-26-pr-list-inline-files-design.md`

---

## File map

**Modify:**
- `src/data/git.rs` — add `diff_numstat` trait method + `GitCli` impl + `FakeGit` field/impl
- `src/data/pr.rs` — no struct changes (reuse `FileMeta`); add defensive non-`OPEN` filter helper
- `src/data/gh.rs` — `--state all` → `--state open`; filter parsed rows defensively
- `src/data/worker.rs` — add `Request::ListFiles` and `Response::ListFiles`; handler
- `src/view/pr_list.rs` — add `ExpandedFiles` enum + `expanded` field; remove `filter_open_only`; render expanded block; viewport scroll
- `src/app.rs` — `after_selection_change` helper, wire into ListUp/Down/Top/Bottom, `ListFast` arrival, refresh; handle `Response::ListFiles`; remove `Action::ListCycleFilter` arm and `filter_open_only` init
- `src/keys.rs` — remove `Action::ListCycleFilter` variant and `f` binding in list scope
- `src/view/help.rs` — drop `f cycle filter` line

**Create:**
- `tests/fixtures/diff_numstat.txt` — parser fixture

---

## Task 1: `diff_numstat` parser (pure)

The parser is straight string work; isolate it first so the trait method is trivial later.

**Files:**
- Modify: `src/data/git.rs` (add a private free function + tests)
- Create: `tests/fixtures/diff_numstat.txt`

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/diff_numstat.txt` with the exact bytes (tabs between columns, LF line endings):

```
12	3	src/sched.rs
85	0	tests/metrics_test.rs
-	-	assets/logo.png
0	42	docs/old_metrics.md
3	1	src/server.rs
```

- [ ] **Step 2: Add the failing parser test**

Add at the bottom of `src/data/git.rs`, inside the existing `#[cfg(test)] mod tests` block (or create one if missing — there isn't one today at the file level; add it):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pr::FileMeta;

    #[test]
    fn parses_numstat_with_text_binary_rename_pure_delete() {
        let raw = include_str!("../../tests/fixtures/diff_numstat.txt");
        let got = parse_numstat(raw).unwrap();
        assert_eq!(
            got,
            vec![
                FileMeta { path: "src/sched.rs".into(), additions: 12, deletions: 3 },
                FileMeta { path: "tests/metrics_test.rs".into(), additions: 85, deletions: 0 },
                FileMeta { path: "assets/logo.png".into(), additions: 0, deletions: 0 },
                FileMeta { path: "docs/old_metrics.md".into(), additions: 0, deletions: 42 },
                FileMeta { path: "src/server.rs".into(), additions: 3, deletions: 1 },
            ],
        );
    }

    #[test]
    fn parses_numstat_empty_input() {
        assert!(parse_numstat("").unwrap().is_empty());
    }

    #[test]
    fn parse_numstat_skips_blank_lines() {
        let raw = "1\t1\ta.rs\n\n2\t0\tb.rs\n";
        let got = parse_numstat(raw).unwrap();
        assert_eq!(got.len(), 2);
    }
}
```

- [ ] **Step 3: Run to verify failure**

`cargo test -p prpr parse_numstat 2>&1 | tail -10`
Expected: compile error — `parse_numstat` doesn't exist.

- [ ] **Step 4: Implement `parse_numstat`**

Add to `src/data/git.rs` (above the `pub trait GitClient` line; it's a free function):

```rust
/// Parse the output of `git diff --numstat`. Each non-blank line is
/// `<adds>\t<dels>\t<path>` where `-\t-` means binary (counted as 0/0).
fn parse_numstat(raw: &str) -> Result<Vec<crate::data::pr::FileMeta>> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let a = parts.next().unwrap_or("");
        let d = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            return Err(anyhow!("malformed numstat line: {line:?}"));
        }
        let additions = if a == "-" { 0 } else {
            a.parse::<u32>().with_context(|| format!("parsing adds in {line:?}"))?
        };
        let deletions = if d == "-" { 0 } else {
            d.parse::<u32>().with_context(|| format!("parsing dels in {line:?}"))?
        };
        out.push(crate::data::pr::FileMeta { path: path.to_string(), additions, deletions });
    }
    Ok(out)
}
```

- [ ] **Step 5: Run tests to verify pass**

`cargo test -p prpr --lib parse_numstat 2>&1 | tail -10`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/data/git.rs tests/fixtures/diff_numstat.txt
git commit -m "feat(git): parse \`git diff --numstat\` output"
```

---

## Task 2: `diff_numstat` trait method + GitCli + FakeGit

**Files:**
- Modify: `src/data/git.rs`

- [ ] **Step 1: Write failing FakeGit test**

Add to the `tests` module in `src/data/git.rs`:

```rust
    #[test]
    fn fake_git_diff_numstat_returns_seeded_value() {
        use crate::data::git::fakes::FakeGit;
        use std::path::Path;
        let mut g = FakeGit::new("/tmp/repo");
        g.numstats.insert(
            ("base".into(), "head".into()),
            vec![crate::data::pr::FileMeta { path: "a.rs".into(), additions: 1, deletions: 2 }],
        );
        let v = g.diff_numstat(Path::new("/tmp/repo"), "base", "head").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].path, "a.rs");
    }

    #[test]
    fn fake_git_diff_numstat_errors_when_missing() {
        use crate::data::git::fakes::FakeGit;
        use std::path::Path;
        let g = FakeGit::new("/tmp/repo");
        let r = g.diff_numstat(Path::new("/tmp/repo"), "base", "head");
        assert!(r.is_err());
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib fake_git_diff_numstat 2>&1 | tail -10`
Expected: compile error — `diff_numstat` not in trait, `numstats` not on `FakeGit`.

- [ ] **Step 3: Add trait method**

In `src/data/git.rs`, in the `pub trait GitClient` block, add after `fn log_patches(...)`:

```rust
    /// `git diff --numstat <base>..<head>`. Returns one `FileMeta` per
    /// changed file (binary files yield 0/0). Used by the PR list's
    /// inline files view, never hits the network.
    fn diff_numstat(
        &self,
        repo_root: &Path,
        base: &str,
        head: &str,
    ) -> Result<Vec<crate::data::pr::FileMeta>>;
```

- [ ] **Step 4: Implement on GitCli**

In the `impl GitClient for GitCli` block, add after `fn log_patches`:

```rust
    fn diff_numstat(
        &self,
        repo_root: &Path,
        base: &str,
        head: &str,
    ) -> Result<Vec<crate::data::pr::FileMeta>> {
        let range = format!("{base}..{head}");
        let out = run(Command::new("git").current_dir(repo_root).args([
            "diff", "--numstat", "--no-color", &range,
        ]))?;
        let raw = String::from_utf8(out.stdout)
            .with_context(|| "`git diff --numstat` returned non-UTF-8")?;
        parse_numstat(&raw)
    }
```

- [ ] **Step 5: Implement on FakeGit**

In `pub(crate) mod fakes`:

Add a new field to `pub struct FakeGit`, right after `log_patches: HashMap<(String, String, String), String>,`:

```rust
        /// Keyed by (base, head) → numstat file list.
        pub numstats: HashMap<(String, String), Vec<crate::data::pr::FileMeta>>,
```

In `impl FakeGit::new`, add to the struct literal (alphabetical-ish order, last):

```rust
                numstats: HashMap::new(),
```

In `impl GitClient for FakeGit`, add at the end:

```rust
        fn diff_numstat(
            &self,
            _root: &Path,
            base: &str,
            head: &str,
        ) -> Result<Vec<crate::data::pr::FileMeta>> {
            self.numstats
                .get(&(base.into(), head.into()))
                .cloned()
                .ok_or_else(|| anyhow!("no fake numstat for {base}..{head}"))
        }
```

- [ ] **Step 6: Run tests to verify pass**

`cargo test -p prpr --lib 2>&1 | tail -5`
Expected: all pass (158 + 2 new = 160).

- [ ] **Step 7: Commit**

```bash
git add src/data/git.rs
git commit -m "feat(git): GitClient::diff_numstat"
```

---

## Task 3: Drop closed/merged PR support

**Files:**
- Modify: `src/data/gh.rs`, `src/view/pr_list.rs`, `src/app.rs`, `src/keys.rs`, `src/view/help.rs`

- [ ] **Step 1: Write the failing defensive-filter test**

Add to the existing `#[cfg(test)] mod tests` block in `src/data/gh.rs`:

```rust
    #[test]
    fn fake_drops_non_open_rows_after_parse() {
        // Even if `gh` returns a non-OPEN row, list_prs_fast must filter
        // it. The fake echoes what's in `prs_fast`; the production CLI
        // does the same defensive filter after JSON parse.
        use super::fakes::FakeGh;
        use crate::data::pr::{Author, Pr, PrState};
        let mut g = FakeGh::new();
        g.prs_fast = vec![
            Pr { number: 1, title: "open".into(), is_draft: false, state: PrState::Open,
                 author: Author { login: "a".into() },
                 created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 base_ref_name: "main".into(), head_ref_name: "f".into(),
                 labels: vec![], status_check_rollup: vec![],
                 review_decision: None, mergeable: None },
            Pr { number: 2, title: "merged".into(), is_draft: false, state: PrState::Merged,
                 author: Author { login: "a".into() },
                 created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 base_ref_name: "main".into(), head_ref_name: "f2".into(),
                 labels: vec![], status_check_rollup: vec![],
                 review_decision: None, mergeable: None },
        ];
        // The fake itself needs to apply the same filter as the prod path.
        let got = super::GhClient::list_prs_fast(&g, std::path::Path::new("/x")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].number, 1);
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib fake_drops_non_open 2>&1 | tail -10`
Expected: FAIL — the fake currently returns both rows.

- [ ] **Step 3: Add the filter on the production path**

In `src/data/gh.rs`, change `list_prs_fast` and `list_prs_enriched` on `GhCli`:

Replace `"all"` with `"open"` on both calls. Then after each parse, filter:

For `list_prs_fast`:

```rust
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "open",
            "--json",
            PR_LIST_FAST_FIELDS,
        ]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (fast) output")?;
        Ok(prs.into_iter().filter(|p| p.state == crate::data::pr::PrState::Open).collect())
    }
```

For `list_prs_enriched`: keep `--state open`, no filter needed (it only carries the number + heavy fields — no state on `PrEnrichment`).

```rust
    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "open",
            "--json",
            PR_LIST_ENRICHED_FIELDS,
        ]))?;
        let v: Vec<PrEnrichment> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (enriched) output")?;
        Ok(v)
    }
```

- [ ] **Step 4: Update FakeGh to filter too**

In `src/data/gh.rs`, replace the `FakeGh::list_prs_fast` impl:

```rust
        fn list_prs_fast(&self, _root: &std::path::Path) -> Result<Vec<Pr>> {
            Ok(self
                .prs_fast
                .clone()
                .into_iter()
                .filter(|p| p.state == crate::data::pr::PrState::Open)
                .collect())
        }
```

- [ ] **Step 5: Remove `filter_open_only` from `PrListState`**

In `src/view/pr_list.rs`:

Delete the field:

```rust
    pub filter_open_only: bool,
```

Update `visible_prs`:

```rust
    pub fn visible_prs(&self) -> Vec<&Pr> {
        let q = self.search.as_deref().map(str::to_lowercase);
        self.prs
            .iter()
            .filter(|p| match &q {
                Some(s) => {
                    p.title.to_lowercase().contains(s) || p.author.login.to_lowercase().contains(s)
                }
                None => true,
            })
            .collect()
    }
```

Update `render_header`:

```rust
fn render_header(f: &mut Frame, area: Rect, st: &PrListState) {
    let visible = st.visible_prs();
    let count = visible.iter().filter(|p| p.state == PrState::Open).count();
    let header = format!(
        "  prpr · {} · {} · {} open",
        st.repo_name, st.branch, count,
    );
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(OVERLAY1)),
        area,
    );
}
```

In `fixture_state`, remove `filter_open_only: true,`.

- [ ] **Step 6: Remove `Action::ListCycleFilter`**

In `src/keys.rs`:
- Delete the `ListCycleFilter,` enum variant.
- Delete the `KeyCode::Char('f') => Action::ListCycleFilter,` line (only in the list scope; if `f` exists in the review scope it stays).

In `src/app.rs`:
- Delete the field `filter_open_only: true,` from `AppState::new`'s `PrListState` literal.
- Delete the `Action::ListCycleFilter` arm in `handle_action`:

```rust
        Action::ListCycleFilter => {
            st.list.filter_open_only = !st.list.filter_open_only;
        }
```

- [ ] **Step 7: Update footer keybinding hint**

In `src/view/pr_list.rs`, change the footer:

```rust
    f.render_widget(
        Paragraph::new("  ↵ open   o browser   m merge   r refresh   / search   q quit")
            .style(Style::default().fg(OVERLAY1)),
        chunks[0],
    );
```

(Removes `f filter`.)

- [ ] **Step 8: Update help text**

In `src/view/help.rs`, remove the `"    f            cycle filter",` line from `HELP_TEXT`.

- [ ] **Step 9: Run all tests**

`cargo test -p prpr 2>&1 | tail -15`
Expected: all pass. Any test that referenced `filter_open_only` should already be updated in the steps above. If there are stragglers, fix them inline.

If a test fails because the header no longer matches `2 open`-style assertions, the assertion is still valid — the header still contains the count. Likely zero churn.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "refactor: drop closed/merged PR support (gh --state open, no filter toggle)"
```

---

## Task 4: `ExpandedFiles` state in `PrListState`

**Files:**
- Modify: `src/view/pr_list.rs`

- [ ] **Step 1: Write the failing state test**

Add to the `#[cfg(test)] mod tests` block in `src/view/pr_list.rs`:

```rust
    #[test]
    fn expanded_files_number_accessor_works_for_all_variants() {
        let l = ExpandedFiles::Loading { number: 7 };
        let r = ExpandedFiles::Ready { number: 8, files: vec![] };
        let e = ExpandedFiles::Error { number: 9, message: "x".into() };
        assert_eq!(l.number(), 7);
        assert_eq!(r.number(), 8);
        assert_eq!(e.number(), 9);
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib expanded_files_number_accessor 2>&1 | tail -10`
Expected: compile error.

- [ ] **Step 3: Add the enum + field**

In `src/view/pr_list.rs`, add above the `PrListState` struct:

```rust
/// Inline file data for the currently selected PR. Tagged with the PR
/// number so a stale response from a previous selection is dropped.
#[derive(Debug, Clone)]
pub enum ExpandedFiles {
    Loading { number: u32 },
    Ready { number: u32, files: Vec<crate::data::pr::FileMeta> },
    Error { number: u32, message: String },
}

impl ExpandedFiles {
    pub fn number(&self) -> u32 {
        match self {
            Self::Loading { number }
            | Self::Ready { number, .. }
            | Self::Error { number, .. } => *number,
        }
    }
}
```

Add to `PrListState` (after `manual_refresh_in_flight`):

```rust
    /// Files for the currently selected PR. Cleared on every selection
    /// change and on refresh.
    pub expanded: Option<ExpandedFiles>,
```

Update `Default` — `PrListState` derives `Default` (line 15: `#[derive(Debug, Default)]`), so `Option<ExpandedFiles>` defaults to `None`. Nothing to do.

Update `fixture_state` to set the new field:

```rust
            expanded: None,
```

- [ ] **Step 4: Run tests to verify pass**

`cargo test -p prpr --lib expanded_files_number_accessor 2>&1 | tail -5`
Expected: pass. Whole-crate `cargo test` should still pass — the new field is unused.

- [ ] **Step 5: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): add ExpandedFiles state slot (not yet rendered)"
```

---

## Task 5: Worker `Request::ListFiles` / `Response::ListFiles`

**Files:**
- Modify: `src/data/worker.rs`

- [ ] **Step 1: Write the failing happy-path worker test**

Add at the end of the `#[cfg(test)] mod tests` block in `src/data/worker.rs`:

```rust
    #[test]
    fn list_files_emits_filemeta_for_resolvable_refs() {
        use crate::data::pr::FileMeta;
        let mut gh = FakeGh::new();
        // gh not needed for ListFiles; leave defaults.
        let _ = &gh;
        let mut git = FakeGit::new("/tmp/repo");
        git.refs.insert("refs/prpr/pr-7".into(), "headoid".into());
        git.refs.insert("origin/main".into(), "baseoid".into());
        git.numstats.insert(
            ("baseoid".into(), "headoid".into()),
            vec![FileMeta { path: "a.rs".into(), additions: 1, deletions: 2 }],
        );
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::ListFiles { number: 7, base_ref: "main".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::ListFiles { number: 7, result: Ok(files) }) => {
                    assert_eq!(files.len(), 1);
                    assert_eq!(files[0].path, "a.rs");
                    return;
                }
                Ok(Response::ListFiles { result: Err(e), .. }) => panic!("unexpected err: {e}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received ListFiles ok");
    }

    #[test]
    fn list_files_emits_error_when_refs_missing() {
        let gh = FakeGh::new();
        let git = FakeGit::new("/tmp/repo");  // empty refs
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::ListFiles { number: 7, base_ref: "main".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::ListFiles { number: 7, result: Err(_) }) => return,
                Ok(Response::ListFiles { result: Ok(_), .. }) => panic!("expected err"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received ListFiles err");
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib list_files_emits 2>&1 | tail -10`
Expected: compile error — `Request::ListFiles` doesn't exist.

- [ ] **Step 3: Add the request/response variants**

In `src/data/worker.rs`:

Add to `pub enum Request` (after `Merge`):

```rust
    ListFiles { number: u32, base_ref: String },
```

Add to `pub enum Response` (after `MergeDone`):

```rust
    /// Inline file list for a PR, emitted in response to `ListFiles`.
    /// Number is the staleness key — the UI matches it against the
    /// currently-selected PR before applying.
    ListFiles {
        number: u32,
        result: anyhow::Result<Vec<crate::data::pr::FileMeta>>,
    },
```

- [ ] **Step 4: Add the worker handler**

In `run_worker`, add a new match arm before the closing `}` of the outer `match req`:

```rust
            Request::ListFiles { number, base_ref } => {
                let head_ref = format!("refs/prpr/pr-{number}");
                let base_ref_full = format!("origin/{base_ref}");
                let result = (|| -> Result<Vec<crate::data::pr::FileMeta>> {
                    let head = git.rev_parse(&repo_root, &head_ref)?;
                    let base = git.rev_parse(&repo_root, &base_ref_full)?;
                    git.diff_numstat(&repo_root, &base, &head)
                })();
                let _ = res_tx.send(Response::ListFiles { number, result });
            }
```

- [ ] **Step 5: Run tests to verify pass**

`cargo test -p prpr --lib list_files_emits 2>&1 | tail -10`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/data/worker.rs
git commit -m "feat(worker): Request/Response::ListFiles via git diff --numstat"
```

---

## Task 6: App wiring — selection-change hook & response handling

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write the failing app-level test**

Existing tests in `src/app.rs` don't fully cover handler logic in isolation. We'll add focused tests using the in-process worker (same pattern as `worker.rs` tests). Add to the `#[cfg(test)] mod tests` block at the bottom of `src/app.rs` (or create one if none exists at the file level):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gh::fakes::FakeGh;
    use crate::data::git::fakes::FakeGit;
    use crate::data::pr::{Author, FileMeta, Pr, PrState};
    use crate::view::pr_list::ExpandedFiles;

    fn make_pr(n: u32) -> Pr {
        Pr {
            number: n,
            title: format!("pr-{n}"),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: format!("f{n}"),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    fn make_app() -> App {
        App::new(
            "/tmp/repo".into(),
            Arc::new(FakeGh::new()),
            Arc::new(FakeGit::new("/tmp/repo")),
            Config::default(),
        )
    }

    #[test]
    fn list_files_response_matching_selection_transitions_to_ready() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7), make_pr(8)];
        st.list.selected = 0;
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        let files = vec![FileMeta { path: "a.rs".into(), additions: 1, deletions: 0 }];
        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 7, result: Ok(files.clone()) },
        );
        match st.list.expanded {
            Some(ExpandedFiles::Ready { number: 7, files: f }) => assert_eq!(f, files),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn list_files_response_for_other_pr_is_dropped() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7), make_pr(8)];
        st.list.selected = 0; // selected is #7
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 8, result: Ok(vec![]) },
        );
        // Expanded must remain Loading on 7 — stale response dropped.
        assert!(matches!(
            st.list.expanded,
            Some(ExpandedFiles::Loading { number: 7 })
        ));
    }

    #[test]
    fn list_files_error_transitions_to_error_variant() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7)];
        st.list.selected = 0;
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 7, result: Err(anyhow::anyhow!("ref missing")) },
        );
        match st.list.expanded {
            Some(ExpandedFiles::Error { number: 7, ref message }) => {
                assert!(message.contains("ref missing"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib list_files_response 2>&1 | tail -15`
Expected: compile error — `Response::ListFiles` not handled in `handle_response`.

- [ ] **Step 3: Add the `Response::ListFiles` arm in `handle_response`**

In `src/app.rs`, in the big `match resp` inside `fn handle_response`, add (before the closing brace, after `Response::MergeDone { number, result: Err(e) }`):

```rust
        Response::ListFiles { number, result } => {
            // Drop if the user has navigated to a different PR (or no PR).
            let sel_number = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number);
            if sel_number != Some(number) {
                return;
            }
            let exp_number = st.list.expanded.as_ref().map(ExpandedFiles::number);
            if exp_number != Some(number) {
                return;
            }
            st.list.expanded = Some(match result {
                Ok(files) => crate::view::pr_list::ExpandedFiles::Ready { number, files },
                Err(e) => crate::view::pr_list::ExpandedFiles::Error {
                    number,
                    message: format!("{e:#}"),
                },
            });
        }
```

Add the import near the other view imports:

```rust
use crate::view::pr_list::{ExpandedFiles, PrListState};
```

(Existing line is `use crate::view::pr_list::PrListState;` — replace it.)

- [ ] **Step 4: Add the selection-change helper**

In `src/app.rs`, near the existing `send_refresh` helper, add:

```rust
/// Trigger a fresh `ListFiles` request for whatever row is currently
/// selected. Always re-issues (no cache); the staleness check on
/// `Response::ListFiles` drops responses for rows the user has left.
fn after_selection_change(app: &App, st: &mut AppState) {
    let Some(pr) = st.list.visible_prs().get(st.list.selected).map(|p| (*p).clone()) else {
        st.list.expanded = None;
        return;
    };
    st.list.expanded = Some(ExpandedFiles::Loading { number: pr.number });
    app.request(Request::ListFiles {
        number: pr.number,
        base_ref: pr.base_ref_name,
    });
}
```

- [ ] **Step 5: Wire into selection-changing actions**

In `fn handle_action`, modify the navigation arms:

```rust
        Action::ListUp => {
            if st.list.selected > 0 {
                st.list.selected -= 1;
                after_selection_change(app, st);
            }
        }
        Action::ListDown => {
            let n = st.list.visible_prs().len();
            if st.list.selected + 1 < n {
                st.list.selected += 1;
                after_selection_change(app, st);
            }
        }
        Action::ListBottom => {
            let n = st.list.visible_prs().len();
            st.list.selected = n.saturating_sub(1);
            after_selection_change(app, st);
        }
```

`Action::ListTop` sets `pending_g` (vim `gg`); the actual top-jump happens elsewhere. Find it (likely a second `g` triggers `selected = 0`). Search for `pending_g` in `src/app.rs` and add `after_selection_change(app, st);` after the line that sets `selected = 0` in that branch.

`Action::ListSearch` and `Action::ListClearFilter` mutate search but not selection directly; however, search filtering can change what's at `selected` index. Conservative fix: also call `after_selection_change` at the end of the input handler that drives search updates (anywhere `st.list.search` changes). Identify by `grep -n "st.list.search" src/app.rs` and add a call where the user accepts the search string (Enter/Esc).

Use TaskList / TaskUpdate as appropriate if the engineer wants to split this micro-work.

- [ ] **Step 6: Trigger on `ListFast` arrival**

In `handle_response`, inside the `Response::ListFast { ... Ok(prs) => { ... }` block, at the very end (after `st.list.selected = reselect_by_number(...)`), add:

```rust
                st.list.expanded = None;
                after_selection_change(app, &mut *st);
```

Note: this requires `app: &mut App` in `handle_response`, which already is the case. If borrow-checker complains because `st` is being mutated and `app.worker` is also needed, restructure: pull the selected PR's `number` and `base_ref_name` out first into locals, then `app.request(...)`. Acceptable refactor of the helper if needed:

```rust
fn after_selection_change(app: &App, st: &mut AppState) {
    let Some((number, base_ref)) = st
        .list
        .visible_prs()
        .get(st.list.selected)
        .map(|p| (p.number, p.base_ref_name.clone()))
    else {
        st.list.expanded = None;
        return;
    };
    st.list.expanded = Some(ExpandedFiles::Loading { number });
    app.request(Request::ListFiles { number, base_ref });
}
```

`app.request` takes `&self` (it's already non-mut), so this should compile cleanly.

- [ ] **Step 7: Refresh path clears expanded**

In `fn send_refresh`, add at the top (after `st.last_refresh_at = Some(...);`):

```rust
    st.list.expanded = None;
```

- [ ] **Step 8: Run all tests**

`cargo test -p prpr 2>&1 | tail -15`
Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): dispatch ListFiles on selection change, handle response"
```

---

## Task 7: Renderer — inline file rows + loading/error states

**Files:**
- Modify: `src/view/pr_list.rs`

- [ ] **Step 1: Write the failing renderer tests**

Add to `src/view/pr_list.rs` tests:

```rust
    #[test]
    fn expanded_ready_renders_file_paths_under_selected_row() {
        use crate::data::pr::FileMeta;
        let mut st = fixture_state();
        st.selected = 0;
        let sel_number = st.visible_prs()[0].number;
        st.expanded = Some(ExpandedFiles::Ready {
            number: sel_number,
            files: vec![
                FileMeta { path: "src/foo.rs".into(), additions: 12, deletions: 3 },
                FileMeta { path: "tests/bar.rs".into(), additions: 4, deletions: 0 },
            ],
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("src/foo.rs"), "missing src/foo.rs in:\n{all}");
        assert!(all.contains("tests/bar.rs"), "missing tests/bar.rs in:\n{all}");
        assert!(all.contains("+12"), "missing +12 in:\n{all}");
        assert!(all.contains("-3"),  "missing -3 in:\n{all}");
        assert!(all.contains("+4"),  "missing +4 in:\n{all}");
    }

    #[test]
    fn expanded_loading_renders_loading_files_text() {
        let mut st = fixture_state();
        st.selected = 0;
        let sel_number = st.visible_prs()[0].number;
        st.expanded = Some(ExpandedFiles::Loading { number: sel_number });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("loading files"), "missing loading text in:\n{all}");
    }

    #[test]
    fn expanded_mismatched_number_does_not_render_files() {
        use crate::data::pr::FileMeta;
        let mut st = fixture_state();
        st.selected = 0;
        // Tag expanded with a number that DOESN'T match the selected PR.
        st.expanded = Some(ExpandedFiles::Ready {
            number: 999_999,
            files: vec![FileMeta { path: "stale.rs".into(), additions: 1, deletions: 0 }],
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("stale.rs"), "stale row leaked into:\n{all}");
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib expanded_ready_renders 2>&1 | tail -10`
Expected: tests fail — renderer doesn't emit file lines yet.

- [ ] **Step 3: Implement the expanded block in `render_rows`**

In `src/view/pr_list.rs`, replace the row-emit loop in `render_rows`:

```rust
    let visible = st.visible_prs();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible.len() + 1);
    lines.push(divider(area.width as usize));
    for (i, pr) in visible.iter().enumerate() {
        lines.push(row_for(pr, i == st.selected, now, area.width));
        if i == st.selected {
            match &st.expanded {
                Some(ExpandedFiles::Loading { number }) if *number == pr.number => {
                    lines.push(loading_line(area.width));
                }
                Some(ExpandedFiles::Ready { number, files }) if *number == pr.number => {
                    let total = files.len();
                    for (fi, f) in files.iter().enumerate() {
                        let last = fi + 1 == total;
                        lines.push(file_line(f, last, area.width));
                    }
                }
                Some(ExpandedFiles::Error { number, message }) if *number == pr.number => {
                    lines.push(error_line(message, area.width));
                }
                _ => {}
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
```

Add the helper functions at the end of `src/view/pr_list.rs` (after `humanize_age`):

```rust
fn loading_line(width: u16) -> Line<'static> {
    let body = format!("  {} loading files…", crate::render::spinner::glyph());
    Line::from(Span::styled(
        format!("{:<width$}", body, width = width as usize),
        Style::default().fg(OVERLAY1),
    ))
}

fn error_line(message: &str, width: u16) -> Line<'static> {
    let max = (width as usize).saturating_sub(10).max(8);
    let trimmed = truncate(message, max);
    Line::from(Span::styled(
        format!("  error: {trimmed}"),
        Style::default().fg(DIFF_DEL_FG),
    ))
}

fn file_line(f: &crate::data::pr::FileMeta, last: bool, width: u16) -> Line<'static> {
    let glyph = if last { "└" } else { "├" };
    let mut stats = String::new();
    if f.additions > 0 {
        stats.push_str(&format!("+{}", f.additions));
    }
    if f.deletions > 0 {
        if !stats.is_empty() { stats.push(' '); }
        stats.push_str(&format!("-{}", f.deletions));
    }
    // Layout: "  ├ <path>" left-aligned; "<stats>" right-aligned.
    let left_cols = 4; // "  ├ " or "  └ "
    let right_cols = stats.chars().count();
    let path_budget = (width as usize)
        .saturating_sub(left_cols)
        .saturating_sub(right_cols + 2) // 2 spaces of gutter
        .max(8);
    let path = if f.path.chars().count() <= path_budget {
        f.path.clone()
    } else {
        let skip = f.path.chars().count() - (path_budget - 1);
        format!("…{}", f.path.chars().skip(skip).collect::<String>())
    };
    let pad_cols = (width as usize)
        .saturating_sub(left_cols)
        .saturating_sub(path.chars().count())
        .saturating_sub(right_cols);
    Line::from(vec![
        Span::styled(format!("  {glyph} "), Style::default().fg(SURFACE2)),
        Span::styled(path, Style::default().fg(TEXT)),
        Span::styled(" ".repeat(pad_cols), Style::default()),
        Span::styled(
            if f.additions > 0 { format!("+{}", f.additions) } else { String::new() },
            Style::default().fg(DIFF_ADD_FG),
        ),
        Span::styled(
            if f.additions > 0 && f.deletions > 0 { " ".to_string() } else { String::new() },
            Style::default(),
        ),
        Span::styled(
            if f.deletions > 0 { format!("-{}", f.deletions) } else { String::new() },
            Style::default().fg(DIFF_DEL_FG),
        ),
    ])
}
```

(Note: the helper splits stats rendering by color, so the `stats` string from the budget calc is just for spacing — it isn't actually used to render. The colored Spans below reproduce the same visible content.)

- [ ] **Step 4: Run tests to verify pass**

`cargo test -p prpr --lib expanded_ 2>&1 | tail -10`
Expected: 3 tests pass.

- [ ] **Step 5: Run whole suite**

`cargo test -p prpr 2>&1 | tail -10`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): render expanded file block under selected row"
```

---

## Task 8: Viewport scrolling so the selected row stays visible

This is the only part of the spec that's a quality concern rather than core functionality. Keep it small; the helper does the math, the renderer slices the Vec of Lines.

**Files:**
- Modify: `src/view/pr_list.rs`

- [ ] **Step 1: Write the failing test**

Add to tests:

```rust
    #[test]
    fn selected_row_stays_visible_when_expanded_block_is_tall() {
        // 5 PRs; selected = 4 (last); expanded with 20 files; area height
        // is only 18. Without scrolling, the selected row would render
        // first (with files below) and still be visible — but on a tall
        // *previous* selection, the renderer's first line wouldn't be the
        // selected PR. Force the case: scroll offset must shift so the
        // selected PR's row appears in the visible area.
        use crate::data::pr::{Author, FileMeta, Pr, PrState};
        let mut st = PrListState::default();
        st.repo_name = "prpr".into();
        st.branch = "main".into();
        st.prs = (0..5).map(|i| Pr {
            number: 100 + i, title: format!("p{i}"), is_draft: false, state: PrState::Open,
            author: Author { login: "a".into() }, created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(), head_ref_name: "f".into(),
            labels: vec![], status_check_rollup: vec![],
            review_decision: None, mergeable: None,
        }).collect();
        st.selected = 4;
        st.expanded = Some(ExpandedFiles::Ready {
            number: 104,
            files: (0..20).map(|i| FileMeta {
                path: format!("file{i}.rs"), additions: 1, deletions: 0,
            }).collect(),
        });
        let mut term = Terminal::new(TestBackend::new(80, 18)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("#104"), "selected PR #104 must be visible in:\n{all}");
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib selected_row_stays_visible 2>&1 | tail -10`
Expected: test FAILS (assertion message will show that `#104` is off-screen because earlier rows pushed it down).

If by accident it passes (e.g., 5 rows fit in 18 lines so selection is at top), increase the PR count to 10 to force the case.

- [ ] **Step 3: Implement scroll offset**

In `render_rows`, between building the `lines` Vec and the final `f.render_widget(...)`, compute the offset:

```rust
    // Find the absolute line index of the selected PR's row, so we can
    // scroll it into view when the expanded block pushes content past
    // the viewport. The selected row's index is:
    //   1 (divider) + sum of (1 + expanded_rows_if_selected) for i < selected
    // = 1 + selected  (since only the selected row has expanded rows)
    let selected_row_idx = 1 + st.selected;
    let h = area.height as usize;
    let total = lines.len();
    let offset = if total <= h {
        0
    } else if selected_row_idx + 2 < h {
        // Selected row is already in the upper portion — no scroll needed.
        0
    } else {
        // Keep the selected row at ~2 lines from the top of the viewport.
        let target_top = selected_row_idx.saturating_sub(2);
        target_top.min(total.saturating_sub(h))
    };
    let view: Vec<Line<'static>> =
        lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(view), area);
```

(Replace the existing `f.render_widget(Paragraph::new(lines), area);` line.)

- [ ] **Step 4: Run tests to verify pass**

`cargo test -p prpr --lib selected_row_stays_visible 2>&1 | tail -5`
Expected: pass.

- [ ] **Step 5: Run whole suite**

`cargo test -p prpr 2>&1 | tail -10`
Expected: all pass. If `manual_refresh_hides_existing_rows` or other tests broke because they expected a non-scrolled buffer, inspect: the scroll offset is `0` when content fits in viewport, so existing tests shouldn't regress.

- [ ] **Step 6: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): scroll to keep selected row visible when files overflow"
```

---

## Task 9: Final integration smoke + lint

**Files:** none (validation only)

- [ ] **Step 1: Run the full test suite**

`cargo test -p prpr 2>&1 | tail -15`
Expected: all green.

- [ ] **Step 2: Lint**

`cargo clippy -p prpr --all-targets -- -D warnings 2>&1 | tail -30`
Expected: no warnings. Fix any that surface.

- [ ] **Step 3: Build release to catch any non-test compile issues**

`cargo build --release 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 4: Manual smoke (best-effort)**

Run `cargo run --release` inside the worktree against a real repo (the prpr repo itself works). Verify:
- The list renders with only open PRs (no `filter:` segment in the header).
- Pressing `j` / `k` expands files under the new selection within ~100ms.
- Pressing `f` does nothing (no panic, no error).
- Pressing `r` clears the expanded block, shows loading, then re-renders it for the current selection.

Document any deviations and fix.

- [ ] **Step 5: Commit if any fixes were needed**

```bash
git add -A
git commit -m "chore: post-integration lint/cleanup"
```

(Skip if no changes.)

---

## Self-review checklist (post-write)

- ✅ Spec coverage:
  - Inline expansion on selection → Tasks 4 + 6
  - File row format with right-aligned +/- → Task 6 (`file_line`)
  - All files shown, no cap → Task 6 (the loop has no upper bound)
  - Loading + Error states → Task 6 + 4
  - Viewport scroll keeps selected row visible → Task 8
  - No cache, re-fetch on every selection change → Task 6 (always re-issues)
  - `git diff --numstat` on local refs → Task 2
  - `Request::ListFiles`/`Response::ListFiles` → Task 5
  - Staleness key on number → Task 4 (enum) + Task 6 app handler
  - Drop closed/merged support → Task 3
- ✅ No placeholders (TBD/TODO/FIXME): none.
- ✅ Type consistency: `FileMeta`, `ExpandedFiles`, `Request::ListFiles { number, base_ref }`, `Response::ListFiles { number, result }` are referenced identically across tasks.
