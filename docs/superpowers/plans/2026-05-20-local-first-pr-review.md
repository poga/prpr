# Local-First PR Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drop the per-PR `PrPackage` cache. Review data lives in `PrReviewState`; per-file blame runs on demand for the file in view.

**Architecture:** Worker stops eagerly blaming every file on `LoadPr`. Replace `LoadPr` with `OpenPr` (emits `PrDetail` + `PrDiff` only) and add `BlameFile` (blames one path). `PrReviewState` becomes the owner of the open PR's detail/files/colors/commit_stats; `Cache` keeps only the `gh pr list` result.

**Tech Stack:** Rust 2024, `ratatui` for TUI, `mpsc` channels for worker requests, `gh`/`git` CLI shells, `pretty_assertions` for tests, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-05-20-local-first-pr-review-design.md`

---

## File Structure

**Modified:**
- `src/view/pr_review.rs` — `PrReviewState` grows to own PR data; `ColorState` enum added; render fns drop their `&PrPackage` parameter.
- `src/data/worker.rs` — `Request::LoadPr` → `Request::OpenPr`; add `Request::BlameFile`; remove all-files blame loop from `run_load`; remove `Response::PrColorsDone`.
- `src/app.rs` — Response handlers populate `PrReviewState` instead of `Cache.packages`; navigation reads from `st.review`; new dispatches for `BlameFile` on PR open and file navigation; `Action::Refresh` in review clears review data.
- `src/data/cache.rs` — drop `PrPackage`, `packages` HashMap, and all per-PR cache methods. Keep `list` only.

**Tests modified:** existing tests in `worker.rs`, `app.rs`, `pr_review.rs`, `cache.rs` are updated as their dependencies change.

---

## Task 1: Add `ColorState` enum and data fields on `PrReviewState`

**Files:**
- Modify: `src/view/pr_review.rs:11-24` — extend `PrReviewState`, add `ColorState`.

This task only adds types and fields. No behavior change. Existing code keeps using `PrPackage` from the cache.

- [ ] **Step 1: Write the failing test**

Add to `src/view/pr_review.rs` at the bottom of `mod tests`:

```rust
    #[test]
    fn pr_review_state_default_has_empty_data_fields() {
        let st = PrReviewState::default();
        assert!(st.detail.is_none());
        assert!(st.files.is_empty());
        assert!(st.colors.is_empty());
        assert!(st.commit_stats.is_empty());
    }

    #[test]
    fn color_state_distinguishes_loading_from_ready() {
        let loading = ColorState::Loading;
        let ready = ColorState::Ready(LineColors {
            head: vec![],
            delete: HashMap::new(),
        });
        assert!(matches!(loading, ColorState::Loading));
        assert!(matches!(ready, ColorState::Ready(_)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib view::pr_review::tests::pr_review_state_default_has_empty_data_fields view::pr_review::tests::color_state_distinguishes_loading_from_ready`
Expected: FAIL with compile errors (`ColorState` undefined, fields don't exist).

- [ ] **Step 3: Add `ColorState` and extend `PrReviewState`**

In `src/view/pr_review.rs` add at the top of the file alongside existing imports:

```rust
use crate::data::pr::PrDetail;
use crate::render::attribution::CommitStats;
```

Replace the `PrReviewState` struct (currently lines 17-24) with:

```rust
#[derive(Debug, Default)]
pub struct PrReviewState {
    // Data owned by the review pane (populated by worker responses).
    pub detail: Option<PrDetail>,
    pub files: Vec<FileDiff>,
    pub colors: HashMap<String, ColorState>,
    pub commit_stats: HashMap<String, CommitStats>,

    // View state.
    pub file_index: usize,
    pub cursor_line: usize,
    pub scroll: u16,
    pub show_sha_margin: bool,
    pub status: String,
}

#[derive(Debug, Clone)]
pub enum ColorState {
    Loading,
    Ready(LineColors),
}
```

- [ ] **Step 4: Run the new tests**

Run: `cargo test --lib view::pr_review::tests::pr_review_state_default_has_empty_data_fields view::pr_review::tests::color_state_distinguishes_loading_from_ready`
Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: PASS (155 tests + the two new ones).

- [ ] **Step 6: Commit**

```bash
git add src/view/pr_review.rs
git commit -m "feat(view): add ColorState and data fields on PrReviewState"
```

---

## Task 2: Dual-populate `PrReviewState` from worker responses

**Files:**
- Modify: `src/app.rs:294-348` — extend `PrDetail`, `PrDiff`, `PrFileColors` handlers to also write into `st.review`.

After this task, `st.review` holds the same data as `app.cache.get(num)` does today. Cache stays primary; we'll switch readers in Task 3/4.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/app.rs` (near the end, before the closing `}` of `mod tests`):

```rust
    #[test]
    fn pr_detail_response_populates_review_state_detail() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            status: "loading…".into(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDetail { number, result: Ok(detail.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.detail.as_ref().unwrap().number, number);
        assert_eq!(r.commit_stats.len(), detail.commits.len(),
            "commit_stats zero-filled for every PR commit");
    }

    #[test]
    fn pr_diff_response_populates_review_state_files() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            status: "loading diff…".into(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        cache.insert_partial(detail);
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDiff { number, result: Ok(files.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.files.len(), files.len());
    }

    #[test]
    fn pr_file_colors_response_marks_path_ready_in_review() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let head_oid = detail.head_ref_oid.clone();
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let path = files[0].path.clone();
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: files.clone(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        cache.insert_partial(detail);
        cache.update_diff(number, &head_oid, files);
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrFileColors {
                number,
                head_oid: head_oid.clone(),
                path: path.clone(),
                colors: LineColors { head: vec![], delete: std::collections::HashMap::new() },
                stats: std::collections::HashMap::new(),
            },
        );

        let r = st.review.as_ref().unwrap();
        assert!(matches!(r.colors.get(&path), Some(crate::view::pr_review::ColorState::Ready(_))));
    }
```

If `fixture_pr_detail` and `test_app_for_state`/`dummy_app_state` don't already exist as test helpers in `src/app.rs`, locate the existing helpers used by the other app tests in this file (search for `fn fixture_` and `fn dummy_app_state`); use the same names and shapes. If a helper truly is missing, add the smallest version that compiles using the existing test fixtures.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app::tests::pr_detail_response_populates_review_state_detail app::tests::pr_diff_response_populates_review_state_files app::tests::pr_file_colors_response_marks_path_ready_in_review`
Expected: FAIL — fields on `PrReviewState` are not populated by handlers yet.

- [ ] **Step 3: Update the `PrDetail` handler**

In `src/app.rs` find `Response::PrDetail { number, result: Ok(detail) }` handler (around line 294). Replace its body with:

```rust
Response::PrDetail { number, result: Ok(detail) } => {
    app.cache.insert_partial(detail.clone());
    if let Some(r) = st.review.as_mut()
        && st.current_pr == Some(number)
    {
        let zero_stats = detail
            .commits
            .iter()
            .map(|c| (c.oid.clone(), crate::render::attribution::CommitStats::default()))
            .collect();
        r.detail = Some(detail);
        r.commit_stats = zero_stats;
        r.status = "loading diff…".into();
    }
}
```

- [ ] **Step 4: Update the `PrDiff` handler**

Find `Response::PrDiff { number, result: Ok(files) }` (around line 310). Replace its body with:

```rust
Response::PrDiff { number, result: Ok(files) } => {
    let head_oid = app
        .cache
        .get(number)
        .map(|p| p.detail.head_ref_oid.clone());
    if let Some(head) = head_oid {
        app.cache.update_diff(number, &head, files.clone());
    }
    if let Some(r) = st.review.as_mut()
        && st.current_pr == Some(number)
    {
        r.files = files;
        r.status = format!("{} files", r.files.len());
    }
}
```

- [ ] **Step 5: Update the `PrFileColors` handler**

Find `Response::PrFileColors { number, head_oid, path, colors, stats }` (around line 332). Replace with:

```rust
Response::PrFileColors {
    number,
    head_oid,
    path,
    colors,
    stats,
} => {
    app.cache.add_file_colors(number, &head_oid, path.clone(), colors.clone(), stats.clone());
    if let Some(r) = st.review.as_mut()
        && st.current_pr == Some(number)
    {
        r.colors.insert(path, crate::view::pr_review::ColorState::Ready(colors));
        for (oid, s) in stats {
            let entry = r.commit_stats.entry(oid).or_default();
            entry.adds += s.adds;
            entry.dels += s.dels;
        }
    }
}
```

- [ ] **Step 6: Run the new tests**

Run: `cargo test --lib app::tests::pr_detail_response_populates_review_state_detail app::tests::pr_diff_response_populates_review_state_files app::tests::pr_file_colors_response_marks_path_ready_in_review`
Expected: PASS.

- [ ] **Step 7: Run the full test suite**

Run: `cargo test`
Expected: PASS (all prior tests + three new ones).

- [ ] **Step 8: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): mirror worker responses into PrReviewState"
```

---

## Task 3: Render from `PrReviewState`; drop `&PrPackage` from view fns

**Files:**
- Modify: `src/view/pr_review.rs:26-189` — render fns take `&PrReviewState` only.
- Modify: `src/view/pr_review.rs:191-` — test module updates fixtures.
- Modify: `src/app.rs:397-411` — call site for `pr_review::render`.

After this task, the review UI reads exclusively from `PrReviewState`. Cache is still being maintained but no view reads it.

- [ ] **Step 1: Update the test fixtures in `pr_review.rs`**

Replace `fixture_pkg() -> PrPackage` and all tests that use it. In `src/view/pr_review.rs` `mod tests`, replace the helper:

```rust
    fn fixture_review_state() -> PrReviewState {
        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let files = parse_diff(include_str!("../../tests/fixtures/diff_basic.patch")).unwrap();
        PrReviewState {
            detail: Some(detail),
            files,
            colors: HashMap::new(),
            commit_stats: HashMap::new(),
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
        }
    }
```

Walk through every test in this module (`renders_pr_number_in_header`, `renders_no_commit_strip`, `binary_file_renders_placeholder`, `diff_body_shows_loading_when_pkg_files_empty`, `file_bar_uses_detail_files_when_pkg_files_empty`, and the two new ones from Task 1). Update each to call `fixture_review_state()` and pass it as a single `&PrReviewState` to `render(...)`. Drop the `pkg` parameter and any `let st = PrReviewState::default()` lines — the fixture is now the single source.

For tests that mutate the package (e.g. `pkg.files = vec![]`), mutate the `PrReviewState` instead (`r.files = vec![]`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib view::pr_review::tests`
Expected: FAIL — `render` and helpers still take `&PrPackage`; signatures don't match.

- [ ] **Step 3: Change render function signatures**

In `src/view/pr_review.rs`:

Drop the `use crate::data::cache::PrPackage;` import (line 11). Add:
```rust
use crate::data::pr::PrDetail;
```
(if not already added in Task 1).

Change `render`:
```rust
pub fn render(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // spacer
            Constraint::Length(2), // file bar
            Constraint::Min(1),    // body
            Constraint::Length(3), // status
        ])
        .split(area);

    render_header(f, chunks[0], st);
    render_file_bar(f, chunks[2], st);
    render_diff_body(f, chunks[3], st);
    render_status(f, chunks[4], st);
}
```

Change `render_header` to read from `st.detail`:
```rust
fn render_header(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let header = match &st.detail {
        Some(d) => format!(
            "  prpr · #{} {} · {} · {} ← {}",
            d.number, d.title, d.author.login, d.base_ref_name, d.head_ref_name,
        ),
        None => "  prpr · loading…".to_string(),
    };
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(TEXT)),
        area,
    );
}
```

Change `render_file_bar`:
```rust
fn render_file_bar(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let paths = file_paths(st);
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

Add the helper that replaces `PrPackage::file_paths`:
```rust
/// Path list with fallback. While `git diff` parsing hasn't completed, `files`
/// is empty; fall back to `detail.files` so the file bar and picker render
/// immediately.
pub fn file_paths(st: &PrReviewState) -> Vec<&str> {
    if st.files.is_empty() {
        st.detail
            .as_ref()
            .map(|d| d.files.iter().map(|f| f.path.as_str()).collect())
            .unwrap_or_default()
    } else {
        st.files.iter().map(|f| f.path.as_str()).collect()
    }
}

/// Total file count using the same fallback as `file_paths`.
pub fn file_count(st: &PrReviewState) -> usize {
    if st.files.is_empty() {
        st.detail.as_ref().map(|d| d.files.len()).unwrap_or(0)
    } else {
        st.files.len()
    }
}
```

Change `render_diff_body`:
```rust
fn render_diff_body(f: &mut Frame, area: Rect, st: &PrReviewState) {
    if st.files.is_empty() {
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
    let Some(file) = st.files.get(st.file_index) else {
        return;
    };
    if file.binary {
        f.render_widget(
            Paragraph::new("  binary file, not displayed").style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let lines = body_lines(file, &st.colors);
    f.render_widget(Paragraph::new(lines).scroll((st.scroll, 0)), area);
}
```

Change `body_lines` to take the new map type:
```rust
fn body_lines<'a>(file: &'a FileDiff, colors: &'a HashMap<String, ColorState>) -> Vec<Line<'a>> {
    let lookup = colors.get(&file.path).and_then(|c| match c {
        ColorState::Ready(lc) => Some(lc),
        ColorState::Loading => None,
    });
    let ext = ext_of(&file.path);
    file.lines
        .iter()
        .map(|l| {
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
            render_line(l, head, base, ext)
        })
        .collect()
}
```

Change `render_status` to take `&PrReviewState` only and read `st.files`:
```rust
fn render_status(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let cursor_info = st
        .files
        .get(st.file_index)
        .and_then(|file| {
            file.lines
                .iter()
                .filter(|l| !l.is_hunk_header)
                .nth(st.cursor_line)
                .and_then(|l| l.new_lineno.or(l.old_lineno))
        })
        .map(|n| format!("line {n}"))
        .unwrap_or_default();
    let status_text = if crate::render::spinner::looks_in_progress(&st.status) {
        format!("{} {}", crate::render::spinner::glyph(), st.status)
    } else if cursor_info.is_empty() {
        st.status.clone()
    } else {
        String::new()
    };
    let line = match (cursor_info.is_empty(), status_text.is_empty()) {
        (true, true) => String::new(),
        (false, true) => cursor_info,
        (true, false) => status_text,
        (false, false) => format!("{cursor_info}    {status_text}"),
    };
    f.render_widget(
        Paragraph::new(format!("  {line}")).style(Style::default().fg(SUBTEXT0)),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(
            "  j/k or ↑/↓ scroll   Ctrl-d/u half-page   PgUp/PgDn page   Home/End top/bottom",
        )
        .style(Style::default().fg(OVERLAY1)),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(
            "  Tab/↵ next file   Shift-Tab prev   f files   c commits   m merge   s sha   ? help   q back",
        )
        .style(Style::default().fg(OVERLAY0)),
        chunks[2],
    );
}
```

- [ ] **Step 4: Update the call site in `app.rs`**

In `src/app.rs` find the render call (around line 401):

```rust
            let pkg = st.current_pr.and_then(|n| app.cache.get(n));
            if let (Some(pkg), Some(review)) = (pkg, st.review.as_ref()) {
                crate::view::pr_review::render(f, area, pkg, review);
            } else {
                // loading placeholder
            }
```

Replace with:

```rust
            if let Some(review) = st.review.as_ref() {
                if review.detail.is_some() {
                    crate::view::pr_review::render(f, area, review);
                } else {
                    let text = format!("{} loading…", crate::render::spinner::glyph());
                    let msg = ratatui::widgets::Paragraph::new(text)
                        .style(ratatui::style::Style::default().fg(crate::render::style::OVERLAY1))
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(msg, area);
                }
            }
```

- [ ] **Step 5: Run the view tests**

Run: `cargo test --lib view::pr_review::tests`
Expected: PASS.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: PASS. App tests still use the cache for their navigation reads — they're untouched by this task.

- [ ] **Step 7: Commit**

```bash
git add src/view/pr_review.rs src/app.rs
git commit -m "refactor(view): render PR review from PrReviewState, drop PrPackage arg"
```

---

## Task 4: Switch app.rs navigation/picker reads to `PrReviewState`

**Files:**
- Modify: `src/app.rs` — replace remaining `app.cache.get(num)` reads in `Action` handlers and helper fns with reads from `st.review`.

After this task, the only remaining cache.get / cache.insert_partial / cache.update_diff / cache.add_file_colors calls in app.rs are inside `handle_response`. Task 8 will remove those.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/app.rs`:

```rust
    #[test]
    fn cycle_file_reads_files_from_review_state_not_cache() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2, "fixture needs at least 2 files for this test");

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: files.clone(),
            file_index: 0,
            ..Default::default()
        });
        // Cache is intentionally empty — proves the read is from st.review.
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        assert_eq!(st.review.as_ref().unwrap().file_index, 1);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib app::tests::cycle_file_reads_files_from_review_state_not_cache`
Expected: FAIL — `cycle_file` returns early because `cache.get(num)` is `None`.

- [ ] **Step 3: Update navigation helpers and action handlers**

In `src/app.rs`, rewrite each of the sites listed below to read from `st.review` instead of `app.cache.get(num)`.

(a) `move_review` (around line 701):
```rust
fn move_review(_app: &App, st: &mut AppState, delta: i32) {
    let Some(r) = st.review.as_mut() else { return };
    let Some(file) = r.files.get(r.file_index) else { return };
    let max_scr = max_scroll(file.lines.len()) as i64;
    let max_cur = max_cursor_line(file) as i64;
    let new_scroll = (r.scroll as i64 + delta as i64).clamp(0, max_scr);
    let new_cursor = (r.cursor_line as i64 + delta as i64).clamp(0, max_cur);
    r.scroll = new_scroll as u16;
    r.cursor_line = new_cursor as usize;
}
```

(b) `cycle_file` (around line 718):
```rust
fn cycle_file(_app: &App, st: &mut AppState, delta: i32) {
    let Some(r) = st.review.as_mut() else { return };
    let n = crate::view::pr_review::file_count(r) as i32;
    if n == 0 { return; }
    let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
    r.file_index = new_idx;
    r.cursor_line = 0;
    r.scroll = 0;
}
```

(c) `Action::Bottom` (around line 597-605):
```rust
Action::Bottom => {
    if let Some(r) = st.review.as_mut()
        && let Some(file) = r.files.get(r.file_index)
    {
        r.scroll = max_scroll(file.lines.len());
        r.cursor_line = max_cursor_line(file);
    }
}
```

(d) `Action::OpenFilePicker` (around line 608-617):
```rust
Action::OpenFilePicker => {
    if let Some(r) = st.review.as_ref() {
        let paths: Vec<String> = crate::view::pr_review::file_paths(r)
            .into_iter().map(String::from).collect();
        let current = crate::view::pr_review::file_paths(r).get(r.file_index).copied();
        st.picker = Some(FilePickerState::new(paths, current));
        st.focused = FocusedView::FilePicker;
    }
}
```

(e) `Action::OpenCommitsModal` (around line 618-635):
```rust
Action::OpenCommitsModal => {
    if let Some(r) = st.review.as_ref()
        && let Some(d) = r.detail.as_ref()
    {
        let rows = commits_modal::build_rows(
            &d.commits,
            &r.commit_stats,
            app.config.window_size,
            Utc::now(),
        );
        st.commits = Some(CommitsModalState {
            rows,
            selected: 0,
            ..Default::default()
        });
        st.focused = FocusedView::CommitsModal;
    }
}
```

(f) Picker `Enter` handler (around line 776-789 inside `handle_file_picker`):
```rust
KeyCode::Enter => {
    let chosen = picker.matches().get(picker.selected).map(|s| (*s).clone());
    if let (Some(path), Some(r)) = (chosen, st.review.as_mut()) {
        let idx = crate::view::pr_review::file_paths(r)
            .iter()
            .position(|p| *p == path.as_str());
        if let Some(idx) = idx {
            r.file_index = idx;
            r.cursor_line = 0;
            r.scroll = 0;
        }
    }
    st.picker = None;
    st.focused = FocusedView::Review;
    return;
}
```

- [ ] **Step 4: Run the new test**

Run: `cargo test --lib app::tests::cycle_file_reads_files_from_review_state_not_cache`
Expected: PASS.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "refactor(app): read PR data from PrReviewState in navigation and picker"
```

---

## Task 5: Replace `LoadPr` with `OpenPr`; add `BlameFile`; drop blame loop and `PrColorsDone`

**Files:**
- Modify: `src/data/worker.rs` — new request/response shape, slim `run_load`.
- Modify: `src/app.rs` — `Action::ListOpen`, `Action::Refresh` dispatch `OpenPr` instead of `LoadPr`; remove `PrColorsDone` arm.

After this task, opening a PR shows the diff with no colors (the next task wires up `BlameFile` dispatch).

- [ ] **Step 1: Write the failing worker tests**

Replace the existing `load_pr_streams_detail_diff_then_per_file_colors` test in `src/data/worker.rs` (around line 454) with two tests:

```rust
    #[test]
    fn open_pr_emits_only_detail_and_diff_no_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;
        let pr = pr_from_fixture(&detail);

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        git.refs.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs.insert(format!("origin/{}", pr.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::OpenPr(pr));

        let mut got_detail = false;
        let mut got_diff = false;
        let mut color_events = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrDiff { number: n, result: Ok(_) }) if n == number => {
                    got_diff = true;
                }
                Ok(Response::PrFileColors { .. }) => color_events += 1,
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) => {}
                Err(_) => {
                    if got_detail && got_diff { break; }
                }
            }
        }
        assert!(got_detail, "never received PrDetail");
        assert!(got_diff, "never received PrDiff");
        assert_eq!(color_events, 0, "OpenPr must not emit color events");
    }

    #[test]
    fn blame_file_emits_one_pr_file_colors_for_requested_path() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames.insert((head_sha.clone(), "src/sched.rs".into()), porcelain);

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::BlameFile {
            number,
            head_oid: head_sha.clone(),
            base_oid: base_sha.clone(),
            path: "src/sched.rs".into(),
            commits: detail.commits.iter().map(|c| c.oid.clone()).collect(),
        });

        let mut got = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrFileColors { number: n, path, .. }) if n == number => {
                    assert_eq!(path, "src/sched.rs");
                    got += 1;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(got, 1, "BlameFile should emit exactly one PrFileColors for the requested path");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib data::worker::tests::open_pr_emits_only_detail_and_diff_no_colors data::worker::tests::blame_file_emits_one_pr_file_colors_for_requested_path`
Expected: FAIL — compile error: `Request::OpenPr`, `Request::BlameFile` don't exist.

- [ ] **Step 3: Update `Request` and `Response` enums**

In `src/data/worker.rs` replace the `Request` enum (currently lines 28-39):

```rust
#[derive(Debug)]
pub enum Request {
    /// Refresh the PR list. `generation` is echoed in both responses so the UI
    /// can drop stale results from a superseded refresh cycle.
    RefreshList { generation: u32 },
    /// Resolve head/base oids, fetch commits + diff, emit `PrDetail` + `PrDiff`.
    /// No per-file blame — issue `BlameFile` for each path that needs coloring.
    OpenPr(crate::data::pr::Pr),
    /// Blame + log-patches for one file, emits one `PrFileColors`.
    BlameFile {
        number: u32,
        head_oid: String,
        base_oid: String,
        path: String,
        commits: Vec<String>,
    },
    /// Run `gh pr merge <number> --<method>`.
    Merge { number: u32, method: String },
}
```

In the `Response` enum (around line 64-110), remove the `PrColorsDone` variant entirely.

- [ ] **Step 4: Slim `run_load` (rename to `run_open_pr`) and add `run_blame_file`**

In `run_worker` (around line 168), update the dispatch:

```rust
while let Ok(req) = req_rx.recv() {
    match req {
        Request::RefreshList { generation } => {
            // unchanged body
        }
        Request::OpenPr(pr) => {
            run_open_pr(&*gh, &*git, &repo_root, &res_tx, pr);
        }
        Request::BlameFile { number, head_oid, base_oid, path, commits } => {
            run_blame_file(&*git, &repo_root, &res_tx, number, &head_oid, &base_oid, &path, &commits, window_size);
        }
        Request::Merge { number, method } => {
            let result = gh.merge_pr(&repo_root, number, &method);
            if res_tx.send(Response::MergeDone { number, result }).is_err() {
                break;
            }
        }
    }
}
```

Replace the `fn run_load(...)` (lines 235-397) with two new functions:

```rust
fn run_open_pr(
    _gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    pr: crate::data::pr::Pr,
) {
    let number = pr.number;
    let head_ref = format!("refs/prpr/pr-{number}");
    let base_ref = format!("origin/{}", pr.base_ref_name);
    let head_oid = match git.rev_parse(repo_root, &head_ref) {
        Ok(o) => o,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("resolving {head_ref} (try `r` to refresh): {e:#}"),
            });
            return;
        }
    };
    let base_oid = match git.rev_parse(repo_root, &base_ref) {
        Ok(o) => o,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("resolving {base_ref}: {e:#}"),
            });
            return;
        }
    };

    let (commits_res, diff_res) = thread::scope(|s| {
        let commits_h = s.spawn(|| git.log_commits(repo_root, &base_oid, &head_oid));
        let diff_h = s.spawn(|| {
            git.diff(repo_root, &base_oid, &head_oid)
                .and_then(|s| parse_diff(&s))
        });
        (commits_h.join().unwrap(), diff_h.join().unwrap())
    });
    let commits = match commits_res {
        Ok(c) => c,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError { number, error: format!("{e:#}") });
            return;
        }
    };
    let files = match diff_res {
        Ok(f) => f,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError { number, error: format!("{e:#}") });
            return;
        }
    };

    let detail = crate::data::pr::PrDetail {
        number: pr.number,
        title: pr.title.clone(),
        is_draft: pr.is_draft,
        state: pr.state,
        author: pr.author.clone(),
        base_ref_name: pr.base_ref_name.clone(),
        base_ref_oid: base_oid.clone(),
        head_ref_name: pr.head_ref_name.clone(),
        head_ref_oid: head_oid.clone(),
        mergeable: pr.mergeable.clone(),
        status_check_rollup: pr.status_check_rollup.clone(),
        review_decision: pr.review_decision,
        commits,
        files: files
            .iter()
            .map(|f| crate::data::pr::FileMeta {
                path: f.path.clone(),
                additions: 0,
                deletions: 0,
            })
            .collect(),
    };
    let _ = res_tx.send(Response::PrDetail { number, result: Ok(detail) });
    let _ = res_tx.send(Response::PrDiff { number, result: Ok(files) });
}

