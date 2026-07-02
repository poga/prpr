# Toggle PR draft ↔ ready Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user flip a PR between draft and ready-for-review with the `d` key, from both the PR list and the review view.

**Architecture:** Follows the merge feature's path exactly: a `GhClient` trait method shells out to `gh pr ready`, a worker `Request`/`Response` pair carries the call off the UI thread, a `keys::Action` binds `d`, and `app.rs` mutates local state on success with no network refresh.

**Tech Stack:** Rust, ratatui TUI, `gh` CLI, crossterm key events.

## Global Constraints

- Comments never exceed one line (≤80 chars); keep them minimal.
- No mocks in tests — use the existing `FakeGh` / `FakeGit` and real worker.
- Post-action state updates are local only; never trigger a list refresh after a mutation.
- `gh` mapping: draft=false → `gh pr ready <n>`; draft=true → `gh pr ready <n> --undo`.

---

### Task 1: `gh.rs` — `set_pr_draft` trait method + impls

**Files:**
- Modify: `src/data/gh.rs`

**Interfaces:**
- Produces: `GhClient::set_pr_draft(&self, repo_root: &Path, number: u32, draft: bool) -> Result<()>`; `FakeGh.set_drafts: Mutex<Vec<(u32, bool)>>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/data/gh.rs`:

```rust
#[test]
fn fake_records_set_pr_draft_calls() {
    use super::GhClient;
    use super::fakes::FakeGh;
    let g = FakeGh::new();
    g.set_pr_draft(std::path::Path::new("/x"), 7, true).unwrap();
    g.set_pr_draft(std::path::Path::new("/x"), 7, false).unwrap();
    let calls = g.set_drafts.lock().unwrap().clone();
    assert_eq!(calls, vec![(7, true), (7, false)]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib fake_records_set_pr_draft_calls 2>&1 | tail -20`
Expected: FAIL — compile error, `set_pr_draft` / `set_drafts` not found.

- [ ] **Step 3: Add the trait method**

In the `pub trait GhClient` block (after `merge_pr`), add:

```rust
    /// Mark ready (draft=false) or convert to draft (draft=true).
    fn set_pr_draft(&self, repo_root: &std::path::Path, number: u32, draft: bool) -> Result<()>;
```

- [ ] **Step 4: Implement for `GhCli`**

In `impl GhClient for GhCli` (after `merge_pr`), add:

```rust
    fn set_pr_draft(&self, repo_root: &std::path::Path, number: u32, draft: bool) -> Result<()> {
        let n = number.to_string();
        let mut args = vec!["pr", "ready", n.as_str()];
        if draft {
            args.push("--undo");
        }
        run(Command::new("gh").current_dir(repo_root).args(&args))?;
        Ok(())
    }
```

- [ ] **Step 5: Add recording field + impl to `FakeGh`**

In `struct FakeGh`, add field:

```rust
        pub set_drafts: Mutex<Vec<(u32, bool)>>,
```

In `FakeGh::new()`, add to the initializer:

```rust
                set_drafts: Mutex::new(vec![]),
```

In `impl GhClient for FakeGh` (after `merge_pr`), add:

```rust
        fn set_pr_draft(&self, _root: &std::path::Path, n: u32, draft: bool) -> Result<()> {
            self.set_drafts.lock().unwrap().push((n, draft));
            Ok(())
        }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --lib fake_records_set_pr_draft_calls 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/data/gh.rs
git commit -m "feat(gh): add set_pr_draft wrapping gh pr ready"
```

---

### Task 2: worker plumbing + app applies the result

Rust's exhaustive match couples `Response::SetDraftDone` to its `handle_response` arm, so the worker plumbing and the app's result-handling ship together.

**Files:**
- Modify: `src/data/worker.rs`
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `GhClient::set_pr_draft` (Task 1).
- Produces: `Request::SetDraft { number: u32, draft: bool }`; `Response::SetDraftDone { number: u32, draft: bool, result: anyhow::Result<()> }`; `handle_response` flips `is_draft` on the matching list row and `review.detail`.

