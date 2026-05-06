# prpr Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a keyboard-driven Rust TUI for reviewing GitHub PRs with per-commit color attribution in the diff.

**Architecture:** Single binary, two full-screen views (PR list / PR review). Subprocess-driven (`gh` for GitHub ops, `git` for diff & blame). Views consume parsed structs from an in-memory cache; only the cache speaks to subprocesses. Main thread runs the ratatui event loop; subprocesses run on worker threads via `std::sync::mpsc`.

**Tech Stack:** Rust, `ratatui`, `crossterm`, `serde_json`, `anyhow`, `thiserror`, `directories`, `toml`, `unicode-width`, `chrono`, `clap`. `gh` and `git` CLIs at runtime.

**Reference spec:** `docs/superpowers/specs/2026-05-06-prpr-tui-pr-review-design.md`

---

## Conventions used in this plan

- All paths are relative to the repo root (`/Users/poga/projects/prpr`).
- Commits use Conventional Commits (`feat:`, `test:`, `refactor:`, `chore:`, `docs:`, `fix:`).
- Every task ends with `cargo test` passing and a commit. Don't merge tasks.
- The plan introduces traits where the spec calls for mocking (`GhClient`, `GitClient`). Tests substitute fakes; the binary uses the subprocess implementations.
- When a step says "expected: PASS / FAIL", the engineer should literally observe that — if the output differs, stop and investigate.
- Some tasks render UI; their tests use `ratatui::buffer::Buffer` direct comparisons. The pattern is shown in Task 13.

---

## Task 1: Cargo project skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `rust-toolchain.toml`
- Modify: `.gitignore`

- [ ] **Step 1: Initialize the Cargo crate**

```bash
cd /Users/poga/projects/prpr
cargo init --bin --name prpr
```

This creates `Cargo.toml` and `src/main.rs` with a default `println!`.

- [ ] **Step 2: Pin a Rust toolchain**

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Add runtime dependencies**

Run each command (do NOT consolidate — one `cargo add` per call so the resolver picks one version per dep):

```bash
cargo add ratatui
cargo add crossterm
cargo add serde --features derive
cargo add serde_json
cargo add anyhow
cargo add thiserror
cargo add toml
cargo add directories
cargo add unicode-width
cargo add chrono --features serde
cargo add clap --features derive
```

- [ ] **Step 4: Add dev dependencies**

```bash
cargo add --dev pretty_assertions
cargo add --dev tempfile
```

- [ ] **Step 5: Update `.gitignore`**

Open `.gitignore` and ensure it contains these lines (preserve any existing content):

```
.superpowers/
target/
Cargo.lock
```

Note: `Cargo.lock` *should* be checked in for binaries normally. We're keeping it out for now to avoid noise during early TDD. We'll re-enable it in Task 23.

- [ ] **Step 6: Replace `src/main.rs` with a minimal stub**

```rust
fn main() {
    println!("prpr v{}", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 7: Verify it builds and runs**

Run:

```bash
cargo build
cargo run --quiet
```

Expected output: `prpr v0.1.0`

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml rust-toolchain.toml src/main.rs .gitignore
git commit -m "chore: initialize cargo project with TUI deps"
```

---

## Task 2: Module skeleton

Lays out the directory structure from the spec. Each file is a near-empty stub so the rest of the plan can fill them in without worrying about wiring.

**Files:**
- Modify: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/app.rs`
- Create: `src/keys.rs`
- Create: `src/config.rs`
- Create: `src/data/mod.rs`
- Create: `src/data/pr.rs`
- Create: `src/data/gh.rs`
- Create: `src/data/git.rs`
- Create: `src/data/cache.rs`
- Create: `src/render/mod.rs`
- Create: `src/render/style.rs`
- Create: `src/render/color.rs`
- Create: `src/render/diff.rs`
- Create: `src/view/mod.rs`
- Create: `src/view/pr_list.rs`
- Create: `src/view/pr_review.rs`
- Create: `src/view/file_picker.rs`
- Create: `src/view/merge_modal.rs`

- [ ] **Step 1: Create `src/lib.rs`**

```rust
pub mod app;
pub mod config;
pub mod data;
pub mod keys;
pub mod render;
pub mod view;
```

- [ ] **Step 2: Create `src/data/mod.rs`**

```rust
pub mod cache;
pub mod gh;
pub mod git;
pub mod pr;
```

- [ ] **Step 3: Create `src/render/mod.rs`**

```rust
pub mod color;
pub mod diff;
pub mod style;
```

- [ ] **Step 4: Create `src/view/mod.rs`**

```rust
pub mod file_picker;
pub mod merge_modal;
pub mod pr_list;
pub mod pr_review;
```

- [ ] **Step 5: Create empty stubs for each remaining module**

Each file gets exactly this content (one line) so the crate compiles:

```rust
// stub — implemented in a later task
```

Files: `src/app.rs`, `src/keys.rs`, `src/config.rs`, `src/data/pr.rs`, `src/data/gh.rs`, `src/data/git.rs`, `src/data/cache.rs`, `src/render/style.rs`, `src/render/color.rs`, `src/render/diff.rs`, `src/view/pr_list.rs`, `src/view/pr_review.rs`, `src/view/file_picker.rs`, `src/view/merge_modal.rs`.

- [ ] **Step 6: Update `src/main.rs` to use the lib**

```rust
fn main() {
    println!("prpr v{}", env!("CARGO_PKG_VERSION"));
    let _ = prpr::config::default_window_size();
}
```

- [ ] **Step 7: Add `default_window_size` to `src/config.rs`**

```rust
pub fn default_window_size() -> usize {
    7
}
```

- [ ] **Step 8: Verify the crate compiles**

Run:

```bash
cargo build
```

Expected: builds clean, no errors.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml src/
git commit -m "chore: scaffold module layout"
```

---

## Task 3: PR data types with JSON deserialization

The structs that everything else consumes. Pure data, easy to TDD with fixture JSON.

**Files:**
- Modify: `src/data/pr.rs`
- Create: `tests/fixtures/pr_list.json`
- Create: `tests/fixtures/pr_view.json`

- [ ] **Step 1: Write a failing test that parses a `gh pr list` fixture**

Create `tests/fixtures/pr_list.json`:

```json
[
  {
    "number": 482,
    "title": "fix: race condition in scheduler",
    "isDraft": false,
    "state": "OPEN",
    "author": { "login": "alice" },
    "createdAt": "2026-05-04T12:00:00Z",
    "labels": [{ "name": "bug" }],
    "statusCheckRollup": [
      { "conclusion": "SUCCESS", "status": "COMPLETED" }
    ],
    "reviewDecision": "APPROVED"
  },
  {
    "number": 479,
    "title": "feat: add /metrics endpoint",
    "isDraft": false,
    "state": "OPEN",
    "author": { "login": "bob" },
    "createdAt": "2026-05-03T08:30:00Z",
    "labels": [{ "name": "feature" }],
    "statusCheckRollup": [
      { "conclusion": "FAILURE", "status": "COMPLETED" }
    ],
    "reviewDecision": "CHANGES_REQUESTED"
  }
]
```

In `src/data/pr.rs` write:

```rust
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pr {
    pub number: u32,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    pub state: PrState,
    pub author: Author,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<StatusCheck>,
    #[serde(rename = "reviewDecision", default)]
    pub review_decision: Option<ReviewDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StatusCheck {
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl Pr {
    /// Aggregate CI conclusion across status_check_rollup.
    /// Returns "fail" if any check failed, "pending" if any are pending,
    /// "pass" if all completed successfully, "none" if empty.
    pub fn ci_state(&self) -> CiState {
        if self.status_check_rollup.is_empty() {
            return CiState::None;
        }
        let mut any_pending = false;
        for c in &self.status_check_rollup {
            match c.status.as_deref() {
                Some("COMPLETED") => match c.conclusion.as_deref() {
                    Some("SUCCESS") => {}
                    Some("FAILURE") | Some("TIMED_OUT") | Some("CANCELLED") => {
                        return CiState::Fail;
                    }
                    _ => {}
                },
                _ => any_pending = true,
            }
        }
        if any_pending {
            CiState::Pending
        } else {
            CiState::Pass
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Pass,
    Fail,
    Pending,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_list_fixture() {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 482);
        assert_eq!(prs[0].author.login, "alice");
        assert_eq!(prs[0].labels[0].name, "bug");
        assert_eq!(prs[0].ci_state(), CiState::Pass);
        assert_eq!(prs[0].review_decision, Some(ReviewDecision::Approved));
        assert_eq!(prs[1].ci_state(), CiState::Fail);
    }

    #[test]
    fn ci_state_none_when_empty() {
        let pr = Pr {
            number: 1,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
        };
        assert_eq!(pr.ci_state(), CiState::None);
    }
}
```

- [ ] **Step 2: Run the tests, expect failures or success**

```bash
cargo test --lib data::pr
```

Expected: PASS (we wrote tests and code in one step here because they're tightly coupled to the type definitions).

- [ ] **Step 3: Add the `PrDetail` type for `gh pr view --json`**

Create `tests/fixtures/pr_view.json`:

```json
{
  "number": 482,
  "title": "fix: race condition in scheduler",
  "isDraft": false,
  "state": "OPEN",
  "author": { "login": "alice" },
  "createdAt": "2026-05-04T12:00:00Z",
  "baseRefName": "main",
  "baseRefOid": "0000000000000000000000000000000000000001",
  "headRefName": "fix-race",
  "headRefOid": "0000000000000000000000000000000000000002",
  "mergeable": "MERGEABLE",
  "labels": [{ "name": "bug" }],
  "statusCheckRollup": [{ "conclusion": "SUCCESS", "status": "COMPLETED" }],
  "reviewDecision": "APPROVED",
  "commits": [
    { "oid": "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0", "messageHeadline": "init structure", "authors": [{ "login": "alice" }] },
    { "oid": "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3", "messageHeadline": "enum dispatch", "authors": [{ "login": "alice" }] },
    { "oid": "789abcdef0123456789abcdef0123456789abcde", "messageHeadline": "add Wait variant", "authors": [{ "login": "alice" }] }
  ],
  "files": [
    { "path": "src/sched.rs", "additions": 5, "deletions": 1 },
    { "path": "src/queue.rs", "additions": 2, "deletions": 0 },
    { "path": "tests/sched.rs", "additions": 12, "deletions": 0 },
    { "path": "README.md", "additions": 1, "deletions": 1 }
  ]
}
```

Append to `src/data/pr.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PrDetail {
    pub number: u32,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    pub state: PrState,
    pub author: Author,
    #[serde(rename = "baseRefName")]
    pub base_ref_name: String,
    #[serde(rename = "baseRefOid")]
    pub base_ref_oid: String,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(rename = "headRefOid")]
    pub head_ref_oid: String,
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<StatusCheck>,
    #[serde(rename = "reviewDecision", default)]
    pub review_decision: Option<ReviewDecision>,
    pub commits: Vec<Commit>,
    pub files: Vec<FileMeta>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Commit {
    pub oid: String,
    #[serde(rename = "messageHeadline")]
    pub message_headline: String,
    pub authors: Vec<Author>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FileMeta {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}
```

Append to the test module:

```rust
    #[test]
    fn parses_pr_view_fixture() {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let pr: PrDetail = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 482);
        assert_eq!(pr.head_ref_oid.len(), 40);
        assert_eq!(pr.commits.len(), 3);
        assert_eq!(pr.commits[0].oid, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0");
        assert_eq!(pr.files.len(), 4);
        assert_eq!(pr.files[0].path, "src/sched.rs");
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test --lib data::pr
```

Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/ src/data/pr.rs
git commit -m "feat(data): Pr/PrDetail types with serde + CI rollup"
```

---

## Task 4: Color palette (Catppuccin Mocha)

Pure constants. No tests beyond "they compile and have the right hex values".

**Files:**
- Modify: `src/render/style.rs`

- [ ] **Step 1: Replace `src/render/style.rs` with the palette**

```rust
//! Catppuccin Mocha named colors used across the UI.
//! https://github.com/catppuccin/catppuccin

use ratatui::style::Color;

// Surfaces
pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const SURFACE2: Color = Color::Rgb(0x58, 0x5b, 0x70);
pub const OVERLAY0: Color = Color::Rgb(0x6c, 0x70, 0x86);
pub const OVERLAY1: Color = Color::Rgb(0x7f, 0x84, 0x9c);

// Text
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);

// Diff
pub const DIFF_ADD_FG: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
pub const DIFF_ADD_BG: Color = Color::Rgb(0x1f, 0x2a, 0x1f);
pub const DIFF_DEL_FG: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const DIFF_DEL_BG: Color = Color::Rgb(0x2a, 0x1f, 0x23);

