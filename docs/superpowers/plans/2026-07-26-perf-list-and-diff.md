# Perf: fast list first-draw + fast diff view — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the PR list appear as soon as `gh pr list` returns (network fetch continues in background), and make the diff view paint/scroll at frame cost proportional to screen height instead of file size.

**Architecture:** The single FIFO worker keeps handling interactive requests, but `RefreshList` moves onto a detached thread (like enrichment already does) and emits rows early via `ListFast`, followed by a new `ListRefsReady` event once refs are fetched and conflict-stamped. `OpenPr` gains an on-demand single-PR ref fetch so Enter works even before the bulk fetch lands. On the UI side: `ListFiles` results are cached per PR number and requests are debounced; the diff body renders only the visible slice with per-line memoized syntax spans; the event loop draws only when state changed or a spinner is animating.

**Tech Stack:** Rust, ratatui 0.30, crossterm, syntect 5 (fancy), std mpsc + threads. No new dependencies.

## Global Constraints

- NEVER run `cargo fmt` (repo is not rustfmt-clean). Match surrounding style by hand.
- Gate every task on `cargo test` and `cargo clippy --all-targets` (warnings in changed code are failures).
- NO MOCKS — use the existing in-repo fakes (`FakeGh`, `FakeGit`) and real `Worker` threads/channels, as existing tests do.
- Comments: max 1 line / 80 chars, explain why/what, never reference tickets/branches or removed code.
- Tests must assert observable outcomes (responses on the worker channel, rendered buffers, state transitions), not setter bookkeeping. Poll-until-deadline for async assertions (existing tests show the pattern).
- All work happens in the worktree at `.claude/worktrees/perf-list-and-diff` (branch `worktree-perf-list-and-diff`). Never commit to `main`.
- Baseline: 212 tests passing before any change.

---

### Task 1: Pre-warm syntect off the first-paint path

**Files:**
- Modify: `src/render/syntax.rs` (add `warm()`)
- Modify: `src/main.rs` (spawn warm thread)

**Interfaces:**
- Produces: `pub fn warm()` in `prpr::render::syntax` — forces the lazy `SyntaxSet`/`Theme` loads (~160ms measured) on a background thread at startup instead of on the first diff frame.

No test: `warm()` is two lines of trivial glue over already-tested lazies; a test would only assert initialization (excluded by the testing principles).

- [ ] **Step 1: Add `warm()` to `src/render/syntax.rs`**

After the `theme()` function:

```rust
/// Force the lazy syntax/theme loads now, off the first diff paint.
pub fn warm() {
    let _ = syntax_set();
    let _ = theme();
}
```

- [ ] **Step 2: Spawn it in `src/main.rs`**

In `real_main()`, right after `let mut app = App::new(...)`:

```rust
    // Syntax definitions take ~150ms to load; warm them while the list loads.
    thread::spawn(prpr::render::syntax::warm);
```

(`use std::thread;` is already imported in main.rs.)

- [ ] **Step 3: Verify**

Run: `cargo test && cargo clippy --all-targets`
Expected: 212 passing, no new warnings.

- [ ] **Step 4: Commit**

```bash
git add src/render/syntax.rs src/main.rs
git commit -m "perf(syntax): pre-warm syntect on a background thread at startup"
```

---

### Task 2: Narrow the bulk fetch refspec to open-PR bases

**Files:**
- Modify: `src/data/git.rs` (trait `fetch_pr_refs` signature, `GitCli` impl, new pure fn `fetch_refspecs`, `FakeGit`)
- Modify: `src/data/worker.rs` (call site collects base branches)
- Test: `src/data/git.rs` tests module

**Interfaces:**
- Consumes: nothing new.
- Produces: `fn fetch_pr_refs(&self, repo_root: &Path, numbers: &[u32], bases: &[String]) -> Result<()>` (trait change) and `pub(crate) fn fetch_refspecs(numbers: &[u32], bases: &[String]) -> Vec<String>`.

Rationale: the current refspec `+refs/heads/*:refs/remotes/origin/*` re-fetches every remote branch every 30s; only the base branches of open PRs are ever read (`origin/<base>` in `run_open_pr`, `run_worker::ListFiles`, `apply_local_merge_states`).

- [ ] **Step 1: Write the failing test in `src/data/git.rs` tests module**

```rust
    #[test]
    fn fetch_refspecs_covers_pr_heads_and_deduped_bases() {
        let specs = super::fetch_refspecs(&[7, 9], &["main".into(), "main".into(), "dev".into()]);
        assert_eq!(
            specs,
            vec![
                "+refs/pull/7/head:refs/prpr/pr-7".to_string(),
                "+refs/pull/9/head:refs/prpr/pr-9".to_string(),
                "+refs/heads/main:refs/remotes/origin/main".to_string(),
                "+refs/heads/dev:refs/remotes/origin/dev".to_string(),
            ],
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test fetch_refspecs_covers -- --nocapture`
Expected: FAIL — `fetch_refspecs` not defined.

- [ ] **Step 3: Implement**

In `src/data/git.rs`, above `pub struct GitCli`:

```rust
/// Refspecs for one bulk fetch: every open PR head plus each distinct base
/// branch. Fetching only referenced bases keeps refresh cheap on remotes
/// with many branches.
pub(crate) fn fetch_refspecs(numbers: &[u32], bases: &[String]) -> Vec<String> {
    let mut out: Vec<String> = numbers
        .iter()
        .map(|n| format!("+refs/pull/{n}/head:refs/prpr/pr-{n}"))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for b in bases {
        if seen.insert(b.as_str()) {
            out.push(format!("+refs/heads/{b}:refs/remotes/origin/{b}"));
        }
    }
    out
}
```

Change the trait method (update its doc comment too — it no longer refreshes all of `origin/*`, and `ListFast` no longer waits for it after Task 4):

```rust
    /// Fetch the given PR numbers' head refs (into `refs/prpr/pr-<n>`)
    /// plus each listed base branch — all in one git invocation.
    fn fetch_pr_refs(&self, repo_root: &Path, numbers: &[u32], bases: &[String]) -> Result<()>;
```

`GitCli` impl becomes:

```rust
    fn fetch_pr_refs(&self, repo_root: &Path, numbers: &[u32], bases: &[String]) -> Result<()> {
        let mut args: Vec<String> =
            vec!["fetch".into(), "--quiet".into(), "origin".into()];
        args.extend(fetch_refspecs(numbers, bases));
        run(Command::new("git").current_dir(repo_root).args(&args))?;
        Ok(())
    }
```

`FakeGit` impl: `fn fetch_pr_refs(&self, _root: &Path, _numbers: &[u32], _bases: &[String]) -> Result<()> { Ok(()) }`

In `src/data/worker.rs` `Request::RefreshList` arm, collect bases next to `open` and pass them:

```rust
                        let bases: Vec<String> = prs
                            .iter()
                            .filter(|p| p.state == crate::data::pr::PrState::Open)
                            .map(|p| p.base_ref_name.clone())
                            .collect();
```
and `git.fetch_pr_refs(&repo_root, &open, &bases)`.

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass (213).

- [ ] **Step 5: Commit**

```bash
git add src/data/git.rs src/data/worker.rs
git commit -m "perf(fetch): fetch only open-PR heads and their base branches"
```

---

### Task 3: On-demand single-PR ref fetch when Enter beats the bulk fetch

**Files:**
- Modify: `src/data/git.rs` (trait + `GitCli` + `FakeGit`: new `fetch_pr_ref`)
- Modify: `src/data/worker.rs` (`run_open_pr` fallback, `fetch_lock` mutex)
- Test: `src/data/worker.rs` tests module

**Interfaces:**
- Consumes: `fetch_refspecs` style refspec strings (inline here, not reused).
- Produces: `fn fetch_pr_ref(&self, repo_root: &Path, number: u32, base: &str) -> Result<()>` on `GitClient`; `run_open_pr(git, repo_root, fetch_lock, res_tx, pr)` gains a `fetch_lock: &std::sync::Mutex<()>` parameter. Task 4 reuses `fetch_lock`.
- `FakeGit` gains: `pub fetched_prs: std::sync::Mutex<Vec<u32>>` (records calls) and `pub refs_on_fetch: HashMap<String, String>` (refs that appear only after a fetch). `rev_parse` consults `refs`, then `refs_on_fetch` if a fetch happened.

- [ ] **Step 1: Write the failing tests in `src/data/worker.rs` tests**

```rust
    /// Enter can beat the bulk fetch. A missing ref must trigger a targeted
    /// single-PR fetch and then load normally — not error out.
    #[test]
    fn open_pr_fetches_missing_ref_on_demand_then_loads() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;
        let pr = pr_from_fixture(&detail);

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        // Refs exist only after a fetch — simulates a cold start.
        git.refs_on_fetch.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs_on_fetch.insert(format!("origin/{}", pr.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );
        let git = Arc::new(git);
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), git.clone(), 7);
        worker.send(Request::OpenPr(pr));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got_detail = false;
        while std::time::Instant::now() < deadline && !got_detail {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(got_detail, "cold-start OpenPr never produced PrDetail");
        assert_eq!(git.fetched_prs.lock().unwrap().clone(), vec![number]);
    }
```

Also extend the existing `open_pr_emits_load_error_when_refs_missing` test: after `assert!(saw_error, ...)`, add (requires keeping a `git` Arc handle as above):

```rust
        assert_eq!(
            git.fetched_prs.lock().unwrap().clone(),
            vec![1],
            "missing refs must attempt one on-demand fetch before erroring"
        );
```

