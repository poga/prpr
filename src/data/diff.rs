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
        // Hunk starts at +42; two context lines (42, 43) advance new_ln to 44;
        // three deletes don't advance new_ln, so the first add is at new=44.
        let first_add = sched.lines.iter().find(|l| l.op == DiffOp::Add).unwrap();
        assert_eq!(first_add.old_lineno, None);
        assert_eq!(first_add.new_lineno, Some(44));
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