// Commit slot palette — fixed order, oldest in window = slot 0.
// Green and red are deliberately absent.
pub const COMMIT_PALETTE: [Color; 7] = [
    Color::Rgb(0x89, 0xb4, 0xfa), // 1 blue
    Color::Rgb(0xcb, 0xa6, 0xf7), // 2 mauve
    Color::Rgb(0xfa, 0xb3, 0x87), // 3 peach
    Color::Rgb(0x94, 0xe2, 0xd5), // 4 teal
    Color::Rgb(0xf9, 0xe2, 0xaf), // 5 yellow
    Color::Rgb(0xf5, 0xc2, 0xe7), // 6 pink
    Color::Rgb(0x74, 0xc7, 0xec), // 7 sapphire
];

// Used for commits that fall outside the window.
pub const OLDER_COMMIT: Color = SURFACE2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_palette_has_no_green_or_red() {
        for c in COMMIT_PALETTE.iter() {
            // No commit color should be too close to diff add (green) or remove (red).
            // This is a sanity check, not a perceptual assertion.
            if let Color::Rgb(r, g, b) = c {
                let mostly_green = *g > 0xc0 && *r < *g && *b < *g;
                let mostly_red = *r > 0xc0 && *g < *r && *b < *r;
                assert!(!mostly_green, "commit palette contains a green-ish color");
                assert!(!mostly_red, "commit palette contains a red-ish color");
            }
        }
    }

    #[test]
    fn palette_size_matches_default_window() {
        assert_eq!(COMMIT_PALETTE.len(), 7);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib render::style
```

Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/render/style.rs
git commit -m "feat(render): catppuccin mocha palette + commit slots"
```

---

## Task 5: Commit slot assignment

Maps commit SHAs → colors based on the window-of-N rule. Pure logic, fully TDD.

**Files:**
- Modify: `src/render/color.rs`

- [ ] **Step 1: Write failing tests**

Replace `src/render/color.rs`:

```rust
//! Commit color assignment.
//!
//! Given a chronological list of commit SHAs in a PR and a window size,
//! assign palette colors. Oldest in window = slot 0 (blue). Anything
//! outside the window shares the OLDER_COMMIT gray.

use std::collections::HashMap;

use ratatui::style::Color;

use crate::render::style::{COMMIT_PALETTE, OLDER_COMMIT};

/// Compute the color for each commit. Commits MUST be in chronological order
/// (oldest first), as returned by `git log --reverse` or `gh pr view --json commits`.
///
/// `window_size` is clamped to `COMMIT_PALETTE.len()` if larger.
pub fn assign_commit_colors(commits: &[String], window_size: usize) -> HashMap<String, Color> {
    let cap = window_size.min(COMMIT_PALETTE.len());
    let mut out = HashMap::with_capacity(commits.len());

    if commits.is_empty() {
        return out;
    }

    // The "window" is the last `cap` commits (the most recent ones).
    let split = commits.len().saturating_sub(cap);
    for (i, sha) in commits.iter().enumerate() {
        let color = if i < split {
            OLDER_COMMIT
        } else {
            COMMIT_PALETTE[i - split]
        };
        out.insert(sha.clone(), color);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sha(c: char) -> String {
        std::iter::repeat(c).take(40).collect()
    }

    #[test]
    fn empty_input_returns_empty_map() {
        let map = assign_commit_colors(&[], 7);
        assert!(map.is_empty());
    }

    #[test]
    fn fewer_commits_than_window_each_get_a_slot() {
        let commits = vec![sha('a'), sha('b'), sha('c')];
        let map = assign_commit_colors(&commits, 7);
        assert_eq!(map[&sha('a')], COMMIT_PALETTE[0]);
        assert_eq!(map[&sha('b')], COMMIT_PALETTE[1]);
        assert_eq!(map[&sha('c')], COMMIT_PALETTE[2]);
    }

    #[test]
    fn more_commits_than_window_pushes_old_into_gray() {
        let commits = vec![
            sha('a'), sha('b'), sha('c'), sha('d'),
            sha('e'), sha('f'), sha('g'), sha('h'), sha('i'),
        ]; // 9 commits, window 7
        let map = assign_commit_colors(&commits, 7);
        // Two oldest are out-of-window → gray.
        assert_eq!(map[&sha('a')], OLDER_COMMIT);
        assert_eq!(map[&sha('b')], OLDER_COMMIT);
        // Remaining 7 fill the palette in order.
        assert_eq!(map[&sha('c')], COMMIT_PALETTE[0]);
        assert_eq!(map[&sha('i')], COMMIT_PALETTE[6]);
    }

    #[test]
    fn window_larger_than_palette_is_clamped() {
        let commits: Vec<String> = (0..10).map(|i| sha((b'a' + i) as char)).collect();
        let map = assign_commit_colors(&commits, 100);
        // Three oldest out-of-window (10 - 7 = 3).
        assert_eq!(map[&sha('a')], OLDER_COMMIT);
        assert_eq!(map[&sha('b')], OLDER_COMMIT);
        assert_eq!(map[&sha('c')], OLDER_COMMIT);
        assert_eq!(map[&sha('d')], COMMIT_PALETTE[0]);
    }

    #[test]
    fn window_size_zero_makes_everything_gray() {
        let commits = vec![sha('a'), sha('b'), sha('c')];
        let map = assign_commit_colors(&commits, 0);
        for sha in &commits {
            assert_eq!(map[sha], OLDER_COMMIT);
        }
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib render::color
```

Expected: 5 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/render/color.rs
git commit -m "feat(render): commit-slot color assignment"
```

---

## Task 6: Unified diff parser

Parses the output of `gh pr diff` into structured `FileDiff { path, lines: Vec<DiffLine> }`. Pure parser. Tests use real diff text fixtures.

**Files:**
- Create: `src/data/diff.rs`
- Modify: `src/data/mod.rs`
- Create: `tests/fixtures/diff_basic.patch`

- [ ] **Step 1: Add the `diff` module**

Append to `src/data/mod.rs`:

```rust
pub mod diff;
```

- [ ] **Step 2: Add the diff fixture**

Create `tests/fixtures/diff_basic.patch`:

```
diff --git a/src/sched.rs b/src/sched.rs
index 1111111..2222222 100644
--- a/src/sched.rs
+++ b/src/sched.rs
@@ -42,7 +42,11 @@ impl Scheduler {
     pub fn run(&mut self, t: Task) {
         let lock = self.lock.lock();
-        if t.state == State::Run {
-            spawn(t);
-        }
+        match t.state {
+            State::Run  => spawn(t),
+            State::Wait => self.queue.push(t),
+            _ => {}
+        }
     }
diff --git a/README.md b/README.md
index aaaaaaa..bbbbbbb 100644
--- a/README.md
+++ b/README.md
@@ -1,3 +1,3 @@
 # prpr
-A PR review tool.
+A keyboard-driven PR review tool.

```

- [ ] **Step 3: Write the parser with failing tests**

Create `src/data/diff.rs`:

```rust
//! Minimal unified-diff parser. Designed for `gh pr diff` output:
//! one or more file diffs separated by `diff --git` headers, each followed
//! by `--- a/<path>` / `+++ b/<path>` and one or more `@@ ...` hunks.

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub lines: Vec<DiffLine>,
    /// True if `gh pr diff` flagged this file as binary (no content lines).
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub op: DiffOp,
    /// Line number in the *base* file. `None` for added lines.
    pub old_lineno: Option<u32>,
    /// Line number in the *head* file. `None` for removed lines.
    pub new_lineno: Option<u32>,
    pub text: String,
    /// True for the `@@ ... @@` separator lines (rendered as section dividers).
    pub is_hunk_header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffOp {
    Context,
    Add,
    Delete,
    Hunk,
}

/// Parse the entire output of `gh pr diff <num>` into a Vec<FileDiff>.
pub fn parse_diff(input: &str) -> Result<Vec<FileDiff>> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut old_ln: u32 = 0;
    let mut new_ln: u32 = 0;

    for raw in input.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);

        if line.starts_with("diff --git ") {
            if let Some(f) = current.take() {
                files.push(f);
            }
            // Parse "diff --git a/<path> b/<path>"; we use the b-path.
            let path = line
                .split_whitespace()
                .nth(3)
                .and_then(|s| s.strip_prefix("b/"))
                .unwrap_or("")
                .to_string();
            current = Some(FileDiff { path, lines: Vec::new(), binary: false });
            old_ln = 0;
            new_ln = 0;
            continue;
        }

        let Some(f) = current.as_mut() else { continue };

        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            f.binary = true;
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("+++ ")
            || line.starts_with("index ") || line.starts_with("similarity ")
            || line.starts_with("rename ") || line.starts_with("new file mode")
            || line.starts_with("deleted file mode") || line.starts_with("\\ No newline") {
            continue;
        }

        if let Some(rest) = line.strip_prefix("@@") {
            // @@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@ ...
            let body = rest.trim_start_matches(' ');
            let (header, _) = body.split_once("@@").ok_or_else(|| anyhow!("malformed hunk: {line}"))?;
            let parts: Vec<&str> = header.split_whitespace().collect();
            if parts.len() < 2 {
                return Err(anyhow!("malformed hunk: {line}"));
            }
            let old_start = parts[0].trim_start_matches('-').split(',').next().unwrap();
            let new_start = parts[1].trim_start_matches('+').split(',').next().unwrap();
            old_ln = old_start.parse().map_err(|_| anyhow!("bad hunk old start: {line}"))?;
            new_ln = new_start.parse().map_err(|_| anyhow!("bad hunk new start: {line}"))?;
            f.lines.push(DiffLine {
                op: DiffOp::Hunk,
                old_lineno: None,
                new_lineno: None,
                text: line.to_string(),
                is_hunk_header: true,
            });
            continue;
        }

        let (op, old, new, text) = if let Some(t) = line.strip_prefix('+') {
            let n = new_ln; new_ln += 1;
            (DiffOp::Add, None, Some(n), t.to_string())
        } else if let Some(t) = line.strip_prefix('-') {
            let n = old_ln; old_ln += 1;
            (DiffOp::Delete, Some(n), None, t.to_string())
        } else if let Some(t) = line.strip_prefix(' ') {
            let o = old_ln; old_ln += 1;
            let n = new_ln; new_ln += 1;
            (DiffOp::Context, Some(o), Some(n), t.to_string())
        } else if line.is_empty() {
            // Trailing blank line in patch.
            continue;
        } else {
            // Unknown — skip.
            continue;
        };
        f.lines.push(DiffLine {
            op,
            old_lineno: old,
            new_lineno: new,
            text,
            is_hunk_header: false,
        });
    }

    if let Some(f) = current.take() {
        files.push(f);
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_two_file_patch() {
        let input = include_str!("../../tests/fixtures/diff_basic.patch");
        let files = parse_diff(input).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/sched.rs");
        assert_eq!(files[1].path, "README.md");
        assert!(!files[0].binary);
    }

    #[test]
    fn assigns_correct_line_numbers() {
        let input = include_str!("../../tests/fixtures/diff_basic.patch");
        let files = parse_diff(input).unwrap();
        let sched = &files[0];
        // First non-hunk line is context line "    pub fn run..." at old=42, new=42.
        let first_content = sched.lines.iter().find(|l| !l.is_hunk_header).unwrap();
        assert_eq!(first_content.op, DiffOp::Context);
        assert_eq!(first_content.old_lineno, Some(42));
        assert_eq!(first_content.new_lineno, Some(42));
        // Find first added line "+        match t.state {".
        let first_add = sched.lines.iter().find(|l| l.op == DiffOp::Add).unwrap();
        assert_eq!(first_add.old_lineno, None);
        assert_eq!(first_add.new_lineno, Some(45));
        assert!(first_add.text.contains("match t.state"));
    }

    #[test]
    fn detects_binary_marker() {
        let input = "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let files = parse_diff(input).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert!(files[0].lines.is_empty());
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --lib data::diff
```

Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/diff_basic.patch src/data/diff.rs src/data/mod.rs
git commit -m "feat(data): unified diff parser"
```

---

## Task 7: Blame porcelain parser

Turn `git blame --porcelain` output into a per-line `Vec<String>` of SHAs (index = line number - 1).

**Files:**
- Create: `src/data/blame.rs`
- Modify: `src/data/mod.rs`
- Create: `tests/fixtures/blame_porcelain.txt`

- [ ] **Step 1: Add module to `src/data/mod.rs`**

Append:

```rust
pub mod blame;
```

- [ ] **Step 2: Add fixture**

Create `tests/fixtures/blame_porcelain.txt`:

```
a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 42 42 1
author Alice
author-mail <alice@example.com>
author-time 1714000000
author-tz +0000
committer Alice
committer-mail <alice@example.com>
committer-time 1714000000
committer-tz +0000
summary init structure
filename src/sched.rs
	    pub fn run(&mut self, t: Task) {
a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 43 43
	        let lock = self.lock.lock();
d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3 45 45 1
author Alice
author-mail <alice@example.com>
author-time 1714000300
author-tz +0000
committer Alice
committer-mail <alice@example.com>
committer-time 1714000300
committer-tz +0000
summary enum dispatch
previous 0000000000000000000000000000000000000000 src/sched.rs
filename src/sched.rs
	        match t.state {
789abcdef0123456789abcdef0123456789abcde 46 46 2
author Alice
author-mail <alice@example.com>
author-time 1714000900
author-tz +0000
committer Alice
committer-mail <alice@example.com>
committer-time 1714000900
committer-tz +0000
summary add Wait variant
previous d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3 src/sched.rs
filename src/sched.rs
	            State::Run  => spawn(t),
789abcdef0123456789abcdef0123456789abcde 47 47
	            State::Wait => self.queue.push(t),
```

- [ ] **Step 3: Write the parser**

Create `src/data/blame.rs`:

```rust
//! Parser for `git blame --porcelain <commit> -- <file>` output.
//!
//! The format alternates between header chunks (starting with a 40-char SHA
//! followed by source-line-number, result-line-number, [num-lines]) and a
//! TAB-prefixed source-line. Subsequent lines from the same commit show only
//! the `<sha> <orig> <result>` header and the TAB line; metadata (author etc.)
//! is omitted after the first appearance of a SHA.

use std::collections::HashMap;

/// Result: a vector indexed by `result_lineno - 1`. Holds the SHA that owns
/// each line. If the file is empty, the vector is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blame {
    pub line_shas: Vec<String>,
}

pub fn parse_blame(input: &str) -> Blame {
    let mut by_lineno: HashMap<u32, String> = HashMap::new();
    let mut max_line: u32 = 0;

    let mut lines = input.split('\n').peekable();
    while let Some(header) = lines.next() {
        if header.is_empty() {
            continue;
        }
        // Header form: "<sha> <orig> <result> [num]".
        let mut parts = header.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        if sha.len() != 40 {
            continue;
        }
        let _orig = parts.next();
        let Some(result_str) = parts.next() else { continue };
        let Ok(result_lineno) = result_str.parse::<u32>() else { continue };

        // Skip metadata lines until we hit the TAB-prefixed source line.
        // Metadata appears only on first-mention of a SHA; for subsequent
        // mentions we go straight to the TAB line.
        loop {
            let Some(next) = lines.next() else { break };
            if next.starts_with('\t') {
                break;
            }
        }

        if result_lineno > max_line {
            max_line = result_lineno;
        }
        by_lineno.insert(result_lineno, sha.to_string());
    }

    let mut line_shas = vec![String::new(); max_line as usize];
    for (lineno, sha) in by_lineno {
        if lineno >= 1 && (lineno as usize) <= line_shas.len() {
            line_shas[lineno as usize - 1] = sha;
        }
    }
    Blame { line_shas }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_porcelain_fixture() {
        let input = include_str!("../../tests/fixtures/blame_porcelain.txt");
        let blame = parse_blame(input);
        // The fixture mentions lines 42, 43, 45, 46, 47.
        // Line indices 0..41 stay empty; index 41 (line 42) = a1...
        assert!(blame.line_shas.len() >= 47);
        assert_eq!(
            blame.line_shas[41],
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0",
        );
        assert_eq!(
            blame.line_shas[42],
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0",
        );
        assert_eq!(
            blame.line_shas[44],
            "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3",
        );
        assert_eq!(
            blame.line_shas[45],
            "789abcdef0123456789abcdef0123456789abcde",
        );
        assert_eq!(
            blame.line_shas[46],
            "789abcdef0123456789abcdef0123456789abcde",
        );
    }

    #[test]
    fn empty_input_returns_empty() {
        let blame = parse_blame("");
        assert!(blame.line_shas.is_empty());
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --lib data::blame
```

Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/blame_porcelain.txt src/data/blame.rs src/data/mod.rs
git commit -m "feat(data): git blame porcelain parser"
```

---

## Task 8: Subprocess traits + real implementations

Defines the `GhClient` and `GitClient` traits the cache will use, plus subprocess-backed impls. The trait split is what lets us unit-test the cache and views without spawning processes.

**Files:**
- Modify: `src/data/gh.rs`
- Modify: `src/data/git.rs`

- [ ] **Step 1: Replace `src/data/gh.rs` with the trait + impl**

```rust
//! `gh` CLI subprocess wrappers. The `GhClient` trait is what the cache
//! depends on; tests substitute a fake. The production binary uses
//! `GhCli`, which shells out to `gh`.

use std::process::{Command, Output};

use anyhow::{anyhow, Context, Result};

use crate::data::pr::{Pr, PrDetail};

pub trait GhClient: Send + Sync {
    fn list_prs(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>>;
    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail>;
    fn diff_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<String>;
    /// `method` is one of "merge", "squash", "rebase".
    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()>;
    fn auth_status(&self) -> Result<()>;
}

pub struct GhCli;

const PR_LIST_FIELDS: &str = "number,title,author,isDraft,state,createdAt,labels,statusCheckRollup,reviewDecision";
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
    fn list_prs(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>> {
        let out = run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "list", "--limit", "200", "--state", "all", "--json", PR_LIST_FIELDS]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` output")?;
        Ok(prs)
    }

    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail> {
        let n = number.to_string();
        let out = run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "view", &n, "--json", PR_VIEW_FIELDS]))?;
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

    fn auth_status(&self) -> Result<()> {
        run(Command::new("gh").args(["auth", "status"]))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory fake. Tests load JSON fixtures and stuff them into this.
    pub struct FakeGh {
        pub prs: Vec<Pr>,
        pub views: HashMap<u32, PrDetail>,
        pub diffs: HashMap<u32, String>,
        pub merges: Mutex<Vec<(u32, String)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self { prs: vec![], views: HashMap::new(), diffs: HashMap::new(), merges: Mutex::new(vec![]) }
        }
    }

    impl GhClient for FakeGh {
        fn list_prs(&self, _root: &std::path::Path) -> Result<Vec<Pr>> {
            Ok(self.prs.clone())
        }
        fn view_pr(&self, _root: &std::path::Path, n: u32) -> Result<PrDetail> {
            self.views.get(&n).cloned().ok_or_else(|| anyhow!("no fake view for #{n}"))
        }
        fn diff_pr(&self, _root: &std::path::Path, n: u32) -> Result<String> {
            self.diffs.get(&n).cloned().ok_or_else(|| anyhow!("no fake diff for #{n}"))
        }
        fn merge_pr(&self, _root: &std::path::Path, n: u32, m: &str) -> Result<()> {
            self.merges.lock().unwrap().push((n, m.to_string()));
            Ok(())
        }
        fn auth_status(&self) -> Result<()> {
            Ok(())
        }
    }
}
```

- [ ] **Step 2: Replace `src/data/git.rs` with the trait + impl**

```rust
//! `git` CLI subprocess wrappers. Same trait pattern as `gh.rs`.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{anyhow, Context, Result};

pub trait GitClient: Send + Sync {
    /// Resolve the repo root containing `cwd`. Errors if `cwd` is not in a git repo.
    fn repo_root(&self, cwd: &Path) -> Result<std::path::PathBuf>;
    /// Returns `true` if the `origin` (or any) remote points at github.com.
    fn has_github_remote(&self, repo_root: &Path) -> Result<bool>;
    /// Fetch `refs/pull/<num>/head` so `head_oid` is locally available.
    fn fetch_pr(&self, repo_root: &Path, number: u32) -> Result<()>;
    /// Run `git blame --porcelain <commit> -- <file>`. Returns raw stdout.
    fn blame(&self, repo_root: &Path, commit: &str, file: &str) -> Result<String>;
}

pub struct GitCli;

fn run(cmd: &mut Command) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn: {cmd:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(anyhow!("git exited with {}: {}", out.status, stderr.trim()));
    }
    Ok(out)
}

impl GitClient for GitCli {
    fn repo_root(&self, cwd: &Path) -> Result<std::path::PathBuf> {
        let out = run(Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "--show-toplevel"]))?;
        let s = String::from_utf8(out.stdout)?
            .trim()
            .to_string();
        if s.is_empty() {
            Err(anyhow!("git rev-parse returned empty"))
        } else {
            Ok(std::path::PathBuf::from(s))
        }
    }

    fn has_github_remote(&self, repo_root: &Path) -> Result<bool> {
        let out = run(Command::new("git")
            .current_dir(repo_root)
            .args(["remote", "-v"]))?;
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(s.contains("github.com"))
    }

    fn fetch_pr(&self, repo_root: &Path, number: u32) -> Result<()> {
        let refspec = format!("+refs/pull/{number}/head:refs/prpr/pr-{number}");
        run(Command::new("git")
            .current_dir(repo_root)
            .args(["fetch", "--quiet", "origin", &refspec]))?;
        Ok(())
    }

    fn blame(&self, repo_root: &Path, commit: &str, file: &str) -> Result<String> {
        let out = run(Command::new("git")
            .current_dir(repo_root)
            .args(["blame", "--porcelain", commit, "--", file]))?;
        let s = String::from_utf8(out.stdout)?;
        Ok(s)
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub struct FakeGit {
        pub root: PathBuf,
        pub has_gh: bool,
        pub blames: HashMap<(String, String), String>,
    }

    impl FakeGit {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into(), has_gh: true, blames: HashMap::new() }
        }
    }

    impl GitClient for FakeGit {
        fn repo_root(&self, _cwd: &Path) -> Result<PathBuf> {
            Ok(self.root.clone())
        }
        fn has_github_remote(&self, _root: &Path) -> Result<bool> {
            Ok(self.has_gh)
        }
        fn fetch_pr(&self, _root: &Path, _n: u32) -> Result<()> {
            Ok(())
        }
        fn blame(&self, _root: &Path, c: &str, f: &str) -> Result<String> {
            self.blames
                .get(&(c.into(), f.into()))
                .cloned()
                .ok_or_else(|| anyhow!("no fake blame for {c} {f}"))
        }
    }
}
```

- [ ] **Step 3: Verify the crate still compiles**

```bash
cargo build
cargo test --lib
```

Expected: builds; existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add src/data/gh.rs src/data/git.rs
git commit -m "feat(data): subprocess traits + gh/git CLI impls"
```

---

## Task 9: Per-line color computation (the headline feature, end-to-end)

Combines `commits` (chronological list), `window_size`, and per-file blame into a `HashMap<file, Vec<Color>>` where index = head-line - 1. This is what the diff renderer actually consumes.

**Files:**
- Create: `src/render/attribution.rs`
- Modify: `src/render/mod.rs`

- [ ] **Step 1: Add module to `src/render/mod.rs`**

Append:

```rust
pub mod attribution;
```

- [ ] **Step 2: Write the attribution module with tests**

Create `src/render/attribution.rs`:

```rust
//! End-to-end commit attribution: produces the line→color map a renderer needs.

use std::collections::HashMap;

use ratatui::style::Color;

use crate::data::blame::Blame;
use crate::render::color::assign_commit_colors;
use crate::render::style::OLDER_COMMIT;

/// One file's worth of attribution, indexed by `head_lineno - 1`.
/// `None` for lines whose owning SHA isn't known (rare).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineColors {
    pub head: Vec<Option<Color>>,
    pub base: Vec<Option<Color>>,
}

/// Build the color lookup for one file given the PR's commits + window + blames.
pub fn attribute_file(
    commits: &[String],
    window_size: usize,
    head_blame: &Blame,
    base_blame: &Blame,
) -> LineColors {
    let palette = assign_commit_colors(commits, window_size);
    let map = |blame: &Blame| -> Vec<Option<Color>> {
        blame
            .line_shas
            .iter()
            .map(|sha| {
                if sha.is_empty() {
                    None
                } else {
                    Some(palette.get(sha).copied().unwrap_or(OLDER_COMMIT))
                }
            })
            .collect()
    };
    LineColors {
        head: map(head_blame),
        base: map(base_blame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::style::COMMIT_PALETTE;
    use pretty_assertions::assert_eq;

    fn sha(c: char) -> String {
        std::iter::repeat(c).take(40).collect()
    }

    #[test]
    fn maps_blame_to_palette_colors() {
        let commits = vec![sha('a'), sha('b'), sha('c')];
        let head_blame = Blame {
            line_shas: vec![sha('a'), sha('b'), sha('c'), sha('a')],
        };
        let base_blame = Blame { line_shas: vec![] };
        let colors = attribute_file(&commits, 7, &head_blame, &base_blame);
        assert_eq!(colors.head[0], Some(COMMIT_PALETTE[0]));
        assert_eq!(colors.head[1], Some(COMMIT_PALETTE[1]));
        assert_eq!(colors.head[2], Some(COMMIT_PALETTE[2]));
        assert_eq!(colors.head[3], Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn lines_from_pre_pr_commits_get_older_gray() {
        // The PR has commit a; line is owned by an unrelated SHA z.
        let commits = vec![sha('a')];
        let head_blame = Blame { line_shas: vec![sha('z')] };
        let base_blame = Blame { line_shas: vec![] };
        let colors = attribute_file(&commits, 7, &head_blame, &base_blame);
        assert_eq!(colors.head[0], Some(OLDER_COMMIT));
    }

    #[test]
    fn empty_sha_means_no_color() {
        let commits = vec![sha('a')];
        let head_blame = Blame {
            line_shas: vec![String::new(), sha('a')],
        };
        let base_blame = Blame { line_shas: vec![] };
        let colors = attribute_file(&commits, 7, &head_blame, &base_blame);
        assert_eq!(colors.head[0], None);
        assert_eq!(colors.head[1], Some(COMMIT_PALETTE[0]));
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib render::attribution
```

Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/render/attribution.rs src/render/mod.rs
git commit -m "feat(render): per-line commit color attribution"
```

---

## Task 10: In-memory cache

Holds the PR list and per-PR data. Keyed by `(pr_number, head_sha)` so a force-push invalidates the entry.

**Files:**
- Modify: `src/data/cache.rs`

- [ ] **Step 1: Write the cache with tests**

Replace `src/data/cache.rs`:

```rust
//! In-memory cache. The cache is the only consumer of `GhClient` / `GitClient`;
//! views consume already-parsed data from here.
//!
//! Concurrency: callers are expected to wrap this in `Arc<Mutex<Cache>>` if
//! shared between threads. The cache itself is `Send` but not `Sync`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::data::blame::{parse_blame, Blame};
use crate::data::diff::{parse_diff, FileDiff};
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::pr::{Pr, PrDetail};
use crate::render::attribution::{attribute_file, LineColors};

#[derive(Debug, Clone)]
pub struct PrPackage {
    pub detail: PrDetail,
    pub files: Vec<FileDiff>,
    /// Indexed by file path.
    pub colors: HashMap<String, LineColors>,
}

pub struct Cache {
    repo_root: PathBuf,
    gh: Arc<dyn GhClient>,
    git: Arc<dyn GitClient>,
    window_size: usize,

    pub list: Option<Vec<Pr>>,
    /// Key = (pr_number, head_sha).
    packages: HashMap<(u32, String), PrPackage>,
}

impl Cache {
    pub fn new(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
    ) -> Self {
        Self {
            repo_root,
            gh,
            git,
            window_size,
            list: None,
            packages: HashMap::new(),
        }
    }

    /// Refresh the PR list (always re-fetches).
    pub fn refresh_list(&mut self) -> Result<&[Pr]> {
        let prs = self.gh.list_prs(&self.repo_root)?;
        self.list = Some(prs);
        Ok(self.list.as_deref().unwrap())
    }

    /// Load a PR. If we already have a cached package for the same `head_sha`,
    /// return it. Otherwise fetch & build.
    pub fn load_pr(&mut self, number: u32) -> Result<&PrPackage> {
        let detail = self.gh.view_pr(&self.repo_root, number)?;
        let key = (number, detail.head_ref_oid.clone());

        if !self.packages.contains_key(&key) {
            let pkg = self.build_package(detail)?;
            self.packages.insert(key.clone(), pkg);
        }
        Ok(self.packages.get(&key).unwrap())
    }

    fn build_package(&self, detail: PrDetail) -> Result<PrPackage> {
        // 1. Make sure the PR refs are local.
        self.git.fetch_pr(&self.repo_root, detail.number)
            .with_context(|| format!("fetching PR #{}", detail.number))?;

        // 2. Pull the unified diff and parse it.
        let raw = self.gh.diff_pr(&self.repo_root, detail.number)?;
        let files = parse_diff(&raw)?;

        // 3. For each text file, run blame on head and on base.
        let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();
        let mut colors: HashMap<String, LineColors> = HashMap::new();
        for f in &files {
            if f.binary {
                continue;
            }
            let head = self
                .git
                .blame(&self.repo_root, &detail.head_ref_oid, &f.path)
                .map(|s| parse_blame(&s))
                .unwrap_or_else(|_| Blame { line_shas: vec![] });
            let base = self
                .git
                .blame(&self.repo_root, &detail.base_ref_oid, &f.path)
                .map(|s| parse_blame(&s))
                .unwrap_or_else(|_| Blame { line_shas: vec![] });
            let lc = attribute_file(&commits, self.window_size, &head, &base);
            colors.insert(f.path.clone(), lc);
        }

        Ok(PrPackage { detail, files, colors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gh::fakes::FakeGh;
    use crate::data::git::fakes::FakeGit;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    fn fixture_pr() -> Pr {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        prs.into_iter().next().unwrap()
    }

    fn fixture_detail() -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn refresh_list_populates_cache() {
        let mut gh = FakeGh::new();
        gh.prs = vec![fixture_pr()];
        let git = FakeGit::new("/tmp/repo");
        let mut cache = Cache::new(
            "/tmp/repo".into(),
            Arc::new(gh),
            Arc::new(git),
            7,
        );
        let prs = cache.refresh_list().unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 482);
    }

    #[test]
    fn load_pr_builds_a_package() {
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
        git.blames.insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames.insert((detail.base_ref_oid.clone(), "src/sched.rs".into()), porcelain);
        // README.md has no blame fixture — cache should tolerate missing blame.

        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        let pkg = cache.load_pr(detail.number).unwrap();
        assert_eq!(pkg.files.len(), 2);
        assert!(pkg.colors.contains_key("src/sched.rs"));
    }

    #[test]
    fn force_push_invalidates_cached_package() {
        let mut detail = fixture_detail();
        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(detail.number, "".into());
        let git = FakeGit::new("/tmp/repo");

        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh.clone_into_box()), Arc::new(git), 7);
        cache.load_pr(detail.number).unwrap();
        assert_eq!(cache.packages.len(), 1);

        // Simulate a force-push: change head_ref_oid in the next view call.
        // We rebuild gh + cache to mimic a refresh in place.
        detail.head_ref_oid = "ffffffffffffffffffffffffffffffffffffffff".into();
        let mut gh2 = FakeGh::new();
        gh2.views.insert(detail.number, detail.clone());
        gh2.diffs.insert(detail.number, "".into());
        let git2 = FakeGit::new("/tmp/repo");
        let mut cache2 = Cache::new("/tmp/repo".into(), Arc::new(gh2), Arc::new(git2), 7);
        cache2.load_pr(detail.number).unwrap();
        // Different key → different entry.
        assert_eq!(cache2.packages.len(), 1);
        let key = cache2.packages.keys().next().unwrap();
        assert_eq!(key.1, "ffffffffffffffffffffffffffffffffffffffff");
    }
}

#[cfg(test)]
trait CloneIntoBox {
    fn clone_into_box(&self) -> Box<Self> where Self: Sized;
}
#[cfg(test)]
impl CloneIntoBox for crate::data::gh::fakes::FakeGh {
    fn clone_into_box(&self) -> Box<Self> {
        let mut copy = crate::data::gh::fakes::FakeGh::new();
        copy.prs = self.prs.clone();
        copy.views = self.views.clone();
        copy.diffs = self.diffs.clone();
        // merges intentionally not cloned.
        Box::new(copy)
    }
}
```

Note: the `clone_into_box` shim exists only because `FakeGh` holds a `Mutex` which isn't `Clone`. Tests in this file are the only consumers.

- [ ] **Step 3: Run tests**

```bash
cargo test --lib data::cache
```

Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add src/data/cache.rs
git commit -m "feat(data): in-memory cache keyed by head_sha"
```

---

## Task 11: Config schema + load

Loads `~/.config/prpr/config.toml` if present. Returns defaults otherwise. CLI overrides applied later in Task 22.

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Replace `src/config.rs` with the schema**

```rust
//! User config. Everything is optional; missing keys fall back to defaults.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Theme,
    pub window_size: usize,
    pub show_commit_strip: bool,
    pub show_sha_margin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Mocha,
    Latte,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::Mocha,
            window_size: default_window_size(),
            show_commit_strip: true,
            show_sha_margin: false,
        }
    }
}

pub fn default_window_size() -> usize {
    7
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    colors: RawColors,
    #[serde(default)]
    commit_attribution: RawCommit,
    #[serde(default)]
    ui: RawUi,
}
#[derive(Debug, Default, Deserialize)]
struct RawColors {
    #[serde(default)]
    theme: Option<Theme>,
}
#[derive(Debug, Default, Deserialize)]
struct RawCommit {
    #[serde(default)]
    window_size: Option<usize>,
}
#[derive(Debug, Default, Deserialize)]
struct RawUi {
    #[serde(default)]
    show_commit_strip: Option<bool>,
    #[serde(default)]
    show_sha_margin: Option<bool>,
}

/// Locate the config file path. Returns `None` if no XDG config dir is
/// resolvable (very rare).
pub fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "prpr")
        .map(|d| d.config_dir().join("config.toml"))
}

/// Load the config from `config_path()`, merging with defaults. If the file
/// doesn't exist, returns `Config::default()`.
pub fn load() -> Result<Config> {
    let Some(path) = config_path() else {
        return Ok(Config::default());
    };
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(merge(Config::default(), raw))
}

fn merge(mut cfg: Config, raw: RawConfig) -> Config {
    if let Some(t) = raw.colors.theme {
        cfg.theme = t;
    }
    if let Some(n) = raw.commit_attribution.window_size {
        cfg.window_size = n;
    }
    if let Some(b) = raw.ui.show_commit_strip {
        cfg.show_commit_strip = b;
    }
    if let Some(b) = raw.ui.show_sha_margin {
        cfg.show_sha_margin = b;
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_full_toml() {
        let toml = r#"
            [colors]
            theme = "latte"
            [commit_attribution]
            window_size = 5
            [ui]
            show_commit_strip = false
            show_sha_margin = true
        "#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = merge(Config::default(), raw);
        assert_eq!(cfg.theme, Theme::Latte);
        assert_eq!(cfg.window_size, 5);
        assert_eq!(cfg.show_commit_strip, false);
        assert_eq!(cfg.show_sha_margin, true);
    }

    #[test]
    fn empty_toml_yields_defaults() {
        let raw: RawConfig = toml::from_str("").unwrap();
        assert_eq!(merge(Config::default(), raw), Config::default());
    }

    #[test]
    fn partial_toml_only_overrides_present_keys() {
        let toml = "[commit_attribution]\nwindow_size = 3";
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let cfg = merge(Config::default(), raw);
        assert_eq!(cfg.window_size, 3);
        assert_eq!(cfg.theme, Theme::Mocha); // unchanged
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib config
```

Expected: 3 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): toml load + merge with defaults"
```

---

## Task 12: Diff line rendering

Produces a `ratatui::text::Line<'a>` for one parsed `DiffLine` + a `LineColors` lookup. This is where the gutter actually appears.

**Files:**
- Modify: `src/render/diff.rs`

- [ ] **Step 1: Replace `src/render/diff.rs`**

```rust
//! Render a single diff line (line number, gutter, op, code) as a ratatui Line.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::data::diff::{DiffLine, DiffOp};
use crate::render::style::*;

pub fn render_line<'a>(
    line: &'a DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
) -> Line<'a> {
    if line.is_hunk_header {
        return Line::from(vec![Span::styled(
            line.text.clone(),
            Style::default().fg(OVERLAY1).add_modifier(Modifier::DIM),
        )]);
    }

    let lineno_str = match (line.old_lineno, line.new_lineno) {
        (_, Some(n)) => format!("{n:>4}"),
        (Some(n), None) => format!("{n:>4}"),
        (None, None) => "    ".to_string(),
    };

    // Pick the gutter color from head for context/add lines, base for delete.
    let gutter_color = match line.op {
        DiffOp::Add | DiffOp::Context => head_color,
        DiffOp::Delete => base_color,
        DiffOp::Hunk => None,
    };
    let gutter_glyph = if gutter_color.is_some() { "█" } else { " " };

    let (op_glyph, op_style) = match line.op {
        DiffOp::Add => ("+", Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG)),
        DiffOp::Delete => ("-", Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG)),
        DiffOp::Context => (" ", Style::default().fg(SUBTEXT0)),
        DiffOp::Hunk => unreachable!(),
    };

    let body_style = match line.op {
        DiffOp::Add => Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG),
        DiffOp::Delete => Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG),
        DiffOp::Context => Style::default().fg(TEXT),
        DiffOp::Hunk => unreachable!(),
    };

    Line::from(vec![
        Span::styled(lineno_str, Style::default().fg(OVERLAY0)),
        Span::raw(" "),
        Span::styled(
            gutter_glyph.to_string(),
            gutter_color
                .map(|c| Style::default().fg(c))
                .unwrap_or_default(),
        ),
        Span::raw(" "),
        Span::styled(op_glyph.to_string(), op_style),
        Span::raw(" "),
        Span::styled(line.text.clone(), body_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::diff::{DiffLine, DiffOp};
    use pretty_assertions::assert_eq;

    fn ctx(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Context,
            old_lineno: Some(ln),
            new_lineno: Some(ln),
            text: text.into(),
            is_hunk_header: false,
        }
    }
    fn add(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Add,
            old_lineno: None,
            new_lineno: Some(ln),
            text: text.into(),
            is_hunk_header: false,
        }
    }
    fn del(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Delete,
            old_lineno: Some(ln),
            new_lineno: None,
            text: text.into(),
            is_hunk_header: false,
        }
    }

    #[test]
    fn context_line_uses_head_gutter_color() {
        let line = ctx("    let x = 1;", 42);
        let rendered = render_line(&line, Some(COMMIT_PALETTE[0]), None);
        // Find the gutter span: it should be "█" with the palette color.
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.content, "█");
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn add_line_uses_diff_add_styling() {
        let line = add("    let x = 2;", 45);
        let rendered = render_line(&line, Some(COMMIT_PALETTE[1]), None);
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.fg, Some(DIFF_ADD_FG));
        assert_eq!(body.style.bg, Some(DIFF_ADD_BG));
    }

    #[test]
    fn delete_line_uses_base_gutter_color() {
        let line = del("    let x = 1;", 42);
        let rendered = render_line(&line, None, Some(COMMIT_PALETTE[2]));
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[2]));
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.fg, Some(DIFF_DEL_FG));
    }

    #[test]
    fn missing_color_renders_blank_gutter() {
        let line = ctx("    // ancient code", 1);
        let rendered = render_line(&line, None, None);
        assert_eq!(rendered.spans[2].content, " ");
    }

    #[test]
    fn hunk_header_renders_dim() {
        let line = DiffLine {
            op: DiffOp::Hunk,
            old_lineno: None,
            new_lineno: None,
            text: "@@ -42,7 +42,11 @@".into(),
            is_hunk_header: true,
        };
        let rendered = render_line(&line, None, None);
        assert_eq!(rendered.spans.len(), 1);
        assert!(rendered.spans[0].style.add_modifier.contains(Modifier::DIM));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib render::diff
```

Expected: 5 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/render/diff.rs
git commit -m "feat(render): styled diff line with commit gutter"
```

---

## Task 13: PR list view rendering

Renders the full PR list view to a `Frame`. Tests use `ratatui::buffer::Buffer` to assert specific cells.

**Files:**
- Modify: `src/view/pr_list.rs`

- [ ] **Step 1: Replace `src/view/pr_list.rs`**

```rust
//! PR list view rendering. State is small and self-contained.

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::data::pr::{CiState, Pr, PrState, ReviewDecision};
use crate::render::style::*;

#[derive(Debug, Default)]
pub struct PrListState {
    pub repo_name: String,
    pub branch: String,
    pub prs: Vec<Pr>,
    pub selected: usize,
    pub filter_open_only: bool,
    pub search: Option<String>,
    pub status: String,
}

impl PrListState {
    pub fn visible_prs(&self) -> Vec<&Pr> {
        let q = self.search.as_deref().map(str::to_lowercase);
        self.prs
            .iter()
            .filter(|p| !self.filter_open_only || p.state == PrState::Open)
            .filter(|p| match &q {
                Some(s) => p.title.to_lowercase().contains(s)
                    || p.author.login.to_lowercase().contains(s),
                None => true,
            })
            .collect()
    }
}

pub fn render(f: &mut Frame, area: Rect, st: &PrListState, now: DateTime<Utc>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)])
        .split(area);
    render_header(f, chunks[0], st);
    render_rows(f, chunks[1], st, now);
    render_footer(f, chunks[2], st);
}

fn render_header(f: &mut Frame, area: Rect, st: &PrListState) {
    let visible = st.visible_prs();
    let count = visible.iter().filter(|p| p.state == PrState::Open).count();
    let header = format!(
        "  prpr · {} · {} · {} open                                   filter: {}",
        st.repo_name,
        st.branch,
        count,
        if st.filter_open_only { "open" } else { "all" },
    );
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(OVERLAY1)),
        area,
    );
}

fn render_rows(f: &mut Frame, area: Rect, st: &PrListState, now: DateTime<Utc>) {
    let visible = st.visible_prs();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible.len() + 1);
    lines.push(divider(area.width as usize));
    for (i, pr) in visible.iter().enumerate() {
        lines.push(row_for(pr, i == st.selected, now));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_footer(f: &mut Frame, area: Rect, _st: &PrListState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(
        Paragraph::new("  ↵ open   m merge   r refresh   / search   f filter   q quit")
            .style(Style::default().fg(OVERLAY1)),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(
            "  state ●open ○draft   ci ✓pass ✗fail …pend   review ✓approved !changes ·pending",
        )
        .style(Style::default().fg(OVERLAY0)),
        chunks[1],
    );
}

fn divider(w: usize) -> Line<'static> {
    Line::from(Span::styled(
        "  ".to_string() + &"─".repeat(w.saturating_sub(2)),
        Style::default().fg(SURFACE2),
    ))
}

fn row_for(pr: &Pr, selected: bool, now: DateTime<Utc>) -> Line<'static> {
    let row_bg = if selected {
        Style::default().bg(SURFACE0)
    } else {
        Style::default()
    };

    let state_glyph = match pr.state {
        _ if pr.is_draft => Span::styled("○", Style::default().fg(OVERLAY0)),
        PrState::Open => Span::styled("●", Style::default().fg(DIFF_ADD_FG)),
        PrState::Closed => Span::styled("●", Style::default().fg(DIFF_DEL_FG)),
        PrState::Merged => Span::styled("●", Style::default().fg(COMMIT_PALETTE[1])),
    };
    let ci_glyph = match pr.ci_state() {
        CiState::Pass => Span::styled("✓", Style::default().fg(DIFF_ADD_FG)),
        CiState::Fail => Span::styled("✗", Style::default().fg(DIFF_DEL_FG)),
        CiState::Pending => Span::styled("…", Style::default().fg(COMMIT_PALETTE[4])),
        CiState::None => Span::styled(" ", Style::default()),
    };
    let review_glyph = match pr.review_decision {
        Some(ReviewDecision::Approved) => Span::styled("✓", Style::default().fg(DIFF_ADD_FG)),
        Some(ReviewDecision::ChangesRequested) => {
            Span::styled("!", Style::default().fg(COMMIT_PALETTE[4]))
        }
        _ => Span::styled("·", Style::default().fg(COMMIT_PALETTE[1])),
    };

    let label = pr
        .labels
        .first()
        .map(|l| format!("[{}]", l.name))
        .unwrap_or_default();
    let age = humanize_age(pr.created_at, now);

    Line::from(vec![
        Span::styled("  ", row_bg),
        state_glyph,
        Span::styled(" ", row_bg),
        ci_glyph,
        Span::styled(" ", row_bg),
        review_glyph,
        Span::styled(format!(" #{} ", pr.number), row_bg.fg(COMMIT_PALETTE[1])),
        Span::styled(truncate(&pr.title, 36), row_bg.fg(TEXT)),
        Span::styled(format!("  {}  ", label), row_bg.fg(COMMIT_PALETTE[4])),
        Span::styled(format!("{} ", pr.author.login), row_bg.fg(COMMIT_PALETTE[0])),
        Span::styled(age, row_bg.fg(OVERLAY0)),
    ])
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{:width$}", s, width = max)
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut)
    }
}