fn run_blame_file(
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    number: u32,
    head_oid: &str,
    base_oid: &str,
    path: &str,
    commits: &[String],
    window_size: usize,
) {
    let head = git
        .blame(repo_root, head_oid, path)
        .map(|s| parse_blame(&s))
        .unwrap_or_else(|_| Blame { line_shas: vec![] });
    let log_out = git
        .log_patches(repo_root, base_oid, head_oid, path)
        .unwrap_or_default();
    let deletes = parse_deletions(&log_out);
    let lc = attribute_file(commits, window_size, &head, &deletes);
    let per = commit_stats_for_file(commits, &head, &deletes);
    let _ = res_tx.send(Response::PrFileColors {
        number,
        head_oid: head_oid.to_string(),
        path: path.to_string(),
        colors: lc,
        stats: per,
    });
}
```

Delete the now-unused `fn blame_file(...)` helper (it lives around line 401).

- [ ] **Step 5: Update app.rs dispatch sites and remove `PrColorsDone` handler**

In `src/app.rs`:

`Action::ListOpen` (around line 568): change `app.request(Request::LoadPr(pr));` to `app.request(Request::OpenPr(pr));`

`Action::Refresh` (around line 657): change `app.request(Request::LoadPr(pr));` to `app.request(Request::OpenPr(pr));`

In `handle_response`, remove the `Response::PrColorsDone { … }` match arm entirely (around line 341-348).

- [ ] **Step 6: Update the end-to-end app test**

In `src/app.rs` test module, find `end_to_end_pr_load_progresses_through_partial_states` (around line 1480). Replace it with the local-first variant:

```rust
    #[test]
    fn end_to_end_open_pr_progresses_through_partial_states() {
        // OpenPr now emits PrDetail then PrDiff and stops; no colors.
        let detail = fixture_pr_detail();
        let number = detail.number;
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();

        let gh = crate::data::gh::fakes::FakeGh::new();
        let mut git = crate::data::git::fakes::FakeGit::new("/tmp/repo");
        git.refs.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs.insert(format!("origin/{}", detail.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut app = App::new(
            "/tmp/repo".into(),
            std::sync::Arc::new(gh),
            std::sync::Arc::new(git),
            crate::config::Config::default(),
        );
        let mut st = AppState::new("repo".into(), "main".into());
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            status: "loading…".into(),
            ..Default::default()
        });

        let pr = crate::data::pr::Pr {
            number: detail.number,
            title: detail.title.clone(),
            is_draft: detail.is_draft,
            state: detail.state,
            author: detail.author.clone(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: detail.base_ref_name.clone(),
            head_ref_name: detail.head_ref_name.clone(),
            labels: vec![],
            status_check_rollup: detail.status_check_rollup.clone(),
            review_decision: detail.review_decision,
            mergeable: detail.mergeable.clone(),
        };
        app.request(Request::OpenPr(pr));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut saw_detail = false;
        let mut saw_diff = false;
        while std::time::Instant::now() < deadline && !(saw_detail && saw_diff) {
            if let Ok(resp) = app.worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                let is_detail = matches!(resp, Response::PrDetail { .. });
                let is_diff = matches!(resp, Response::PrDiff { .. });
                handle_response(&mut app, &mut st, resp);
                if is_detail {
                    saw_detail = true;
                    let r = st.review.as_ref().unwrap();
                    assert!(r.detail.is_some());
                    assert_eq!(r.status, "loading diff…");
                }
                if is_diff {
                    saw_diff = true;
                    let r = st.review.as_ref().unwrap();
                    assert!(!r.files.is_empty());
                    assert_eq!(r.status, format!("{} files", r.files.len()));
                }
            }
        }
        assert!(saw_detail && saw_diff, "missed an event");
    }