- [ ] **Step 1: Write the failing worker round-trip test**

Add to `#[cfg(test)] mod tests` in `src/data/worker.rs`:

```rust
#[test]
fn set_draft_request_calls_gh_and_reports_done() {
    let gh = FakeGh::new();
    let git = FakeGit::new("/tmp/repo");
    let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
    worker.send(Request::SetDraft { number: 7, draft: true });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Response::SetDraftDone { number: 7, draft: true, result: Ok(()) }) => return,
            Ok(Response::SetDraftDone { result: Err(e), .. }) => panic!("unexpected err: {e}"),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    panic!("never received SetDraftDone");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib set_draft_request_calls_gh_and_reports_done 2>&1 | tail -20`
Expected: FAIL — `Request::SetDraft` / `Response::SetDraftDone` not found.

- [ ] **Step 3: Add the `Request` and `Response` variants**

In `enum Request` (after `Merge { .. }`):

```rust
    SetDraft { number: u32, draft: bool },
```

In `enum Response` (after `MergeDone { .. }`):

```rust
    SetDraftDone {
        number: u32,
        draft: bool,
        result: Result<()>,
    },
```

- [ ] **Step 4: Handle the request in the worker loop**

In `run_worker`'s `match req`, after the `Request::Merge` arm:

```rust
            Request::SetDraft { number, draft } => {
                let result = gh.set_pr_draft(&repo_root, number, draft);
                if res_tx.send(Response::SetDraftDone { number, draft, result }).is_err() {
                    break;
                }
            }
```

- [ ] **Step 5: Run worker test to verify it passes**

Run: `cargo test --lib set_draft_request_calls_gh_and_reports_done 2>&1 | tail -20`
Expected: FAIL — crate won't compile yet: `handle_response` match in `src/app.rs` is now non-exhaustive. Proceed to Step 6.

- [ ] **Step 6: Write the failing app-response tests**

Add to `#[cfg(test)] mod tests` in `src/app.rs`:

```rust
#[test]
fn set_draft_done_flips_local_flag_without_refresh() {
    let mut st = dummy_app_state();
    let mut cache = Cache::new();
    let mut app = test_app_for_state(&mut cache);
    st.list_gen = 1;
    st.list.prs = vec![open_pr(5), open_pr(7), open_pr(8)];
    let prior_gen = st.list_gen;

    handle_response(
        &mut app,
        &mut st,
        Response::SetDraftDone { number: 7, draft: true, result: Ok(()) },
    );

    let row = st.list.prs.iter().find(|p| p.number == 7).unwrap();
    assert!(row.is_draft, "row #7 should now be draft");
    assert_eq!(st.list_gen, prior_gen, "no new refresh generation");
    assert!(!st.list_refresh_in_flight);
    assert!(!st.list.loading);
    assert!(st.list.status.contains("#7"));
}

#[test]
fn set_draft_done_err_leaves_flag_and_shows_error() {
    let mut st = dummy_app_state();
    let mut cache = Cache::new();
    let mut app = test_app_for_state(&mut cache);
    st.list.prs = vec![open_pr(7)];

    handle_response(
        &mut app,
        &mut st,
        Response::SetDraftDone {
            number: 7,
            draft: true,
            result: Err(anyhow::anyhow!("boom")),
        },
    );

    assert!(!st.list.prs[0].is_draft, "flag must not change on failure");
    assert!(st.list.status.contains("failed"));
}
```

- [ ] **Step 7: Run app tests to verify they fail**

Run: `cargo test --lib set_draft_done 2>&1 | tail -20`
Expected: FAIL — still non-exhaustive match / assertions unmet.

- [ ] **Step 8: Add the `handle_response` arms**

In `handle_response`'s `match response`, after the two `Response::MergeDone` arms:

```rust
        Response::SetDraftDone { number, draft, result: Ok(()) } => {
            if let Some(p) = st.list.prs.iter_mut().find(|p| p.number == number) {
                p.is_draft = draft;
            }
            if let Some(d) = st.review.as_mut().and_then(|r| r.detail.as_mut())
                && d.number == number
            {
                d.is_draft = draft;
            }
            st.list.status = if draft {
                format!("#{number} converted to draft")
            } else {
                format!("#{number} marked ready for review")
            };
        }
        Response::SetDraftDone { number, result: Err(e), .. } => {
            st.list.status = format!("draft toggle #{number} failed: {e}");
        }
```