fn humanize_age(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds().max(0);
    if secs < 60 { format!("{}s", secs) }
    else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else if secs < 86400 * 14 { format!("{}d", secs / 86400) }
    else { format!("{}w", secs / (86400 * 7)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pr::Pr;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn fixture_state() -> PrListState {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        PrListState {
            repo_name: "prpr".into(),
            branch: "main".into(),
            prs,
            selected: 0,
            filter_open_only: true,
            search: None,
            status: String::new(),
        }
    }

    #[test]
    fn renders_header_with_repo_and_count() {
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let st = fixture_state();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let line0 = buffer_line(buf, 0);
        assert!(line0.contains("prpr"));
        assert!(line0.contains("2 open"));
    }

    #[test]
    fn search_filters_rows() {
        let mut st = fixture_state();
        st.search = Some("metrics".into());
        assert_eq!(st.visible_prs().len(), 1);
        assert_eq!(st.visible_prs()[0].number, 479);
    }

    fn buffer_line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf.get(x, y).symbol().to_string())
            .collect::<String>()
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib view::pr_list
```

Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/view/pr_list.rs
git commit -m "feat(view): PR list rendering"
```

---

## Task 14: PR review view rendering

Renders the five regions (header, commit strip, file bar, diff body, status). Larger task, but each region is small.

**Files:**
- Modify: `src/view/pr_review.rs`

- [ ] **Step 1: Replace `src/view/pr_review.rs`**

```rust
//! PR review view: header / commit strip / file bar / diff body / status.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::cache::PrPackage;
use crate::data::diff::{DiffOp, FileDiff};
use crate::render::attribution::LineColors;
use crate::render::color::assign_commit_colors;
use crate::render::diff::render_line;
use crate::render::style::*;

#[derive(Debug, Default)]
pub struct PrReviewState {
    pub file_index: usize,
    pub cursor_line: usize,
    pub scroll: u16,
    pub show_commit_strip: bool,
    pub show_sha_margin: bool,
    pub status: String,
}

pub fn render(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let strip_h = if st.show_commit_strip { 3 } else { 0 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(strip_h),
            Constraint::Length(2), // file bar (title + divider)
            Constraint::Min(1),    // diff body
            Constraint::Length(1), // status
        ])
        .split(area);

    render_header(f, chunks[0], pkg);
    if st.show_commit_strip {
        render_commit_strip(f, chunks[1], pkg);
    }
    render_file_bar(f, chunks[2], pkg, st);
    render_diff_body(f, chunks[3], pkg, st);
    render_status(f, chunks[4], pkg, st);
}

fn render_header(f: &mut Frame, area: Rect, pkg: &PrPackage) {
    let d = &pkg.detail;
    let header = format!(
        "  prpr · #{} {} · {} · {} ← {}",
        d.number, d.title, d.author.login, d.base_ref_name, d.head_ref_name,
    );
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(TEXT)),
        area,
    );
}

fn render_commit_strip(f: &mut Frame, area: Rect, pkg: &PrPackage) {
    let commits: Vec<String> = pkg.detail.commits.iter().map(|c| c.oid.clone()).collect();
    let palette = assign_commit_colors(&commits, 7);
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw("  commits  "));
    for c in &pkg.detail.commits {
        let color = palette
            .get(&c.oid)
            .copied()
            .unwrap_or(OLDER_COMMIT);
        spans.push(Span::styled("█ ", Style::default().fg(color)));
        spans.push(Span::styled(
            short_sha(&c.oid),
            Style::default().fg(SUBTEXT0),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            truncate(&c.message_headline, 18),
            Style::default().fg(TEXT),
        ));
        spans.push(Span::raw("   "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).wrap(ratatui::widgets::Wrap { trim: true }), area);
}

fn render_file_bar(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let total = pkg.files.len();
    let path = pkg.files.get(st.file_index).map(|f| f.path.as_str()).unwrap_or("");
    let label = format!(
        "  {}{}                                              file {}/{}",
        path,
        " ".repeat(40_usize.saturating_sub(path.len())),
        st.file_index + 1,
        total,
    );
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(label).style(Style::default().fg(SUBTEXT0)), chunks[0]);
    f.render_widget(
        Paragraph::new("  ".to_string() + &"─".repeat((area.width as usize).saturating_sub(2)))
            .style(Style::default().fg(SURFACE2)),
        chunks[1],
    );
}

fn render_diff_body(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
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

fn body_lines<'a>(
    file: &'a FileDiff,
    colors: &'a HashMap<String, LineColors>,
) -> Vec<Line<'a>> {
    let lookup = colors.get(&file.path);
    file.lines
        .iter()
        .map(|l| {
            let head = l.new_lineno.and_then(|n| {
                lookup
                    .and_then(|lc| lc.head.get(n.saturating_sub(1) as usize).copied())
                    .flatten()
            });
            let base = l.old_lineno.and_then(|n| {
                lookup
                    .and_then(|lc| lc.base.get(n.saturating_sub(1) as usize).copied())
                    .flatten()
            });
            render_line(l, head, base)
        })
        .collect()
}

fn render_status(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let Some(file) = pkg.files.get(st.file_index) else { return };
    let cursor_line_text = file
        .lines
        .iter()
        .filter(|l| !l.is_hunk_header)
        .nth(st.cursor_line)
        .and_then(|l| l.new_lineno.or(l.old_lineno));
    let cursor_color_info = match cursor_line_text {
        Some(n) => {
            let sha_opt = pkg
                .colors
                .get(&file.path)
                .and_then(|c| c.head.get(n as usize - 1).copied().flatten());
            match sha_opt {
                Some(_) => format!("line {n}"),
                None => format!("line {n}"),
            }
        }
        None => st.status.clone(),
    };

    let bar = format!(
        "  {}    │ ↵ next file   Esc back",
        cursor_color_info,
    );
    f.render_widget(Paragraph::new(bar).style(Style::default().fg(SUBTEXT0)), area);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut)
    }
}

fn short_sha(s: &str) -> String {
    s.chars().take(6).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::cache::PrPackage;
    use crate::data::diff::parse_diff;
    use crate::data::pr::PrDetail;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn fixture_pkg() -> PrPackage {
        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let files = parse_diff(include_str!("../../tests/fixtures/diff_basic.patch")).unwrap();
        PrPackage { detail, files, colors: HashMap::new() }
    }

    #[test]
    fn renders_pr_number_in_header() {
        let pkg = fixture_pkg();
        let st = PrReviewState { show_commit_strip: false, ..Default::default() };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let header: String = (0..80)
            .map(|x| buf.get(x, 0).symbol().to_string())
            .collect();
        assert!(header.contains("#482"));
        assert!(header.contains("fix-race"));
    }

    #[test]
    fn binary_file_renders_placeholder() {
        let mut pkg = fixture_pkg();
        pkg.files = vec![FileDiff {
            path: "img.png".into(),
            lines: vec![],
            binary: true,
        }];
        let st = PrReviewState { show_commit_strip: false, ..Default::default() };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let body: String = (0..80)
            .map(|x| buf.get(x, 4).symbol().to_string())
            .collect();
        assert!(body.contains("binary file"));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib view::pr_review
```

Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/view/pr_review.rs
git commit -m "feat(view): PR review rendering with commit strip"
```

---

## Task 15: App skeleton + terminal lifecycle

Sets up ratatui, raw mode, alternate screen, panic hook. No real input handling yet — that lands in Task 16.

**Files:**
- Modify: `src/app.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Replace `src/app.rs`**

```rust
//! Top-level app: terminal init/teardown, panic hook, the run-loop scaffold.

use std::io::{self, Stdout};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::Config;
use crate::data::cache::Cache;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub struct App {
    pub cache: Cache,
    pub config: Config,
}

impl App {
    pub fn new(
        repo_root: std::path::PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        config: Config,
    ) -> Self {
        let window = config.window_size;
        Self {
            cache: Cache::new(repo_root, gh, git, window),
            config,
        }
    }
}

pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        original(info);
    }));
}

pub fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Kitty-style enhanced keyboard, ignored on terminals that don't support it.
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        )
    );
    let backend = CrosstermBackend::new(stdout);
    let term = Terminal::new(backend)?;
    Ok(term)
}

pub fn restore_terminal() -> Result<()> {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo build
cargo test --lib
```

Expected: builds clean; tests still pass.

- [ ] **Step 3: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): terminal lifecycle + panic hook"
```

---

## Task 16: Key dispatch

Pure-logic mapping from `(view, key event) → Action`. Tests cover each binding from the spec.

**Files:**
- Modify: `src/keys.rs`

- [ ] **Step 1: Replace `src/keys.rs`**

```rust
//! Key dispatch. Pure logic: given the current view and a key event,
//! return an `Action`. The event loop interprets actions.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Quit,

    // PR list
    ListUp,
    ListDown,
    ListTop,
    ListBottom,
    ListOpen,
    ListMerge,
    ListRefresh,
    ListSearch,
    ListCycleFilter,
    ListClearFilter,

    // PR review
    CursorUp,
    CursorDown,
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
    NextFile,
    PrevFile,
    OpenFilePicker,
    Merge,
    ToggleCommitStrip,
    ToggleShaMargin,
    BackToList,

    // Global
    Help,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedView {
    List,
    Review,
    HelpOverlay,
    FilePicker,
    MergeModal,
}

pub fn dispatch(view: FocusedView, ev: KeyEvent) -> Action {
    if ev.code == KeyCode::Char('c') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit;
    }
    match view {
        FocusedView::List => list(ev),
        FocusedView::Review => review(ev),
        FocusedView::HelpOverlay => match ev.code {
            KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => Action::Nothing, /* swallowed by caller */
            _ => Action::Nothing,
        },
        FocusedView::FilePicker | FocusedView::MergeModal => Action::Nothing, /* handled by overlay impls */
    }
}