```

- [ ] **Step 7: Run worker tests**

Run: `cargo test --lib data::worker::tests`
Expected: PASS.

- [ ] **Step 8: Run full suite**

Run: `cargo test`
Expected: PASS. Any remaining `LoadPr` / `PrColorsDone` references will surface as compile errors — fix them by replacing with `OpenPr` and removing handler code respectively.

- [ ] **Step 9: Commit**

```bash
git add src/data/worker.rs src/app.rs
git commit -m "refactor(worker): replace LoadPr with OpenPr + on-demand BlameFile"
```

---

## Task 6: App dispatches `BlameFile` on PR open and file navigation

**Files:**
- Modify: `src/app.rs` — on `Response::PrDiff` dispatch `BlameFile` for current file; on `cycle_file` / picker Enter dispatch `BlameFile` if the new file isn't already loading or ready.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/app.rs`:

```rust
    #[test]
    fn pr_diff_response_dispatches_blame_file_for_current_file_and_marks_loading() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let first_path = files[0].path.clone();

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            ..Default::default()
        });
        let mut cache = Cache::new();
        cache.insert_partial(detail.clone());
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDiff { number, result: Ok(files.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert!(matches!(
            r.colors.get(&first_path),
            Some(crate::view::pr_review::ColorState::Loading)
        ));
    }

    #[test]
    fn cycle_file_marks_new_file_loading_when_not_yet_blamed() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2);
        let second_path = files[1].path.clone();

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            files: files.clone(),
            file_index: 0,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.file_index, 1);
        assert!(matches!(
            r.colors.get(&second_path),
            Some(crate::view::pr_review::ColorState::Loading)
        ));
    }

    #[test]
    fn cycle_file_does_not_remark_loading_or_ready_file() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2);
        let second_path = files[1].path.clone();

        let ready = crate::view::pr_review::ColorState::Ready(LineColors {
            head: vec![], delete: std::collections::HashMap::new(),
        });
        let mut colors = std::collections::HashMap::new();
        colors.insert(second_path.clone(), ready);

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            files,
            colors,
            file_index: 0,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        let r = st.review.as_ref().unwrap();
        assert!(matches!(
            r.colors.get(&second_path),
            Some(crate::view::pr_review::ColorState::Ready(_))
        ), "ready entry must not be reset to Loading");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib app::tests::pr_diff_response_dispatches_blame_file_for_current_file_and_marks_loading app::tests::cycle_file_marks_new_file_loading_when_not_yet_blamed app::tests::cycle_file_does_not_remark_loading_or_ready_file`
