# Fancy file paths Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the PR list's inline file rows, render the directory prefix in dim (`OVERLAY1`) and the filename in bright (`TEXT`), so the eye lands on the filename.

**Architecture:** Inside `file_line` (`src/view/pr_list.rs`), after the (possibly left-truncated) path is computed, split on the last `/` and emit two spans for the path instead of one. No data-model or worker changes.

**Tech Stack:** Rust, ratatui (TestBackend for assertions on rendered spans).

**Spec:** `docs/superpowers/specs/2026-05-26-fancy-file-paths-design.md`

---

## File map

**Modify:**
- `src/view/pr_list.rs` — change `file_line` to split the path span, add one test.

---

## Task 1: Split path into dim-dir + bright-filename spans

**Files:**
- Modify: `src/view/pr_list.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `src/view/pr_list.rs`:

```rust
    #[test]
    fn file_line_dims_directory_and_brightens_filename() {
        use crate::data::pr::FileMeta;
        let line = file_line(
            &FileMeta { path: "src/foo/bar.rs".into(), additions: 1, deletions: 0 },
            false,
            80,
        );
        // Spans, in order: glyph, dir-prefix (dim), filename (bright),
        // padding, then stats. Verify the dir/filename split is correct.
        let dim_span = line
            .spans
            .iter()
            .find(|s| s.content == "src/foo/")
            .expect("expected a span with text 'src/foo/'");
        assert_eq!(dim_span.style.fg, Some(OVERLAY1), "dir prefix must be OVERLAY1");
        let bright_span = line
            .spans
            .iter()
            .find(|s| s.content == "bar.rs")
            .expect("expected a span with text 'bar.rs'");
        assert_eq!(bright_span.style.fg, Some(TEXT), "filename must be TEXT");
    }

    #[test]
    fn file_line_top_level_file_is_all_bright() {
        use crate::data::pr::FileMeta;
        let line = file_line(
            &FileMeta { path: "Cargo.toml".into(), additions: 1, deletions: 0 },
            false,
            80,
        );
        // No '/' in path → no dim span; whole path renders in TEXT.
        let bright_span = line
            .spans
            .iter()
            .find(|s| s.content == "Cargo.toml")
            .expect("expected a span with text 'Cargo.toml'");
        assert_eq!(bright_span.style.fg, Some(TEXT), "filename must be TEXT");
        // No span should have the path's content with OVERLAY1.
        assert!(
            !line.spans.iter().any(|s| s.style.fg == Some(OVERLAY1) && s.content.contains("Cargo.toml")),
            "top-level file should not have a dim path span"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

`cargo test -p prpr --lib file_line_dims 2>&1 | tail -10`
Expected: FAIL — `file_line` currently emits a single span for the whole path with `TEXT` color, so the `find(|s| s.content == "src/foo/")` lookup returns `None` and the `expect` panics.

- [ ] **Step 3: Modify `file_line` to split the path**

In `src/view/pr_list.rs`, find the `file_line` function (around line 345) and replace the `spans` construction. The current code is:

```rust
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(format!("  {glyph} "), Style::default().fg(SURFACE2)),
        Span::styled(path, Style::default().fg(TEXT)),
        Span::styled(" ".repeat(pad_cols), Style::default()),
    ];
```

Replace the entire `vec![...]` initializer plus the path-only span with:

```rust
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        format!("  {glyph} "),
        Style::default().fg(SURFACE2),
    )];
    // Split the (possibly truncated) path at the last '/' so the
    // directory prefix renders dim and the filename pops.
    match path.rfind('/') {
        Some(i) => {
            let (dir, name) = path.split_at(i + 1);
            spans.push(Span::styled(dir.to_string(), Style::default().fg(OVERLAY1)));
            spans.push(Span::styled(name.to_string(), Style::default().fg(TEXT)));
        }
        None => {
            spans.push(Span::styled(path.clone(), Style::default().fg(TEXT)));
        }
    }
    spans.push(Span::styled(" ".repeat(pad_cols), Style::default()));
```

Note: the variable `path` (a `String` previously consumed by `Span::styled(path, ...)`) is now referenced via `path.rfind`, `path.split_at`, and `path.clone()`. Make sure the variable is still in scope (it's a local `let path = ...` earlier in the function).

If clippy complains about the `path.clone()` in the `None` branch (a common nag for `String`), keep the clone — splitting `path` ownership cleanly through both branches is less code than alternatives.

- [ ] **Step 4: Run tests to verify pass**

`cargo test -p prpr --lib file_line_ 2>&1 | tail -10`
Expected: both new tests pass. Run the rest of the renderer tests too — the `expanded_ready_renders_file_paths_under_selected_row` test asserts on `contains("src/foo.rs")` against the joined buffer text. Since the path content is unchanged (just split across two spans that print adjacent in the buffer), the buffer text is identical and that test still passes.

`cargo test -p prpr 2>&1 | grep "^test result" | head`
Expected: 177 total pass (175 prior + 2 new).

- [ ] **Step 5: Lint**

`cargo clippy -p prpr --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(pr_list): dim directory prefix, brighten filename in file list"
```

---

## Task 2: Integration smoke + ship

**Files:** none (validation only)

- [ ] **Step 1: Full test suite**

`cargo test -p prpr 2>&1 | grep "^test result" | head`
Expected: 177 pass.

- [ ] **Step 2: Lint**

`cargo clippy -p prpr --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 3: Release build**

`cargo build --release 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Hand back to controller**

The controller will run the merge + push + reinstall sequence (same pattern as the prior branch). No commit in this task.

---

## Self-review checklist

- ✅ Spec coverage:
  - Path splits at the last `/`, dir → `OVERLAY1`, filename → `TEXT` → Task 1 Step 3
  - Top-level file (no `/`) renders entirely in `TEXT` → Task 1 Step 3 (`None` arm) + Task 1 Step 1 second test
  - Truncation strategy preserved (split happens AFTER truncation, on the possibly-`…`-prefixed string) → Task 1 Step 3
  - One renderer test asserting the dim/bright split → Task 1 Step 1
- ✅ No placeholders (no TBD / TODO / "similar to" / etc.)
- ✅ Type consistency: `Span`, `Style`, `OVERLAY1`, `TEXT`, `SURFACE2` are existing imports used unchanged.