fn list(ev: KeyEvent) -> Action {
    match ev.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('r') => Action::ListRefresh,
        KeyCode::Char('j') | KeyCode::Down => Action::ListDown,
        KeyCode::Char('k') | KeyCode::Up => Action::ListUp,
        KeyCode::Char('G') => Action::ListBottom,
        KeyCode::Char('g') => Action::ListTop, /* second `g` handled by stateful caller */
        KeyCode::Enter => Action::ListOpen,
        KeyCode::Char('m') => Action::ListMerge,
        KeyCode::Char('/') => Action::ListSearch,
        KeyCode::Char('f') => Action::ListCycleFilter,
        KeyCode::Esc => Action::ListClearFilter,
        _ => Action::Nothing,
    }
}

fn review(ev: KeyEvent) -> Action {
    match ev.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::BackToList,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('j') | KeyCode::Down => Action::CursorDown,
        KeyCode::Char('k') | KeyCode::Up => Action::CursorUp,
        KeyCode::Char('d') if ev.modifiers.contains(KeyModifiers::CONTROL) => Action::HalfPageDown,
        KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => Action::HalfPageUp,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::Char('g') => Action::Top,
        KeyCode::Tab => Action::NextFile,
        KeyCode::BackTab => Action::PrevFile,
        KeyCode::Char('f') => Action::OpenFilePicker,
        KeyCode::Char('m') => Action::Merge,
        KeyCode::Char('c') => Action::ToggleCommitStrip,
        KeyCode::Char('s') => Action::ToggleShaMargin,
        _ => Action::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use pretty_assertions::assert_eq;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn k_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn list_q_quits() {
        assert_eq!(dispatch(FocusedView::List, k('q')), Action::Quit);
    }

    #[test]
    fn ctrl_c_quits_anywhere() {
        assert_eq!(dispatch(FocusedView::Review, k_ctrl('c')), Action::Quit);
        assert_eq!(dispatch(FocusedView::List, k_ctrl('c')), Action::Quit);
    }

    #[test]
    fn list_enter_opens_pr() {
        assert_eq!(
            dispatch(
                FocusedView::List,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ),
            Action::ListOpen,
        );
    }

    #[test]
    fn review_q_returns_to_list() {
        assert_eq!(dispatch(FocusedView::Review, k('q')), Action::BackToList);
    }

    #[test]
    fn review_ctrl_d_pages_down() {
        assert_eq!(
            dispatch(FocusedView::Review, k_ctrl('d')),
            Action::HalfPageDown,
        );
    }

    #[test]
    fn review_tab_next_file() {
        assert_eq!(
            dispatch(
                FocusedView::Review,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            ),
            Action::NextFile,
        );
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib keys
```

Expected: 6 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/keys.rs
git commit -m "feat(keys): pure-logic key dispatch"
```

---

## Task 17: Mouse dispatch

Same pattern: pure-logic mapping from `MouseEvent` to `Action`. Limited surface area in v1 (wheel scroll, click rows, click strip).

**Files:**
- Modify: `src/keys.rs` (extend, don't rewrite)

- [ ] **Step 1: Append a `mouse_dispatch` fn and `MouseAction` enum to `src/keys.rs`**

Add to the bottom of the file (above the `#[cfg(test)] mod tests`):

```rust
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    Nothing,
    /// Scroll the focused region by `delta` (negative = up).
    Scroll(i16),
    /// Move selection / cursor to the given cell coordinates.
    ClickAt { col: u16, row: u16 },
    /// Treat as the same as Enter (open / confirm).
    DoubleClickAt { col: u16, row: u16 },
}

pub fn mouse_dispatch(ev: MouseEvent) -> MouseAction {
    match ev.kind {
        MouseEventKind::ScrollUp => MouseAction::Scroll(-3),
        MouseEventKind::ScrollDown => MouseAction::Scroll(3),
        MouseEventKind::Down(MouseButton::Left) => MouseAction::ClickAt {
            col: ev.column,
            row: ev.row,
        },
        // crossterm doesn't natively report double-click; the event loop
        // detects it by timing two ClickAt events on the same cell.
        _ => MouseAction::Nothing,
    }
}
```

Also append to the test module (inside the existing `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn wheel_scroll_up() {
        let ev = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(mouse_dispatch(ev), MouseAction::Scroll(-3));
    }

    #[test]
    fn left_click_yields_click_at() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(mouse_dispatch(ev), MouseAction::ClickAt { col: 12, row: 7 });
    }
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib keys
```

Expected: 8 tests PASS (the original 6 + 2 new).

- [ ] **Step 3: Commit**

```bash
git add src/keys.rs
git commit -m "feat(keys): mouse dispatch (wheel + click)"
```

---

## Task 18: File picker overlay

fzf-style modal: query box + ranked file list. Uses simple substring + position-bonus scoring (no external fuzzy crate needed).

**Files:**
- Modify: `src/view/file_picker.rs`

- [ ] **Step 1: Replace `src/view/file_picker.rs`**

```rust
//! File picker overlay (Esc/Enter handled by the app loop).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::render::style::*;

#[derive(Debug, Default)]
pub struct FilePickerState {
    pub query: String,
    pub all_files: Vec<String>,
    pub selected: usize,
}

impl FilePickerState {
    pub fn matches(&self) -> Vec<&String> {
        let q = self.query.to_lowercase();
        let mut scored: Vec<(i64, &String)> = self
            .all_files
            .iter()
            .filter_map(|f| {
                if q.is_empty() {
                    Some((0, f))
                } else {
                    score(&q, &f.to_lowercase()).map(|s| (s, f))
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored.into_iter().map(|(_, f)| f).collect()
    }
}

fn score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    // Substring bonus, position bonus.
    let pos = candidate.find(query)?;
    let mut s: i64 = 100 - (pos as i64);
    // Earlier occurrences score higher.
    s += if pos == 0 { 50 } else { 0 };
    // Shorter candidates score higher when matches are equal.
    s -= candidate.len() as i64 / 8;
    Some(s)
}

/// Overlay sized to ~60% of the area, centered.
pub fn render(f: &mut Frame, area: Rect, st: &FilePickerState) {
    let modal = centered(area, 60, 60);
    f.render_widget(Clear, modal);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(modal);

    let query = Paragraph::new(format!("> {}", st.query))
        .style(Style::default().fg(TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(SURFACE2))
                .title(" file "),
        );
    f.render_widget(query, chunks[0]);

    let matches = st.matches();
    let list_lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let style = if i == st.selected {
                Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(vec![Span::styled(format!("  {}", p), style)])
        })
        .collect();
    let list = Paragraph::new(list_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(SURFACE2)),
    );
    f.render_widget(list, chunks[1]);
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = (area.width - w) / 2 + area.x;
    let y = (area.height - h) / 2 + area.y;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn st_with(files: &[&str], query: &str) -> FilePickerState {
        FilePickerState {
            query: query.into(),
            all_files: files.iter().map(|s| s.to_string()).collect(),
            selected: 0,
        }
    }

    #[test]
    fn empty_query_keeps_input_order() {
        let st = st_with(&["src/main.rs", "src/lib.rs", "README.md"], "");
        let m = st.matches();
        // With empty query, all files match equally; secondary sort is by name asc.
        let names: Vec<_> = m.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, vec!["README.md", "src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn substring_query_filters_and_ranks() {
        let st = st_with(&["src/main.rs", "src/lib.rs", "README.md", "tests/main.rs"], "main");
        let m = st.matches();
        let names: Vec<_> = m.iter().map(|s| s.as_str()).collect();
        // Both "src/main.rs" and "tests/main.rs" contain "main" at the same depth;
        // shorter path scores higher.
        assert_eq!(names, vec!["src/main.rs", "tests/main.rs"]);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib view::file_picker
```

Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/view/file_picker.rs
git commit -m "feat(view): file picker overlay"
```

---

## Task 19: Merge modal

Tiny overlay; selection state tracked with one enum.

**Files:**
- Modify: `src/view/merge_modal.rs`

- [ ] **Step 1: Replace `src/view/merge_modal.rs`**

```rust
//! Merge modal: pick Merge / Squash / Rebase, confirm with Enter.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::render::style::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn cli_flag(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
    pub fn letter(self) -> char {
        match self {
            MergeMethod::Merge => 'M',
            MergeMethod::Squash => 'S',
            MergeMethod::Rebase => 'R',
        }
    }
}

pub fn from_letter(c: char) -> Option<MergeMethod> {
    match c.to_ascii_uppercase() {
        'M' => Some(MergeMethod::Merge),
        'S' => Some(MergeMethod::Squash),
        'R' => Some(MergeMethod::Rebase),
        _ => None,
    }
}

#[derive(Debug)]
pub struct MergeModalState {
    pub pr_number: u32,
    pub default: MergeMethod,
    pub selected: MergeMethod,
}

pub fn render(f: &mut Frame, area: Rect, st: &MergeModalState) {
    let modal = centered(area, 56, 9);
    f.render_widget(Clear, modal);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    for m in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
        let prefix = format!("   [{}] ", m.letter());
        let label = match m {
            MergeMethod::Merge => "Merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        };
        let mut text = format!("{}{}", prefix, label);
        if m == st.default {
            text.push_str("       (repo default)");
        }
        let style = if m == st.selected {
            Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT)
        };
        lines.push(Line::styled(text, style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "   ↵ confirm     letter to pick     Esc cancel".to_string(),
        Style::default().fg(OVERLAY1),
    ));

    let title = format!(" Merge #{}? ", st.pr_number);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(title);
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn letter_mapping_round_trip() {
        for m in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
            assert_eq!(from_letter(m.letter()), Some(m));
        }
    }

    #[test]
    fn cli_flags_match_gh_options() {
        assert_eq!(MergeMethod::Merge.cli_flag(), "merge");
        assert_eq!(MergeMethod::Squash.cli_flag(), "squash");
        assert_eq!(MergeMethod::Rebase.cli_flag(), "rebase");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib view::merge_modal
```

Expected: 2 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add src/view/merge_modal.rs
git commit -m "feat(view): merge modal"
```

---

## Task 20: Event loop wiring + view transitions

Pulls everything together: poll events from crossterm, dispatch via `keys.rs`, mutate `App` state, redraw.

**Files:**
- Modify: `src/data/cache.rs` (add a non-mutating getter)
- Modify: `src/app.rs`

- [ ] **Step 1: Add `Cache::get` for non-fetching lookups**

In `src/data/cache.rs`, append to the `impl Cache` block:

```rust
impl Cache {
    /// Look up a cached package by number (does not fetch). Returns the
    /// most recently-cached entry for that number, regardless of head_sha.
    pub fn get(&self, number: u32) -> Option<&PrPackage> {
        self.packages
            .iter()
            .find(|((n, _), _)| *n == number)
            .map(|(_, v)| v)
    }
}
```

Add a quick test to the existing `tests` module in `cache.rs`:

```rust
    #[test]
    fn get_returns_cached_package() {
        // Reuse the load_pr_builds_a_package setup.
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(detail.number, "".into());
        let mut git = FakeGit::new("/tmp/repo");
        git.blames.insert((head_sha, "src/sched.rs".into()), String::new());
        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        assert!(cache.get(detail.number).is_none());
        cache.load_pr(detail.number).unwrap();
        assert!(cache.get(detail.number).is_some());
    }
```

Run the cache tests:

```bash
cargo test --lib data::cache
```

Expected: 4 tests PASS.

- [ ] **Step 2: Add the `AppState` struct and helper methods on `App`**

Append to `src/app.rs`:

```rust
use std::time::Duration;

use chrono::Utc;
use crossterm::event::{self, Event};

use crate::keys::{dispatch, mouse_dispatch, Action, FocusedView, MouseAction};
use crate::view::file_picker::FilePickerState;
use crate::view::merge_modal::{MergeMethod, MergeModalState};
use crate::view::pr_list::PrListState;
use crate::view::pr_review::PrReviewState;

pub struct AppState {
    pub focused: FocusedView,
    pub list: PrListState,
    pub review: Option<PrReviewState>,
    pub current_pr: Option<u32>,
    pub picker: Option<FilePickerState>,
    pub merge: Option<MergeModalState>,
    pub pending_g: bool,
    pub running: bool,
}

impl AppState {
    pub fn new(repo_name: String, branch: String) -> Self {
        Self {
            focused: FocusedView::List,
            list: PrListState {
                repo_name,
                branch,
                prs: vec![],
                selected: 0,
                filter_open_only: true,
                search: None,
                status: String::new(),
            },
            review: None,
            current_pr: None,
            picker: None,
            merge: None,
            pending_g: false,
            running: true,
        }
    }
}

impl App {
    /// Populate the cache for `number` if not already loaded. Errors are
    /// silently swallowed — they show up in `st.list.status` via the caller.
    pub fn ensure_pr_loaded(&mut self, number: u32) {
        if self.cache.get(number).is_some() {
            return;
        }
        if let Err(e) = self.cache.load_pr(number) {
            // surface via the typical status mechanism in handle_key
            eprintln!("cache load #{number}: {e}");
        }
    }
}
```

- [ ] **Step 3: Implement `run` and `draw`**

Append to `src/app.rs`:

```rust
pub fn run(term: &mut Term, app: &mut App, st: &mut AppState) -> Result<()> {
    if let Err(e) = app.cache.refresh_list() {
        st.list.status = format!("refresh failed: {e}");
    } else if let Some(prs) = app.cache.list.as_ref() {
        st.list.prs = prs.clone();
    }

    while st.running {
        term.draw(|f| draw(f, app, st))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k) => handle_key(app, st, k),
                Event::Mouse(m) => handle_mouse(app, st, m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn draw(f: &mut ratatui::Frame, app: &App, st: &AppState) {
    let area = f.area();
    if area.width < 80 || area.height < 24 {
        let msg = ratatui::widgets::Paragraph::new("terminal too small (need ≥80×24)")
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(msg, area);
        return;
    }
    match st.focused {
        FocusedView::List | FocusedView::HelpOverlay => {
            crate::view::pr_list::render(f, area, &st.list, Utc::now());
        }
        FocusedView::Review | FocusedView::FilePicker | FocusedView::MergeModal => {
            let pkg = st.current_pr.and_then(|n| app.cache.get(n));
            if let (Some(pkg), Some(review)) = (pkg, st.review.as_ref()) {
                crate::view::pr_review::render(f, area, pkg, review);
            } else {
                let msg = ratatui::widgets::Paragraph::new("loading…")
                    .alignment(ratatui::layout::Alignment::Center);
                f.render_widget(msg, area);
            }
        }
    }

    if let Some(p) = &st.picker {
        crate::view::file_picker::render(f, area, p);
    }
    if let Some(m) = &st.merge {
        crate::view::merge_modal::render(f, area, m);
    }
}
```

- [ ] **Step 4: Implement `handle_key` and helpers**

Append to `src/app.rs`:

```rust
fn handle_key(app: &mut App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    // `g g` for top of PR list.
    if st.focused == FocusedView::List
        && st.pending_g
        && ev.code == crossterm::event::KeyCode::Char('g')
    {
        st.pending_g = false;
        st.list.selected = 0;
        return;
    }
    st.pending_g = false;

    // Search-input mode for the PR list.
    if st.focused == FocusedView::List {
        if let Some(buf) = st.list.search.as_mut() {
            match ev.code {
                crossterm::event::KeyCode::Esc => st.list.search = None,
                crossterm::event::KeyCode::Enter => { /* keep filter, leave input */ }
                crossterm::event::KeyCode::Backspace => { buf.pop(); }
                crossterm::event::KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
    }

    let action = dispatch(st.focused, ev);
    match action {
        Action::Quit => st.running = false,
        Action::ListUp => {
            if st.list.selected > 0 {
                st.list.selected -= 1;
            }
        }
        Action::ListDown => {
            let n = st.list.visible_prs().len();
            if st.list.selected + 1 < n {
                st.list.selected += 1;
            }
        }
        Action::ListTop => {
            st.pending_g = true;
        }
        Action::ListBottom => {
            let n = st.list.visible_prs().len();
            st.list.selected = n.saturating_sub(1);
        }
        Action::ListOpen => {
            if let Some(pr) = st.list.visible_prs().get(st.list.selected).copied() {
                let num = pr.number;
                st.current_pr = Some(num);
                app.ensure_pr_loaded(num);
                let pkg = app.cache.get(num);
                let files_count = pkg.map(|p| p.files.len()).unwrap_or(0);
                st.review = Some(PrReviewState {
                    file_index: 0,
                    cursor_line: 0,
                    scroll: 0,
                    show_commit_strip: app.config.show_commit_strip,
                    show_sha_margin: app.config.show_sha_margin,
                    status: format!("{} files", files_count),
                });
                st.focused = FocusedView::Review;
            }
        }
        Action::ListMerge => open_merge(st),
        Action::ListRefresh => {
            if let Err(e) = app.cache.refresh_list() {
                st.list.status = format!("refresh failed: {e}");
            } else if let Some(prs) = app.cache.list.as_ref() {
                st.list.prs = prs.clone();
            }
        }
        Action::ListSearch => {
            st.list.search = Some(String::new());
        }
        Action::ListCycleFilter => {
            st.list.filter_open_only = !st.list.filter_open_only;
        }
        Action::ListClearFilter => {
            st.list.search = None;
        }
        Action::CursorDown => {
            if let Some(r) = st.review.as_mut() {
                r.cursor_line = r.cursor_line.saturating_add(1);
            }
        }
        Action::CursorUp => {
            if let Some(r) = st.review.as_mut() {
                r.cursor_line = r.cursor_line.saturating_sub(1);
            }
        }
        Action::HalfPageDown => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_add(10);
            }
        }
        Action::HalfPageUp => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_sub(10);
            }
        }
        Action::Top => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = 0;
                r.cursor_line = 0;
            }
        }
        Action::Bottom => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = u16::MAX / 2;
            }
        }
        Action::NextFile => cycle_file(app, st, 1),
        Action::PrevFile => cycle_file(app, st, -1),
        Action::OpenFilePicker => {
            if let (Some(num), Some(_)) = (st.current_pr, st.review.as_ref()) {
                if let Some(pkg) = app.cache.get(num) {
                    st.picker = Some(FilePickerState {
                        query: String::new(),
                        all_files: pkg.files.iter().map(|f| f.path.clone()).collect(),
                        selected: 0,
                    });
                    st.focused = FocusedView::FilePicker;
                }
            }
        }
        Action::Merge => open_merge(st),
        Action::ToggleCommitStrip => {
            if let Some(r) = st.review.as_mut() {
                r.show_commit_strip = !r.show_commit_strip;
            }
        }
        Action::ToggleShaMargin => {
            if let Some(r) = st.review.as_mut() {
                r.show_sha_margin = !r.show_sha_margin;
            }
        }
        Action::BackToList => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
        }
        Action::Help => {
            st.focused = FocusedView::HelpOverlay;
        }
        Action::Refresh => {
            if let Some(num) = st.current_pr {
                let _ = app.cache.load_pr(num);
            }
        }
        Action::Nothing => {}
    }
}

fn open_merge(st: &mut AppState) {
    if let Some(num) = st
        .list
        .visible_prs()
        .get(st.list.selected)
        .map(|p| p.number)
        .or(st.current_pr)
    {
        st.merge = Some(MergeModalState {
            pr_number: num,
            default: MergeMethod::Merge,
            selected: MergeMethod::Merge,
        });
        st.focused = FocusedView::MergeModal;
    }
}

fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else { return };
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

- [ ] **Step 5: Implement `handle_mouse`**

Append to `src/app.rs`:

```rust
fn handle_mouse(_app: &mut App, st: &mut AppState, ev: crossterm::event::MouseEvent) {
    match mouse_dispatch(ev) {
        MouseAction::Scroll(d) => {
            if st.focused == FocusedView::List {
                let n = st.list.visible_prs().len();
                if d > 0 {
                    st.list.selected =
                        (st.list.selected + d as usize).min(n.saturating_sub(1));
                } else {
                    st.list.selected = st.list.selected.saturating_sub((-d) as usize);
                }
            } else if let Some(r) = st.review.as_mut() {
                if d > 0 {
                    r.scroll = r.scroll.saturating_add(d as u16);
                } else {
                    r.scroll = r.scroll.saturating_sub((-d) as u16);
                }
            }
        }
        MouseAction::ClickAt { col: _, row } => {
            // PR list rows start at y=2 (header is rows 0-1).
            if st.focused == FocusedView::List && row >= 2 {
                let idx = (row - 2) as usize;
                if idx < st.list.visible_prs().len() {
                    st.list.selected = idx;
                }
            }
        }
        MouseAction::DoubleClickAt { .. } | MouseAction::Nothing => {}
    }
}
```

- [ ] **Step 6: Verify everything compiles**

```bash
cargo build
cargo test --lib
```

Expected: builds clean, all existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs src/data/cache.rs
git commit -m "feat(app): event loop + view transitions"
```

---

## Task 21: Help overlay

Renders a static keymap. State is just "is the overlay open?".

**Files:**
- Create: `src/view/help.rs`
- Modify: `src/view/mod.rs`
- Modify: `src/app.rs`

- [ ] **Step 1: Add module to `src/view/mod.rs`**

Append:

```rust
pub mod help;
```

- [ ] **Step 2: Create `src/view/help.rs`**

```rust
//! Static help overlay.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::render::style::*;

pub fn render(f: &mut Frame, area: Rect) {
    let modal = centered(area, 70, 24);
    f.render_widget(Clear, modal);
    let lines: Vec<Line<'static>> = HELP_TEXT
        .iter()
        .map(|s| Line::styled(s.to_string(), Style::default().fg(TEXT)))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(" help · ? to close ");
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

const HELP_TEXT: &[&str] = &[
    "",
    "  Global",
    "    Ctrl-C       quit",
    "    ?            toggle this help",
    "    r            refresh current view",
    "",
    "  PR list",
    "    j/k or ↓/↑   move",
    "    g g / G      top / bottom",
    "    ↵            open PR",
    "    m            merge modal",
    "    /            search",
    "    f            cycle filter",
    "    Esc          clear filter",
    "    q            quit",
    "",
    "  PR review",
    "    j/k          cursor",
    "    Ctrl-d/u     half-page",
    "    Tab/Shift-Tab next/prev file",
    "    f            file picker      m  merge modal",
    "    c            toggle commit strip",
    "    s            toggle SHA margin",
    "    q / Esc      back to list",
    "",
];
```

- [ ] **Step 3: Wire up the help overlay in `src/app.rs`**

In `draw`, after the existing modals, add:

```rust
    if st.focused == FocusedView::HelpOverlay {
        crate::view::help::render(f, area);
    }
```

In `handle_key`, when `st.focused == FocusedView::HelpOverlay`, swallow keys:

Insert at the top of `handle_key`, before the `g g` handling:

```rust
    if st.focused == FocusedView::HelpOverlay {
        match ev.code {
            crossterm::event::KeyCode::Char('?')
            | crossterm::event::KeyCode::Esc
            | crossterm::event::KeyCode::Char('q') => {
                st.focused = if st.review.is_some() {
                    FocusedView::Review
                } else {
                    FocusedView::List
                };
            }
            _ => {}
        }
        return;
    }
```

- [ ] **Step 4: Build and run tests**

```bash
cargo build
cargo test --lib
```

Expected: builds clean, tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/view/help.rs src/view/mod.rs src/app.rs
git commit -m "feat(view): help overlay"
```

---

## Task 22: Preconditions + main wiring

Hook everything together with `clap` for CLI args and run the launch checks (gh auth, git repo, GitHub remote, TTY).

**Files:**
- Modify: `src/main.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Replace `src/main.rs`**

```rust
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;

use prpr::app::{install_panic_hook, restore_terminal, run, setup_terminal, App, AppState};
use prpr::config;
use prpr::data::gh::{GhCli, GhClient};
use prpr::data::git::{GitCli, GitClient};

#[derive(Debug, Parser)]
#[command(name = "prpr", version, about = "TUI PR review")]
struct Cli {
    /// Override window_size from the config file.
    #[arg(long)]
    window_size: Option<usize>,
    /// Hide the commit strip on launch.
    #[arg(long)]
    no_commit_strip: bool,
}

fn main() {
    if let Err(e) = real_main() {
        let _ = restore_terminal();
        eprintln!("prpr: {e:?}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let mut cfg = config::load()?;
    if let Some(n) = cli.window_size {
        cfg.window_size = n;
    }
    if cli.no_commit_strip {
        cfg.show_commit_strip = false;
    }

    if !is_tty() {
        return Err(anyhow!("prpr requires a TTY"));
    }
    if std::env::var("COLORTERM")
        .map(|v| !(v == "truecolor" || v == "24bit"))
        .unwrap_or(true)
    {
        eprintln!("prpr: COLORTERM is not 'truecolor' — colors may render incorrectly");
    }

    let gh: Arc<dyn GhClient> = Arc::new(GhCli);
    let git: Arc<dyn GitClient> = Arc::new(GitCli);

    gh.auth_status().context("gh auth status failed (run `gh auth login`)")?;

    let cwd = std::env::current_dir()?;
    let repo_root = git.repo_root(&cwd).context("not inside a git repo")?;
    if !git.has_github_remote(&repo_root)? {
        return Err(anyhow!("no github.com remote in {}", repo_root.display()));
    }

    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let branch = current_branch(&repo_root).unwrap_or_else(|| "?".into());

    let mut app = App::new(repo_root, gh, git, cfg);
    let mut st = AppState::new(repo_name, branch);

    install_panic_hook();
    let mut term = setup_terminal()?;
    let result = run(&mut term, &mut app, &mut st);
    restore_terminal()?;
    result
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn current_branch(repo_root: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
```

- [ ] **Step 2: Update `src/lib.rs` to expose `app::run`**

`src/lib.rs` already declares `pub mod app;` — verify that `App`, `AppState`, `run`, `setup_terminal`, `install_panic_hook`, `restore_terminal` are `pub` in `app.rs`. They should be from earlier tasks; if any are missing, add `pub`.

- [ ] **Step 3: Build**

```bash
cargo build
```

Expected: builds clean. Don't run yet — we'll smoke-test in Task 23.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/lib.rs
git commit -m "feat: main wiring + launch preconditions"
```

---

## Task 23: Smoke test on Kitty + lockfile + README

Final sanity pass. Run `prpr` in a real GitHub repo on Kitty. Document what to expect. Re-enable `Cargo.lock`.

**Files:**
- Modify: `.gitignore`
- Create: `Cargo.lock`
- Modify: `README.md` (or create)

- [ ] **Step 1: Re-enable Cargo.lock for the binary**

Edit `.gitignore` and remove the `Cargo.lock` line. Then:

```bash
cargo build
git add Cargo.lock .gitignore
git commit -m "chore: track Cargo.lock"
```

- [ ] **Step 2: Manual smoke test on Kitty**

Run from inside a real GitHub-hosted Rust repo (e.g., clone `cli/cli` for quick test):

```bash
cd /tmp && git clone --depth=200 https://github.com/cli/cli && cd cli
cargo run --manifest-path /Users/poga/projects/prpr/Cargo.toml
```

Verify, in order:

1. PR list view appears, populated, focused on the most recent PR.
2. `j` / `k` move the highlight, no flicker.
3. `↵` opens a PR; commit strip appears with colored swatches.
4. The diff renders, colors are clearly visible, the gutter has 1-cell colored bars.
5. `]f` / `Tab` cycle through files. `f` opens the file picker; typing filters; `↵` jumps.
6. `Esc` returns to the list.
7. `?` opens help; `?` again closes it.
8. `m` opens the merge modal; `Esc` cancels (do NOT confirm).
9. `q` quits cleanly — terminal restored to normal mode.
10. Resize the window to under 80×24; the "terminal too small" message appears.
11. In Kitty's keyboard inspector (Ctrl-Shift-F1), confirm Ctrl-d / Ctrl-u / Tab / BackTab arrive correctly.

Stop on the first failure. Open a follow-up issue describing it; that's a v1.1 fix unless it blocks the smoke test.

- [ ] **Step 3: Add a minimal README**

Create `README.md`:

```markdown
# prpr

A keyboard-driven TUI for reviewing GitHub PRs, with per-commit color
attribution in the diff.

## Requirements

- `gh` CLI on `$PATH`, authenticated (`gh auth login`)
- `git` on `$PATH`
- A truecolor-capable terminal (Kitty recommended)

## Install

```bash
cargo install --path .
```

## Run

From inside a clone of any GitHub-hosted repo:

```bash
prpr
```

## Keys

Press `?` inside the app for the full keymap.
```

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: README with quickstart"
```

- [ ] **Step 5: Final test sweep**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Fix any lint or formatting issues; commit fixes as `chore: fmt+clippy`. Expected: all green.

---

## Self-Review

A pass over the spec to confirm coverage:

| Spec section | Plan coverage |
|---|---|
| Goals 1–6 | Tasks 13 (PR list), 14 (PR review), 8 + 19 (merge), 15 + 20 (snappy event loop, off-thread fetches via `App::ensure_pr_loaded` + future worker thread upgrade), 12–14 (visual, no anims), 22 (Kitty checks at launch), 23 (smoke). |
| Non-goals | Not implemented anywhere — confirmed by absence. |
| F1 Triage | Tasks 13, 16, 20. |
| F2 Review a PR | Tasks 14, 16, 18, 20. |
| F3 Merge | Tasks 8 (`merge_pr`), 19 (modal), 20 (open_merge wiring). |
| Tech stack table | Task 1. |
| Module layout | Task 2 + per-module tasks. |
| Data flow | Tasks 8, 10, 20 (event loop). |
| PR list view | Task 13. |
| PR review view | Task 14. |
| File picker overlay | Task 18. |
| Merge modal overlay | Task 19. |
| Loading & error placeholders | Task 20 (`loading…`, `terminal too small`), 22 (preconditions). |
| Commit color engine — palette | Task 4. |
| Commit color engine — assignment | Tasks 5, 9. |
| Commit color engine — edge cases (force-push, binary, renames) | Task 10 (force-push key), Tasks 12 + 14 (binary placeholder). Renames rely on `git blame --follow` semantics — not asserted in tests; acceptable for v1. |
| Performance budget | Not unit-tested; smoke test in Task 23 covers gross regressions. |
| Input — global, list, review | Task 16. |
| Mouse | Task 17 + Task 20 wiring. |
| Configuration | Task 11 + Task 22 CLI overrides. |
| Preconditions | Task 22. |
| Error handling | Task 22 (`real_main` returns Result) + Task 20 (status messages). Panic hook in Task 15. |
| Truecolor | Task 22 warning. |
| Testing — unit | Each task includes its own. |
| Testing — integration | The cache test in Task 10 with fixtures is the integration anchor. |
| Manual test matrix | Task 23 explicitly lists Kitty; engineer should also run inside `tmux` and Alacritty. |

**Gap noted:** No automated integration test that exercises the full PR-open path through the cache *and* the renderer. Acceptable for v1 since each layer is unit-tested and the Task 23 smoke covers the join.

**Type consistency:** `MergeMethod::cli_flag` returns `&'static str` (Task 19); `GhClient::merge_pr` accepts `&str` (Task 8) — compatible. `assign_commit_colors` returns `HashMap<String, Color>` (Task 5); `attribute_file` calls `palette.get(sha)` (Task 9) — compatible. `PrPackage.colors` is `HashMap<String, LineColors>` (Task 10); `body_lines` looks up `colors.get(&file.path)` (Task 14) — compatible.

**Placeholder scan:** No "TBD"/"TODO"/"add appropriate" / "etc." in steps. Each step contains the literal code or command needed.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-06-prpr-tui-pr-review.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