Expected: FAIL.

- [ ] **Step 3: Add a helper and wire `PrDiff` to dispatch `BlameFile`**

In `src/app.rs`, add a private helper above `handle_response`:

```rust
/// If `path`'s color state is unset, mark it `Loading` and dispatch a
/// `BlameFile` request for it. No-op if it's already Loading or Ready.
fn ensure_blame(app: &App, st: &mut AppState, number: u32, path: &str) {
    let Some(r) = st.review.as_mut() else { return };
    let Some(d) = r.detail.as_ref() else { return };
    if r.colors.contains_key(path) { return; }
    r.colors.insert(path.to_string(), crate::view::pr_review::ColorState::Loading);
    let commits: Vec<String> = d.commits.iter().map(|c| c.oid.clone()).collect();
    app.request(Request::BlameFile {
        number,
        head_oid: d.head_ref_oid.clone(),
        base_oid: d.base_ref_oid.clone(),
        path: path.to_string(),
        commits,
    });
}
```

In the `Response::PrDiff { number, result: Ok(files) }` handler (from Task 2), append after the existing body:

```rust
// Kick off blame for the file the user is currently looking at.
if st.current_pr == Some(number) {
    if let Some(r) = st.review.as_ref() {
        if let Some(path) = r.files.get(r.file_index).map(|f| f.path.clone()) {
            ensure_blame(app, st, number, &path);
        }
    }
}
```