(Adapt that test's setup to `let git = Arc::new(FakeGit::new("/tmp/repo"));` and pass `git.clone()` to `Worker::spawn`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test open_pr_fetches_missing_ref -- --nocapture`
Expected: FAIL — no `refs_on_fetch` / `fetched_prs` fields, no `fetch_pr_ref`.

- [ ] **Step 3: Implement**

`src/data/git.rs` trait, after `fetch_pr_refs`:

```rust
    /// Targeted fetch of one PR's head ref plus its base branch. Used when
    /// a PR is opened before the bulk fetch has primed its ref.
    fn fetch_pr_ref(&self, repo_root: &Path, number: u32, base: &str) -> Result<()>;
```

`GitCli`:

```rust
    fn fetch_pr_ref(&self, repo_root: &Path, number: u32, base: &str) -> Result<()> {
        let head = format!("+refs/pull/{number}/head:refs/prpr/pr-{number}");
        let base = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
        run(Command::new("git")
            .current_dir(repo_root)
            .args(["fetch", "--quiet", "origin", &head, &base]))?;
        Ok(())
    }
```

`FakeGit` — add fields (init in `new()`: `fetched_prs: Mutex::new(vec![])`, `refs_on_fetch: HashMap::new()`), add `use std::sync::Mutex;` in the fakes module, and:

```rust
        fn fetch_pr_ref(&self, _root: &Path, number: u32, _base: &str) -> Result<()> {
            self.fetched_prs.lock().unwrap().push(number);
            Ok(())
        }
```

`FakeGit::rev_parse` becomes:

```rust
        fn rev_parse(&self, _root: &Path, refname: &str) -> Result<String> {
            if let Some(oid) = self.refs.get(refname) {
                return Ok(oid.clone());
            }
            if !self.fetched_prs.lock().unwrap().is_empty()
                && let Some(oid) = self.refs_on_fetch.get(refname)
            {
                return Ok(oid.clone());
            }
            Err(anyhow!("no fake ref for {refname}"))
        }
```

`src/data/worker.rs` — create the shared fetch lock in `run_worker` before the loop:

```rust
    // Serializes all `git fetch` invocations: concurrent fetches of the same
    // ref would race on git's per-ref lock files.
    let fetch_lock = Arc::new(std::sync::Mutex::new(()));
```

Pass `&fetch_lock` to `run_open_pr` (`Request::OpenPr(pr) => run_open_pr(&*git, &repo_root, &fetch_lock, &res_tx, pr)`).

In `run_open_pr`, replace the two leading `rev_parse` blocks with a resolve-with-fallback:

```rust
    let number = pr.number;
    let head_ref = format!("refs/prpr/pr-{number}");
    let base_ref = format!("origin/{}", pr.base_ref_name);
    let resolve = |g: &dyn GitClient| -> Result<(String, String)> {
        Ok((g.rev_parse(repo_root, &head_ref)?, g.rev_parse(repo_root, &base_ref)?))
    };
    let (head_oid, base_oid) = match resolve(git) {
        Ok(oids) => oids,
        // Cold start: refs not primed yet. Take the fetch lock (waits out any
        // in-flight bulk fetch), re-check, and fetch just this PR if needed.
        Err(_) => {
            let fetched = {
                let _g = fetch_lock.lock().unwrap();
                resolve(git).or_else(|_| {
                    git.fetch_pr_ref(repo_root, number, &pr.base_ref_name)
                        .and_then(|()| resolve(git))
                })
            };
            match fetched {
                Ok(oids) => oids,
                Err(e) => {
                    let _ = res_tx.send(Response::PrLoadError {
                        number,
                        error: format!("resolving {head_ref} (try `r` to refresh): {e:#}"),
                    });
                    return;
                }
            }
        }
    };
```

Update `run_open_pr`'s signature: `fetch_lock: &std::sync::Mutex<()>` as the third parameter.

Note: the module doc comment at the top of worker.rs still says "exactly one worker … FIFO"; Task 4 rewrites it — leave it for now.

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/data/git.rs src/data/worker.rs
git commit -m "feat(worker): fetch a PR's ref on demand when opened before the bulk fetch"
```

---

### Task 4: Detach RefreshList; emit rows early; add ListRefsReady

**Files:**
- Modify: `src/data/worker.rs` (RefreshList arm → detached thread; `apply_local_merge_states` → `local_merge_states`; new `Response::ListRefsReady`; module doc comment)
- Test: `src/data/worker.rs` tests module

**Interfaces:**
- Consumes: `fetch_pr_refs(root, numbers, bases)` (Task 2), `fetch_lock` (Task 3).
- Produces: new response variant consumed by Task 5:

```rust
    /// Refs for the current generation are fetched and conflict-checked.
    /// Carries `(number, "MERGEABLE" | "CONFLICTING")` per open PR git could
    /// merge; refs git can't merge are absent (enrichment fills them).
    ListRefsReady {
        generation: u32,
        result: anyhow::Result<Vec<(u32, String)>>,
    },
```

New event order per refresh generation: `ListProgress(FetchingList)` → `ListFast(rows, no local merge state)` → `ListProgress(FetchingRefs)` → `ListRefsReady`. `ListEnriched` interleaves anywhere (unchanged). `ListFast` rows now have `mergeable: None` until either `ListRefsReady` or `ListEnriched` fills it.

- [ ] **Step 1: Rewrite/add worker tests**

Replace `refresh_emits_progress_stages_before_list_fast` with an ordering test over the full pipeline (same FakeGh setup):

```rust
    /// Rows must not wait for the ref fetch: ListFast comes right after the
    /// FetchingList stage, then FetchingRefs and the terminal ListRefsReady.
    #[test]
    fn refresh_emits_rows_before_ref_fetch_stage() {
        // ... same FakeGh/FakeGit setup as before ...
        worker.send(Request::RefreshList { generation: 9 });

        let mut events: Vec<String> = vec![];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "ListRefsReady never arrived");
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::ListProgress { generation: 9, stage }) => {
                    events.push(format!("stage:{stage:?}"));
                }
                Ok(Response::ListFast { generation: 9, result: Ok(_) }) => {
                    events.push("fast".into());
                }
                Ok(Response::ListRefsReady { generation: 9, result: Ok(_) }) => {
                    events.push("refs".into());
                    break;
                }
                Ok(Response::ListEnriched { .. }) => {}
                Ok(other) => panic!("unexpected response: {other:?}"),
                Err(_) => {}
            }
        }
        assert_eq!(
            events,
            vec!["stage:FetchingList", "fast", "stage:FetchingRefs", "refs"],
        );
    }
```

Replace `list_fast_rows_carry_locally_computed_conflict_state` with:

```rust
    /// GitHub answers UNKNOWN to a cold mergeable query, so the conflict
    /// verdicts must come from local git via ListRefsReady.
    #[test]
    fn refs_ready_carries_locally_computed_conflict_state() {
        // ... same mk()/FakeGh/FakeGit conflict setup as the old test ...
        worker.send(Request::RefreshList { generation: 1 });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for ListRefsReady");
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::ListRefsReady { generation: 1, result: Ok(states) }) => {
                    let by: std::collections::HashMap<u32, &str> =
                        states.iter().map(|(n, s)| (*n, s.as_str())).collect();
                    assert_eq!(by.get(&7), Some(&"CONFLICTING"));
                    assert_eq!(by.get(&8), Some(&"MERGEABLE"));
                    return;
                }
                Ok(Response::ListRefsReady { result: Err(e), .. }) => panic!("refs failed: {e}"),
                Ok(_) => {}
                Err(_) => {}
            }
        }
    }
```

In `worker_emits_list_fast_then_enriched_with_matching_gen`, add ignore arms for the new events:

```rust
                Response::ListRefsReady { generation: 42, .. } => {}