- [ ] **Step 9: Run the full suite to verify green**

Run: `cargo test 2>&1 | tail -20`
Expected: PASS — all tests, including the new worker + app tests.

- [ ] **Step 10: Commit**

```bash
git add src/data/worker.rs src/app.rs
git commit -m "feat(worker): SetDraft request applies draft state locally"
```

---

### Task 3: `keys.rs` action + app dispatch on `d`

Adding `Action::ToggleDraft` makes `handle_action`'s match non-exhaustive, so the key binding and its dispatch arm ship together.

**Files:**
- Modify: `src/keys.rs`
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `Request::SetDraft` (Task 2).
- Produces: `Action::ToggleDraft`; `d` bound in `list()` and `review()`; `toggle_draft(app, st)` in `app.rs`.

- [ ] **Step 1: Write the failing key-dispatch tests**

Add to `#[cfg(test)] mod tests` in `src/keys.rs`:

```rust
#[test]
fn list_d_toggles_draft() {
    assert_eq!(dispatch(FocusedView::List, k('d')), Action::ToggleDraft);
}

#[test]
fn review_d_toggles_draft() {
    assert_eq!(dispatch(FocusedView::Review, k('d')), Action::ToggleDraft);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib toggles_draft 2>&1 | tail -20`
Expected: FAIL — `Action::ToggleDraft` not found.

- [ ] **Step 3: Add the `Action` variant**

In `enum Action`, add under the `// PR list` group (after `ListMerge`):

```rust
    ToggleDraft,
```

- [ ] **Step 4: Bind `d` in both contexts**

In `fn list`, add before the `_ => Action::Nothing` arm:

```rust
        KeyCode::Char('d') => Action::ToggleDraft,
```

In `fn review`, add after the `Ctrl-d` half-page arm (plain `d`, so it must come after the CONTROL-guarded arm), before `_ => Action::Nothing`:

```rust
        KeyCode::Char('d') => Action::ToggleDraft,
```

- [ ] **Step 5: Run key tests to verify they pass**

Run: `cargo test --lib toggles_draft 2>&1 | tail -20`
Expected: FAIL — crate won't compile: `handle_action` match in `src/app.rs` is now non-exhaustive. Proceed to Step 6.

- [ ] **Step 6: Write the failing app-action test**

Add to `#[cfg(test)] mod tests` in `src/app.rs`:

```rust
#[test]
fn toggle_draft_action_sends_inverted_state() {
    let mut st = dummy_app_state();
    let mut cache = Cache::new();
    let mut app = test_app_for_state(&mut cache);
    // #7 is ready (open_pr sets is_draft=false); toggling must request draft=true.
    st.list.prs = vec![open_pr(7)];
    st.list.selected = 0;
    st.focused = FocusedView::List;

    handle_action(&mut app, &mut st, Action::ToggleDraft);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        assert!(std::time::Instant::now() < deadline, "no SetDraftDone");
        if let Ok(Response::SetDraftDone { number: 7, draft, result: Ok(()) }) =
            app.worker.rx.recv_timeout(std::time::Duration::from_millis(200))
        {
            assert!(draft, "ready PR must toggle to draft=true");
            break;
        }
    }
    assert!(st.list.status.contains("#7"));
}
```

- [ ] **Step 7: Run to verify it fails**

Run: `cargo test --lib toggle_draft_action_sends_inverted_state 2>&1 | tail -20`
Expected: FAIL — non-exhaustive match / `toggle_draft` missing.

- [ ] **Step 8: Add the dispatch arm + handler**

In `handle_action`'s `match action`, after `Action::ListMerge => open_merge(st),`:

```rust
        Action::ToggleDraft => toggle_draft(app, st),
```

Add the handler next to `open_merge` in `src/app.rs`:

```rust
fn toggle_draft(app: &mut App, st: &mut AppState) {
    let target = match st.focused {
        FocusedView::Review => st.current_pr,
        _ => st.list.visible_prs().get(st.list.selected).map(|p| p.number),
    };
    let Some(number) = target else { return };
    let Some(is_draft) = st.list.prs.iter().find(|p| p.number == number).map(|p| p.is_draft)
    else {
        return;
    };
    let draft = !is_draft;
    app.request(Request::SetDraft { number, draft });
    st.list.status = if draft {
        format!("converting #{number} to draft…")
    } else {
        format!("marking #{number} ready…")
    };
}
```

- [ ] **Step 9: Run the full suite to verify green**

Run: `cargo test 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add src/keys.rs src/app.rs
git commit -m "feat(keys): bind d to toggle PR draft in list and review"
```

---

### Task 4: draft marker in review header + help text

**Files:**
- Modify: `src/view/pr_review.rs`
- Modify: `src/view/help.rs`

**Interfaces:**
- Consumes: `PrDetail.is_draft` (existing).
- Produces: review header shows `· draft` when the PR is a draft.

- [ ] **Step 1: Write the failing header tests**

Add to `#[cfg(test)] mod tests` in `src/view/pr_review.rs`:

```rust
#[test]
fn header_shows_draft_marker_when_draft() {
    let mut r = fixture_review_state();
    r.detail.as_mut().unwrap().is_draft = true;
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    term.draw(|f| {
        let area = f.area();
        render(f, area, &r)
    })
    .unwrap();
    let header = buffer_line(term.backend().buffer(), 0);
    assert!(header.contains("· draft"), "expected draft marker, got {header:?}");
}

#[test]
fn header_hides_draft_marker_when_ready() {
    let mut r = fixture_review_state();
    r.detail.as_mut().unwrap().is_draft = false;
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    term.draw(|f| {
        let area = f.area();
        render(f, area, &r)
    })
    .unwrap();
    let header = buffer_line(term.backend().buffer(), 0);
    assert!(!header.contains("· draft"), "ready PR must not show marker, got {header:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib header_ 2>&1 | tail -20`
Expected: FAIL — `header_shows_draft_marker_when_draft` fails (no marker rendered).

- [ ] **Step 3: Render the marker**

In `render_header`, replace the `Some(d) => format!( … )` block with:

```rust
        Some(d) => format!(
            "  prpr · #{} {} · {} · {} ← {}{}",
            d.number,
            d.title,
            d.author.login,
            d.base_ref_name,
            d.head_ref_name,
            if d.is_draft { " · draft" } else { "" },
        ),
```

- [ ] **Step 4: Run header tests to verify they pass**

Run: `cargo test --lib header_ 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Add help entries**

In `src/view/help.rs` `HELP_TEXT`, in the `PR list` block after the `m  merge modal` line:

```rust
    "    d            toggle draft",
```

In the `PR review` block after the `f  file picker … m  merge modal` line:

```rust
    "    d            toggle draft",
```

- [ ] **Step 6: Run the full suite to verify green**

Run: `cargo test 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/view/pr_review.rs src/view/help.rs
git commit -m "feat(view): draft marker in review header + help entries"
```

---

## Self-Review

**Spec coverage:**
- Key `d`, immediate toggle, both contexts → Task 3. ✓
- `gh pr ready` mapping → Task 1. ✓
- Worker request/response → Task 2. ✓
- Local `is_draft` flip, no refresh → Task 2. ✓
- Review header marker → Task 4. ✓
- Help text → Task 4. ✓
- No in-flight lock → nothing added; nothing to do. ✓

**Placeholder scan:** none — every step has concrete code and commands.

**Type consistency:** `set_pr_draft(&self, &Path, u32, bool) -> Result<()>` used identically in Tasks 1–2. `Request::SetDraft { number, draft }` and `Response::SetDraftDone { number, draft, result }` consistent across Tasks 2–3. `Action::ToggleDraft` consistent across Tasks 3. `toggle_draft(app, st)` matches its call site.