- [ ] **Step 4: Wire `cycle_file` and picker Enter to dispatch `BlameFile`**

Update `cycle_file` (from Task 4):
```rust
fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let path_for_blame = {
        let Some(r) = st.review.as_mut() else { return };
        let n = crate::view::pr_review::file_count(r) as i32;
        if n == 0 { return; }
        let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
        r.file_index = new_idx;
        r.cursor_line = 0;
        r.scroll = 0;
        r.files.get(new_idx).map(|f| f.path.clone())
    };
    if let Some(path) = path_for_blame {
        ensure_blame(app, st, num, &path);
    }
}
```

In the picker `Enter` handler from Task 4 step 3(f), add the dispatch after setting `file_index`:
```rust
KeyCode::Enter => {
    let chosen = picker.matches().get(picker.selected).map(|s| (*s).clone());
    let blame_target = if let (Some(path), Some(r)) = (chosen, st.review.as_mut()) {
        let idx = crate::view::pr_review::file_paths(r)
            .iter()
            .position(|p| *p == path.as_str());
        if let Some(idx) = idx {
            r.file_index = idx;
            r.cursor_line = 0;
            r.scroll = 0;
            r.files.get(idx).map(|f| f.path.clone())
        } else { None }
    } else { None };
    if let (Some(path), Some(num)) = (blame_target, st.current_pr) {
        ensure_blame(app, st, num, &path);
    }
    st.picker = None;
    st.focused = FocusedView::Review;
    return;
}
```