```

- [ ] **Step 2: Run to verify failures**

Run: `cargo test -- worker`
Expected: FAIL — `ListRefsReady` not defined.

- [ ] **Step 3: Implement**

Add the `ListRefsReady` variant to `Response` (doc comment from Interfaces above).

Refactor `apply_local_merge_states` → `local_merge_states` returning verdicts instead of mutating (same threading internals, ~12ms per PR note stays):

```rust
fn local_merge_states(git: &dyn GitClient, repo_root: &Path, prs: &[Pr]) -> Vec<(u32, String)> {
    let targets: Vec<(u32, String, String)> = prs
        .iter()
        .filter(|p| p.state == crate::data::pr::PrState::Open)
        .map(|p| {
            (p.number, format!("origin/{}", p.base_ref_name), format!("refs/prpr/pr-{}", p.number))
        })
        .collect();
    if targets.is_empty() {
        return vec![];
    }
    // ~12ms per PR; serial checks would stall a 200-PR repo for seconds.
    let chunk = targets.len().div_ceil(MERGE_CHECK_THREADS.min(targets.len()));
    thread::scope(|s| {
        let handles: Vec<_> = targets
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .filter_map(|(n, base, head)| {
                            git.merge_conflicts(repo_root, base, head).ok().map(|conflicting| {
                                let v = if conflicting { "CONFLICTING" } else { "MERGEABLE" };
                                (*n, v.to_string())
                            })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}
```

Rewrite the `Request::RefreshList` arm: keep the existing enrichment thread spawn verbatim, then replace the inline fast-list/fetch block with a second detached thread:

```rust
                // The fast pipeline also runs detached so interactive
                // requests (OpenPr, ListFiles) never queue behind network.
                let gh_fast = Arc::clone(&gh);
                let git_fast = Arc::clone(&git);
                let repo_fast = repo_root.clone();
                let tx_fast = res_tx.clone();
                let lock = Arc::clone(&fetch_lock);
                thread::spawn(move || {
                    let _ = tx_fast.send(Response::ListProgress {
                        generation,
                        stage: ListStage::FetchingList,
                    });
                    let prs = match gh_fast.list_prs_fast(&repo_fast) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = tx_fast.send(Response::ListFast { generation, result: Err(e) });
                            return;
                        }
                    };
                    let open: Vec<u32> = prs
                        .iter()
                        .filter(|p| p.state == crate::data::pr::PrState::Open)
                        .map(|p| p.number)
                        .collect();
                    let bases: Vec<String> = prs
                        .iter()
                        .filter(|p| p.state == crate::data::pr::PrState::Open)
                        .map(|p| p.base_ref_name.clone())
                        .collect();
                    // Rows first: the list draws now; refs and conflict
                    // verdicts stream in behind it.
                    let _ = tx_fast.send(Response::ListFast { generation, result: Ok(prs.clone()) });
                    let _ = tx_fast.send(Response::ListProgress {
                        generation,
                        stage: ListStage::FetchingRefs,
                    });
                    let fetched = {
                        let _g = lock.lock().unwrap();
                        git_fast.fetch_pr_refs(&repo_fast, &open, &bases)
                    };
                    let result = match fetched {
                        Ok(()) => Ok(local_merge_states(&*git_fast, &repo_fast, &prs)),
                        Err(e) => Err(anyhow::anyhow!("fetching open PR refs: {e:#}")),
                    };
                    let _ = tx_fast.send(Response::ListRefsReady { generation, result });
                });
```

Update the module doc comment (lines 1–10): it currently promises "exactly one worker … FIFO". New text:

```rust
//! Worker thread + request/response channels.
//!
//! Interactive subprocess work (`git diff`, `git blame`, `gh pr merge`)
//! runs on a single worker thread, FIFO. `RefreshList` is the exception:
//! its gh/network/fetch pipeline runs on detached threads (one for the
//! fast list + ref fetch, one for enrichment) so a slow refresh never
//! delays opening a PR. All `git fetch` calls share one lock.
//!
//! The UI thread sends `Request`s and drains `Response`s every iteration
//! of its event loop. The worker exits cleanly when `Worker` is dropped.
```

Also update the stale comment above the `RefreshList` arm (the "renders only after every OPEN PR's head ref is locally fetched" block) to describe the new order.

Note: `handle_response` in app.rs doesn't compile without a match arm — add a placeholder arm now, replaced properly in Task 5:

```rust
        Response::ListRefsReady { .. } => { /* handled in the UI task */ }
```

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass. The app still shows conflict markers (via enrichment) but not yet from local refs — Task 5 wires that.

- [ ] **Step 5: Commit**

```bash
git add src/data/worker.rs src/app.rs
git commit -m "perf(worker): detach refresh pipeline and emit list rows before the ref fetch"
```

---

### Task 5: UI — early rows unblock input; apply ListRefsReady; defer file lists until refs exist

**Files:**
- Modify: `src/app.rs` (`AppState` new field `refs_ready`, `handle_response` ListFast/ListRefsReady arms, `after_selection_change`)
- Modify: `src/view/pr_list.rs` (footer shows stage while rows visible; stale comment in `render_rows`)
- Test: `src/app.rs` tests module

**Interfaces:**
- Consumes: `Response::ListRefsReady { generation, result: Result<Vec<(u32, String)>> }` (Task 4), `Pr::merge_state()` / `MergeState` from `src/data/pr.rs`.
- Produces: `AppState.refs_ready: bool` — false from construction until the first `ListRefsReady`; `after_selection_change` defers its request while false. Task 6 layers caching/debounce onto the same function.

Behavior changes:
1. `ListFast(Ok)` now also clears `manual_refresh_in_flight` — rows appear and input unblocks as soon as `gh pr list` returns (previously blocked until enrichment).
2. New `ListRefsReady` arm: on Ok, stamp each `(number, verdict)` onto matching rows **unless** the row already has a definite verdict (mirrors `apply_enrichment`'s rule — enrichment that landed first wins); set `refs_ready = true`; clear `loading_stage`; re-run `after_selection_change`. On Err, put `fetching refs failed: …` in `st.list.status`, still set `refs_ready = true` (later requests surface real errors instead of deferring forever), and re-run `after_selection_change`.
3. `after_selection_change` while `!refs_ready`: show `ExpandedFiles::Loading` but send nothing (a `ListFiles` against unfetched refs would error).
4. Footer: while rows are visible and a stage is running (`loading_stage` set but `loading` false), show the stage spinner instead of falling through to "enriching…".

- [ ] **Step 1: Write failing tests in `src/app.rs` tests**

Locate the existing test helpers (they build an `App` with fakes and call `handle_response` directly — follow the local pattern, e.g. the tests around `MergeDone`). Add:

```rust
    #[test]
    fn list_fast_unblocks_manual_refresh_before_enrichment() {
        // setup app + state via the file's existing helper pattern
        send_refresh(&app, &mut st, false);
        assert!(st.list.manual_refresh_in_flight);
        handle_response(&mut app, &mut st, Response::ListFast {
            generation: st.list_gen,
            result: Ok(vec![pr(1)]),
        });
        assert!(!st.list.manual_refresh_in_flight, "rows arrived — input must unblock");
        assert!(st.list.enriching, "enrichment is still running");
    }

    #[test]
    fn refs_ready_stamps_verdicts_but_never_overwrites_definite_enrichment() {
        // rows present; PR 1 has no verdict, PR 2 was already enriched CONFLICTING
        st.list.prs = vec![pr(1), pr(2)];
        st.list.prs[1].mergeable = Some("CONFLICTING".into());
        handle_response(&mut app, &mut st, Response::ListRefsReady {
            generation: st.list_gen,
            result: Ok(vec![(1, "MERGEABLE".into()), (2, "MERGEABLE".into())]),
        });
        assert_eq!(st.list.prs[0].mergeable.as_deref(), Some("MERGEABLE"));
        assert_eq!(
            st.list.prs[1].mergeable.as_deref(),
            Some("CONFLICTING"),
            "a definite enrichment verdict must not be overwritten by the local check"
        );
        assert!(st.refs_ready);
    }

    #[test]
    fn selection_change_defers_files_request_until_refs_ready() {
        st.list.prs = vec![pr(1)];
        st.refs_ready = false;
        after_selection_change(&app, &mut st);
        assert!(matches!(st.list.expanded, Some(ExpandedFiles::Loading { number: 1 })));
        // ListRefsReady flips the gate and re-issues the request path.
        handle_response(&mut app, &mut st, Response::ListRefsReady {
            generation: st.list_gen,
            result: Ok(vec![]),
        });
        assert!(st.refs_ready);
    }
```

(`pr(n)` — reuse/construct the file's existing `Pr` fixture helper; if only tests in other modules have one, add a small local `fn pr(n: u32) -> Pr` mirroring `src/data/cache.rs` tests.)

- [ ] **Step 2: Run to verify failures**

Run: `cargo test -- app::tests`
Expected: FAIL — `refs_ready` field missing, ListFast doesn't clear the flag, ListRefsReady arm is a stub.

- [ ] **Step 3: Implement**

`AppState`: add `pub refs_ready: bool` (doc: `/// False until the first ListRefsReady; file lists defer on a cold start.`), init `false` in `AppState::new`.

`handle_response` `ListFast Ok` arm: add `st.list.manual_refresh_in_flight = false;` next to `st.list.loading = false;`.

Replace the Task-4 placeholder arm:

```rust
        Response::ListRefsReady { generation, result } if generation == st.list_gen => {
            match result {
                Ok(states) => {
                    for (number, verdict) in states {
                        if let Some(p) = st.list.prs.iter_mut().find(|p| p.number == number) {
                            let definite = matches!(
                                p.merge_state(),
                                Some(MergeState::Mergeable) | Some(MergeState::Conflicting)
                            );
                            if !definite {
                                p.mergeable = Some(verdict);
                            }
                        }
                    }
                }
                Err(e) => st.list.status = format!("fetching refs failed: {e:#}"),
            }
            st.list.loading_stage = None;
            st.refs_ready = true;
            after_selection_change(app, st);
        }
        Response::ListRefsReady { .. } => { /* stale; drop */ }
```

(Import `MergeState` from `crate::data::pr` — check its exact path/name in `src/data/pr.rs` first.)

`after_selection_change`: after setting `expanded = Some(Loading …)`, gate the request:

```rust
    st.list.expanded = Some(ExpandedFiles::Loading { number });
    // A ListFiles against unfetched refs can only error; wait for refs.
    if !st.refs_ready {
        return;
    }
    app.request(Request::ListFiles { number, base_ref });
```

`src/view/pr_list.rs` `render_footer`: insert between the `st.loading` and `st.enriching` branches:

```rust
    } else if let Some(stage) = st.loading_stage {
        // Rows are visible but a pipeline stage is still running.
        f.render_widget(
            Paragraph::new(format!("  {} {}…", spinner::glyph(), stage.label()))
                .style(Style::default().fg(OVERLAY1)),
            chunks[1],
        );
```

`render_rows`: update the stale comment ("until the fast list AND enrichment have both arrived" → "until the fast list arrives").

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass. Some existing app tests may assert `manual_refresh_in_flight` survives ListFast — update them to the new contract (rows unblock at ListFast).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/view/pr_list.rs
git commit -m "feat(list): draw rows on ListFast, stamp conflicts via ListRefsReady"
```

---

### Task 6: File-list cache + debounced ListFiles requests

**Files:**
- Modify: `src/app.rs` (`AppState`: `files_cache`, `pending_files`; `after_selection_change`; new `take_due_files_request`; run-loop flush; `ListFiles`/`ListRefsReady` handlers)
- Test: `src/app.rs` tests module

**Interfaces:**
- Consumes: `after_selection_change` shape from Task 5; `FileMeta` (`crate::data::pr::FileMeta`).
- Produces:

```rust
pub struct PendingFiles {
    pub number: u32,
    pub base_ref: String,
    pub at: Instant,
}
// AppState fields:
    /// File lists by PR number; valid until refs move (cleared on ListRefsReady).
    pub files_cache: HashMap<u32, Vec<crate::data::pr::FileMeta>>,
    /// Debounced ListFiles request; sent once selection rests FILES_DEBOUNCE.
    pub pending_files: Option<PendingFiles>,
// app.rs consts:
const FILES_DEBOUNCE: Duration = Duration::from_millis(100);
// pure-ish flush, called from the run loop:
fn take_due_files_request(st: &mut AppState, now: Instant) -> Option<(u32, String)>;
```

Behavior: scrolling j/k no longer fires one worker request per row. `after_selection_change` serves from `files_cache` instantly when possible; otherwise shows Loading and arms `pending_files`. The run loop flushes a pending request only once it is at least `FILES_DEBOUNCE` old. `ListFiles(Ok)` responses populate the cache (even for rows no longer selected). `ListRefsReady` clears the cache (refs moved) — add that to its handler from Task 5.

- [ ] **Step 1: Write failing tests in `src/app.rs` tests**

```rust
    #[test]
    fn selection_change_serves_files_from_cache_without_request() {
        st.refs_ready = true;
        st.list.prs = vec![pr(1)];
        st.files_cache.insert(1, vec![FileMeta { path: "a.rs".into(), additions: 1, deletions: 0 }]);
        after_selection_change(&app, &mut st);
        assert!(matches!(&st.list.expanded, Some(ExpandedFiles::Ready { number: 1, files }) if files.len() == 1));
        assert!(st.pending_files.is_none(), "cache hit must not arm a request");
    }

    #[test]
    fn uncached_selection_arms_debounce_instead_of_sending() {
        st.refs_ready = true;
        st.list.prs = vec![pr(1)];
        after_selection_change(&app, &mut st);
        assert!(matches!(st.list.expanded, Some(ExpandedFiles::Loading { number: 1 })));
        let p = st.pending_files.as_ref().expect("pending request armed");
        assert_eq!(p.number, 1);
    }

    #[test]
    fn debounce_flushes_only_after_window_elapses() {
        st.refs_ready = true;
        st.list.prs = vec![pr(1)];
        after_selection_change(&app, &mut st);
        let armed_at = st.pending_files.as_ref().unwrap().at;
        assert!(take_due_files_request(&mut st, armed_at).is_none(), "too early");
        let due = take_due_files_request(&mut st, armed_at + FILES_DEBOUNCE);
        assert_eq!(due, Some((1, "main".into())));
        assert!(st.pending_files.is_none(), "flush consumes the pending slot");
    }

    #[test]
    fn list_files_response_populates_cache() {
        st.list.prs = vec![pr(1)];
        st.list.expanded = Some(ExpandedFiles::Loading { number: 1 });
        handle_response(&mut app, &mut st, Response::ListFiles {
            number: 1,
            result: Ok(vec![FileMeta { path: "a.rs".into(), additions: 2, deletions: 1 }]),
        });
        assert_eq!(st.files_cache.get(&1).map(Vec::len), Some(1));
    }

    #[test]
    fn refs_ready_invalidates_files_cache() {
        st.files_cache.insert(1, vec![]);
        handle_response(&mut app, &mut st, Response::ListRefsReady {
            generation: st.list_gen,
            result: Ok(vec![]),
        });
        assert!(st.files_cache.is_empty(), "new refs mean cached file lists are stale");
    }
```

- [ ] **Step 2: Run to verify failures**

Run: `cargo test -- app::tests`
Expected: FAIL — fields and `take_due_files_request` missing.

- [ ] **Step 3: Implement**

Add fields/struct/const per Interfaces (init: `files_cache: HashMap::new()`, `pending_files: None`; add `use std::collections::HashMap;` to app.rs imports).

`after_selection_change` final form (builds on Task 5; note it keeps its `app` param but no longer sends directly — the parameter is now unused, so drop it and update all call sites, or keep sending on cache miss? **Drop the direct send**: signature becomes `fn after_selection_change(st: &mut AppState)`; update every call site — `handle_response` (ListFast, MergeDone, ListRefsReady), `handle_key`, `handle_action`, `handle_mouse`):

```rust
/// Refresh the expanded file list for the selected row: serve from cache,
/// or show a loading row and arm a debounced ListFiles request.
fn after_selection_change(st: &mut AppState) {
    let Some((number, base_ref)) = st
        .list
        .visible_prs()
        .get(st.list.selected)
        .map(|p| (p.number, p.base_ref_name.clone()))
    else {
        st.list.expanded = None;
        st.pending_files = None;
        return;
    };
    if let Some(files) = st.files_cache.get(&number) {
        st.list.expanded = Some(ExpandedFiles::Ready { number, files: files.clone() });
        st.pending_files = None;
        return;
    }
    st.list.expanded = Some(ExpandedFiles::Loading { number });
    // A ListFiles against unfetched refs can only error; wait for refs.
    if !st.refs_ready {
        st.pending_files = None;
        return;
    }
    st.pending_files = Some(PendingFiles { number, base_ref, at: Instant::now() });
}
```

```rust
/// Pop the pending ListFiles request once it has rested a full debounce
/// window, so holding j/k doesn't fire one subprocess per row.
fn take_due_files_request(st: &mut AppState, now: Instant) -> Option<(u32, String)> {
    let due = st
        .pending_files
        .as_ref()
        .is_some_and(|p| now.duration_since(p.at) >= FILES_DEBOUNCE);
    if !due {
        return None;
    }
    let p = st.pending_files.take().unwrap();
    Some((p.number, p.base_ref))
}
```

Run loop (in `run`, after the auto-refresh check):

```rust
        if let Some((number, base_ref)) = take_due_files_request(st, Instant::now()) {
            app.request(Request::ListFiles { number, base_ref });
        }
```

`ListFiles` handler: insert into cache first, then the existing selection/expanded guards:

```rust
        Response::ListFiles { number, result } => {
            if let Ok(files) = &result {
                st.files_cache.insert(number, files.clone());
            }
            // ... existing sel_number / exp_number guards and expanded update ...
        }
```

`ListRefsReady` handler (from Task 5): add `st.files_cache.clear();` before `after_selection_change(st);`.

Update the stale doc comment on `after_selection_change` (it said "Always re-issues (no cache)").

`take_due_files_request` test uses `armed_at + FILES_DEBOUNCE` — `Instant + Duration` works via `std::ops::Add`.

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass. Existing tests that asserted an immediate `ListFiles` request after selection change need updating to flush via `take_due_files_request(st, Instant::now() + FILES_DEBOUNCE)` first.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "perf(list): cache per-PR file lists and debounce ListFiles requests"
```

---

### Task 7: Diff body — render only the visible slice with memoized syntax spans

**Files:**
- Modify: `src/render/diff.rs` (split `render_line` → `render_line_with_spans`)
- Modify: `src/view/pr_review.rs` (`syntax_cache` field; `render`/`render_diff_body` take `&mut`; `body_lines` → `visible_body_lines`)
- Modify: `src/app.rs` (`draw` takes `&mut AppState`; clear `syntax_cache` when files are replaced)
- Test: `src/view/pr_review.rs` tests module

**Interfaces:**
- Consumes: `highlight_line(text, ext) -> Vec<Span<'static>>` (unchanged).
- Produces:

```rust
// render/diff.rs — new; render_line keeps its exact current signature and
// delegates, so its tests and any other callers are untouched:
pub fn render_line_with_spans(
    line: &DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
    highlighted: Vec<Span<'static>>,
) -> Line<'static>;
// view/pr_review.rs:
    /// Memoized syntax spans keyed by (file_index, line index). Cleared
    /// whenever `files` is replaced.
    pub syntax_cache: HashMap<(usize, usize), Vec<Span<'static>>>,
pub fn render(f: &mut Frame, area: Rect, st: &mut PrReviewState);
// app.rs:
fn draw(f: &mut ratatui::Frame, _app: &App, st: &mut AppState);
```

Why this is safe: `highlight_line` builds a fresh `HighlightLines` per call, so per-line results are position-independent — memoizing them changes nothing observable. Gutter/background colors are applied per frame on a clone of the cached spans, so blame colors arriving later never require re-highlighting. The `Paragraph::new(all_lines).scroll((n, 0))` call has no `.wrap()`, so one `Line` == one terminal row and slicing `lines[scroll..scroll+height]` is pixel-identical.

- [ ] **Step 1: Write failing tests in `src/view/pr_review.rs`**

First make a fixture with enough lines: add a helper that synthesizes a large file:

```rust
    fn big_file(n: u32) -> FileDiff {
        let lines = (1..=n)
            .map(|i| crate::data::diff::DiffLine {
                op: crate::data::diff::DiffOp::Context,
                old_lineno: Some(i),
                new_lineno: Some(i),
                text: format!("let x{i} = {i};"),
                is_hunk_header: false,
            })
            .collect();
        FileDiff { path: "src/big.rs".into(), lines, binary: false }
    }

    #[test]
    fn scrolled_body_starts_at_the_scroll_offset() {
        let mut r = fixture_review_state();
        r.files = vec![big_file(200)];
        r.scroll = 50;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        let buf = term.backend().buffer();
        // Layout rows: 0 header, 1 spacer, 2-3 file bar, 4.. body.
        let first_body = buffer_line(buf, 4);
        assert!(
            first_body.contains("let x51 = 51;"),
            "body must start at line index 50, got: {first_body:?}"
        );
    }

    #[test]
    fn render_highlights_only_the_visible_window() {
        let mut r = fixture_review_state();
        r.files = vec![big_file(500)];
        r.scroll = 0;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        // Body height is 24 - 4 (header/bar) - 3 (status) = 17 rows.
        assert!(
            !r.syntax_cache.is_empty() && r.syntax_cache.len() <= 17,
            "cache must cover exactly the visible window, got {} entries",
            r.syntax_cache.len()
        );
        assert!(r.syntax_cache.keys().all(|(_, idx)| *idx < 17));

        // Scrolling exposes new lines; already-seen ones are not redone.
        r.scroll = 100;
        let before = r.syntax_cache.len();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        assert!(r.syntax_cache.len() <= before + 17);
        assert!(r.syntax_cache.contains_key(&(0, 100)));
    }
```

- [ ] **Step 2: Run to verify failures**

Run: `cargo test -- pr_review`
Expected: FAIL — no `syntax_cache`, `render` takes `&PrReviewState`.

- [ ] **Step 3: Implement**

`src/render/diff.rs` — extract everything after the hunk-header branch into the new function; `render_line` becomes:

```rust
pub fn render_line<'a>(
    line: &'a DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
    file_ext: &str,
) -> Line<'a> {
    if line.is_hunk_header {
        return hunk_header_line(line);
    }
    render_line_with_spans(line, head_color, base_color, syntax::highlight_line(&line.text, file_ext))
}

fn hunk_header_line(line: &DiffLine) -> Line<'static> {
    Line::from(vec![Span::styled(
        line.text.clone(),
        Style::default().fg(OVERLAY1).add_modifier(Modifier::DIM),
    )])
}

pub fn render_line_with_spans(
    line: &DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
    mut highlighted: Vec<Span<'static>>,
) -> Line<'static> {
    if line.is_hunk_header {
        return hunk_header_line(line);
    }
    // ... existing body verbatim from `render_line` (lineno_str, gutter,
    // op glyph, body_bg application onto `highlighted`, span assembly) ...
}
```

`src/view/pr_review.rs`:
- Add the `syntax_cache` field to `PrReviewState` (`HashMap` is already imported).
- `render` and `render_diff_body` take `&mut PrReviewState`. Inside `render_diff_body`, split borrows before use:

```rust
fn render_diff_body(f: &mut Frame, area: Rect, st: &mut PrReviewState) {
    if st.files.is_empty() {
        // ... existing loading spinner branch unchanged ...
        return;
    }
    let file_index = st.file_index;
    let scroll = st.scroll as usize;
    let PrReviewState { files, colors, syntax_cache, .. } = st;
    let Some(file) = files.get(file_index) else {
        return;
    };
    if file.binary {
        // ... existing binary branch unchanged ...
        return;
    }
    let lines =
        visible_body_lines(file, file_index, colors, syntax_cache, scroll, area.height as usize);
    f.render_widget(Paragraph::new(lines), area);
}
```

- Replace `body_lines` with (keep the head/base lookup logic identical):

```rust
/// Rows for the visible window only — frame cost scales with screen height,
/// not file size. Syntax spans are memoized per line in `cache`.
fn visible_body_lines(
    file: &FileDiff,
    file_index: usize,
    colors: &HashMap<String, ColorState>,
    cache: &mut HashMap<(usize, usize), Vec<Span<'static>>>,
    scroll: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let lookup = colors.get(&file.path).and_then(|c| match c {
        ColorState::Ready(lc) => Some(lc),
        ColorState::Loading => None,
    });
    let ext = ext_of(&file.path);
    let start = scroll.min(file.lines.len());
    let end = (start + height).min(file.lines.len());
    file.lines[start..end]
        .iter()
        .enumerate()
        .map(|(off, l)| {
            let idx = start + off;
            let head = l.new_lineno.and_then(|n| {
                lookup
                    .and_then(|lc| lc.head.get(n.saturating_sub(1) as usize).copied())
                    .flatten()
            });
            let base = if l.op == crate::data::diff::DiffOp::Delete {
                lookup.and_then(|lc| lc.delete.get(&l.text).copied())
            } else {
                None
            };
            if l.is_hunk_header {
                return render_line(l, head, base, ext);
            }
            let spans = cache
                .entry((file_index, idx))
                .or_insert_with(|| crate::render::syntax::highlight_line(&l.text, ext))
                .clone();
            crate::render::diff::render_line_with_spans(l, head, base, spans)
        })
        .collect()
}
```

(If `render_line(l, head, base, ext)` returning `Line<'a>` clashes with the `Line<'static>` return type here, route hunk headers through `render_line_with_spans(l, head, base, vec![])` — it early-returns the owned hunk line.)

- Update imports: `render_line_with_spans` alongside `render_line`.

`src/app.rs`:
- `fn draw(f: &mut ratatui::Frame, _app: &App, st: &mut AppState)`; in the Review arm use `st.review.as_mut()`. The call site `term.draw(|f| draw(f, app, st))?` already has `&mut` access.
- `PrDiff Ok` handler: add `r.syntax_cache.clear();` next to `r.files = files;`.
- `Action::Refresh`: add `r.syntax_cache.clear();` next to `r.files.clear();`.
- `Action::Bottom` / `move_review` / `max_scroll` are untouched (scroll semantics unchanged).

Update existing pr_review tests: `render(f, area, &r)` → `let mut r = …; render(f, area, &mut r)`.

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass, including the two new window tests.

- [ ] **Step 5: Commit**

```bash
git add src/render/diff.rs src/view/pr_review.rs src/app.rs
git commit -m "perf(review): render only the visible diff window with memoized syntax spans"
```

---

### Task 8: Draw only when dirty or animating

**Files:**
- Modify: `src/app.rs` (`needs_animation` helper, run-loop dirty tracking)
- Test: `src/app.rs` tests module

**Interfaces:**
- Consumes: all `AppState` fields from prior tasks.
- Produces: `fn needs_animation(st: &AppState) -> bool` — true iff some on-screen spinner must keep ticking.

- [ ] **Step 1: Write failing tests in `src/app.rs` tests**

```rust
    #[test]
    fn idle_list_needs_no_animation() {
        // fully-loaded quiet state
        let mut st = AppState::new("repo".into(), "main".into());
        st.refs_ready = true;
        st.list.enriching = false;
        assert!(!needs_animation(&st));
    }

    #[test]
    fn spinners_require_animation() {
        let base = || {
            let mut s = AppState::new("r".into(), "m".into());
            s.list.enriching = false;
            s
        };
        let mut a = base();
        a.list.loading = true;
        assert!(needs_animation(&a), "list loading spinner");

        let mut b = base();
        b.list.expanded = Some(ExpandedFiles::Loading { number: 1 });
        assert!(needs_animation(&b), "expanded files spinner");

        let mut c = base();
        c.merging = Some(MergingState {
            pr_number: 1,
            method: MergeMethod::Merge,
            mark_ready: false,
        });
        assert!(needs_animation(&c), "merge progress spinner");

        let mut d = base();
        d.review = Some(PrReviewState { status: "loading…".into(), ..Default::default() });
        assert!(needs_animation(&d), "review loading spinner");

        let mut e = base();
        e.list.status = "merging #1…".into();
        assert!(needs_animation(&e), "in-progress status spinner");
    }
```

(`AppState::new` sets `enriching: false` already — check the actual initial state; the point of `base()` is a quiet list. Adjust constructors to match real field defaults.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -- needs_animation`
Expected: FAIL — `needs_animation` not defined.

- [ ] **Step 3: Implement**

```rust
/// True while any on-screen spinner is animating and needs redraw ticks.
fn needs_animation(st: &AppState) -> bool {
    if st.merging.is_some() {
        return true;
    }
    if st.list.loading
        || st.list.enriching
        || st.list.manual_refresh_in_flight
        || st.list.loading_stage.is_some()
    {
        return true;
    }
    if crate::render::spinner::looks_in_progress(&st.list.status) {
        return true;
    }
    if matches!(st.list.expanded, Some(ExpandedFiles::Loading { .. })) {
        return true;
    }
    if let Some(r) = &st.review {
        if r.detail.is_none() || r.files.is_empty() {
            return true;
        }
        if crate::render::spinner::looks_in_progress(&r.status) {
            return true;
        }
    }
    false
}
```

Run loop becomes:

```rust
    send_refresh(app, st, false);

    let mut dirty = true;
    while st.running {
        while let Ok(resp) = app.worker.rx.try_recv() {
            handle_response(app, st, resp);
            dirty = true;
        }

        if should_auto_refresh(/* unchanged args */) {
            send_refresh(app, st, true);
            dirty = true;
        }

        if let Some((number, base_ref)) = take_due_files_request(st, Instant::now()) {
            app.request(Request::ListFiles { number, base_ref });
            dirty = true;
        }

        // Skip identical frames; spinners still tick via needs_animation.
        if dirty || needs_animation(st) {
            term.draw(|f| draw(f, app, st))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(k) => {
                    handle_key(app, st, k);
                    dirty = true;
                }
                Event::Mouse(m) => {
                    handle_mouse(app, st, m);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test && cargo clippy --all-targets`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "perf(loop): redraw only on state change or while a spinner animates"
```

---

### Task 9: Final verification

**Files:** none (verification only).

- [ ] **Step 1: Full gate**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all tests pass, zero warnings. If `-D warnings` trips on pre-existing warnings outside the changed files, drop the flag and verify no warning mentions a changed file.

- [ ] **Step 2: Release build sanity**

Run: `cargo build --release`
Expected: clean build (this is the binary profile users actually run).

- [ ] **Step 3: Behavior audit against the spec**

Re-read the diff (`git diff main...HEAD`) and confirm each item shipped:
1. syntect pre-warm at startup ✅/❌
2. narrowed fetch refspec (PR heads + distinct bases only) ✅/❌
3. on-demand single-PR fetch under the shared fetch lock ✅/❌
4. detached refresh pipeline, `ListFast` before fetch, `ListRefsReady` after ✅/❌
5. rows + input unblock at `ListFast`; conflict stamping honors definite enrichment ✅/❌
6. `files_cache` + debounce; cache cleared on `ListRefsReady` ✅/❌
7. visible-slice diff rendering + per-line span memoization; caches cleared when files replaced ✅/❌
8. dirty/animation-gated redraw incl. Resize handling ✅/❌
9. **No disk cache for the PR list anywhere** (explicit user constraint) ✅/❌

- [ ] **Step 4: Commit any audit fixes, then hand off**

Use superpowers:finishing-a-development-branch (push branch, open draft PR — never merge to main).