- [ ] **Step 5: Run new tests**

Run: `cargo test --lib app::tests::pr_diff_response_dispatches_blame_file_for_current_file_and_marks_loading app::tests::cycle_file_marks_new_file_loading_when_not_yet_blamed app::tests::cycle_file_does_not_remark_loading_or_ready_file`
Expected: PASS.

- [ ] **Step 6: Run full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): dispatch BlameFile on PR open and file navigation"
```

---

## Task 7: `Action::Refresh` in review clears review data before re-opening

**Files:**
- Modify: `src/app.rs` — `Action::Refresh` resets review data fields.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    #[test]
    fn refresh_action_in_review_clears_data_and_dispatches_open_pr() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let pr = crate::data::pr::Pr {
            number, title: detail.title.clone(), is_draft: detail.is_draft,
            state: detail.state, author: detail.author.clone(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: detail.base_ref_name.clone(),
            head_ref_name: detail.head_ref_name.clone(),
            labels: vec![], status_check_rollup: detail.status_check_rollup.clone(),
            review_decision: detail.review_decision, mergeable: detail.mergeable.clone(),
        };

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.list.prs = vec![pr.clone()];
        let mut colors = std::collections::HashMap::new();
        colors.insert("src/sched.rs".into(), crate::view::pr_review::ColorState::Ready(
            LineColors { head: vec![], delete: std::collections::HashMap::new() }
        ));
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: vec![crate::data::diff::FileDiff {
                path: "src/sched.rs".into(),
                lines: vec![], binary: false,
            }],
            colors,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_action(&mut app, &mut st, Action::Refresh);

        let r = st.review.as_ref().unwrap();
        assert!(r.detail.is_none());
        assert!(r.files.is_empty());
        assert!(r.colors.is_empty());
        assert!(r.commit_stats.is_empty());
        assert_eq!(r.status, "loading…");
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test --lib app::tests::refresh_action_in_review_clears_data_and_dispatches_open_pr`
Expected: FAIL.

- [ ] **Step 3: Update `Action::Refresh`**

In `src/app.rs` replace the `Action::Refresh` arm:

```rust
Action::Refresh => {
    if let Some(num) = st.current_pr
        && let Some(pr) = st.list.prs.iter().find(|p| p.number == num).cloned()
    {
        if let Some(r) = st.review.as_mut() {
            r.detail = None;
            r.files.clear();
            r.colors.clear();
            r.commit_stats.clear();
            r.status = "loading…".into();
        }
        app.request(Request::OpenPr(pr));
    }
}
```

- [ ] **Step 4: Run the new test**

Run: `cargo test --lib app::tests::refresh_action_in_review_clears_data_and_dispatches_open_pr`
Expected: PASS.

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): refresh in review clears review state before re-opening"
```

---

## Task 8: Remove `PrPackage`, `Cache.packages`, and dead cache writes

**Files:**
- Modify: `src/data/cache.rs` — strip down to `list` only.
- Modify: `src/app.rs` — remove `app.cache.insert_partial / update_diff / add_file_colors` calls and the `LineColors`/`stats` clones they required.

- [ ] **Step 1: Slim `src/data/cache.rs`**

Replace the whole file with:

```rust
//! Passive in-memory cache for the PR list. The PR list comes from
//! `gh pr list` (the only real network call); everything else is derived
//! from local git refs on demand in the worker.

use crate::data::pr::Pr;

#[derive(Default)]
pub struct Cache {
    pub list: Option<Vec<Pr>>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_list(&mut self, prs: Vec<Pr>) {
        self.list = Some(prs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pr::Pr;
    use pretty_assertions::assert_eq;

    fn pr(n: u32) -> Pr {
        // Reuse whatever Pr constructor / fixture the other tests in this
        // crate already use. If a helper exists, prefer it; otherwise hand-
        // construct the minimal Pr.
        Pr {
            number: n,
            title: "t".into(),
            is_draft: false,
            state: crate::data::pr::PrState::Open,
            author: crate::data::pr::Author { login: "u".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: "feat".into(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    #[test]
    fn set_list_replaces_old_value() {
        let mut c = Cache::new();
        c.set_list(vec![pr(1)]);
        c.set_list(vec![pr(2), pr(3)]);
        let list = c.list.as_ref().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].number, 2);
    }
}
```

If the `Pr` constructor literal doesn't match the actual `Pr` struct shape, adjust to match `src/data/pr.rs`. Run `cargo build` to surface any field mismatches.

- [ ] **Step 2: Remove cache write calls from `handle_response` in app.rs**

In `src/app.rs`:

(a) `Response::PrDetail` handler — remove `app.cache.insert_partial(detail.clone());`. The rest stays.

(b) `Response::PrDiff` handler — remove the `head_oid` lookup + `app.cache.update_diff(...)` block. Keep only the part that writes into `st.review` and dispatches `BlameFile`.

(c) `Response::PrFileColors` handler — remove `app.cache.add_file_colors(...)`. Keep only the `st.review` updates.

- [ ] **Step 3: Fix tests that assert against the old cache shape**

Search the test module in `src/app.rs` for `app.cache.get(` and `cache.insert_partial` / `update_diff` / `add_file_colors`. Each call needs replacement:

- `app.cache.get(number).is_some()` → `st.review.as_ref().map(|r| r.detail.is_some()).unwrap_or(false)` (or similar — the test should be asserting whatever review-state condition matches what the old assertion was for).
- `app.cache.get(number).unwrap().files.is_empty()` → `st.review.as_ref().unwrap().files.is_empty()`.
- Setup calls like `cache.insert_partial(detail.clone())` from earlier tasks' tests → drop them; the tests no longer need to prime the cache.

Walk through each app test (`grep -n "cache\." src/app.rs`) and replace each occurrence. If a test exists solely to assert cache state (e.g. `insert_partial_zero_fills_commit_stats_for_every_pr_commit`-style), delete it — that concern lives in `pr_review.rs` now (commit_stats zero-fill happens in the `PrDetail` handler against `st.review`).

- [ ] **Step 4: Run the cache tests**

Run: `cargo test --lib data::cache::tests`
Expected: PASS (one test: `set_list_replaces_old_value`).

- [ ] **Step 5: Run full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Verify no remaining references to `PrPackage` or the removed cache methods**

Run: `grep -rn "PrPackage\|insert_partial\|update_diff\|add_file_colors\|cache\.packages" src/`
Expected: no matches (or only matches inside comments that are documentation about the historical state — review and remove).

- [ ] **Step 7: Commit**

```bash
git add src/data/cache.rs src/app.rs
git commit -m "refactor(cache): remove PrPackage and per-PR cache, list-only"
```

---

## Final verification

- [ ] **Step 1: Full test run**

Run: `cargo test`
Expected: PASS, all tests green.

- [ ] **Step 2: Lint / build check**

Run: `cargo build`
Expected: PASS, no warnings about dead code that should be removed.

- [ ] **Step 3: Manual sanity check (informational)**

The user can run `cargo run` in a repo with multiple open PRs and verify:
- Opening a PR shows the diff quickly, with the first file's colors painting in shortly after.
- Tab-navigating to another file shows the diff immediately; colors paint when blame finishes.
- Pressing `r` in the review pane reloads the PR from scratch (status returns to `loading…`, then `loading diff…`, then `N files`).
- Pressing `r` on the PR list re-runs `gh pr list` + `git fetch`; afterwards opening a PR that had new commits shows the updated diff.

---

## Self-Review

**Spec coverage:**
- "Only `gh pr list` is cached" — Task 8 strips Cache to list-only ✓
- New `Request::OpenPr` and `Request::BlameFile` — Task 5 ✓
- `PrReviewState` owns detail/files/colors/commit_stats — Tasks 1–4 ✓
- `ColorState::{Loading, Ready}` — Task 1 ✓
- Per-file blame dispatched on diff arrival and on navigation, skip if Loading/Ready — Task 6 ✓
- Refresh in review clears state — Task 7 ✓
- `Response::PrColorsDone` removed — Task 5 ✓
- Status-line transitions updated (no `coloring N files…` → `N files` transition) — Task 2 / Task 5: status becomes `coloring N files…` on `PrDiff` and stays that way (no terminal "done" event). The spec called for status to become `N files` on `PrDiff`. Reconciled below.

**Status-line reconciliation:** The spec said the final transition is `"N files"` on `PrDiff`. The plan currently uses `format!("coloring {} files…", r.files.len())` in the PrDiff handler (Task 2) and never updates it after. Update Task 2 step 4 to use `format!("{} files", files.len())` so the status string matches the spec — fix this in the implementation by writing `r.status = format!("{} files", r.files.len());` in the `PrDiff` handler.

**Placeholder scan:** None remaining.

**Type consistency:** `ColorState`, `ensure_blame`, `file_paths`/`file_count` helpers consistent across tasks. `Request::BlameFile` field names (`number`, `head_oid`, `base_oid`, `path`, `commits`) consistent in worker definition (Task 5), `ensure_blame` (Task 6), and tests.
