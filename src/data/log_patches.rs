//! Parser for `git log --reverse --pretty=format:prpr-commit %H -p <range> -- <file>`.
//!
//! Each PR commit's patch is sandwiched between a `prpr-commit <sha>` marker
//! and the next marker (or EOF). For each `-` line in any patch, we record
//! `(text → sha)` so the renderer can color a deleted line by the commit
//! that actually removed it.

use std::collections::HashMap;

/// Parse the raw stdout into a `(line_text → most_recent_sha_that_removed_it)`
/// map. Later commits override earlier ones for the same text — the typical
/// case is a line being removed exactly once, but a line that is removed,
/// re-added, and removed again gets attributed to the most recent removal.
pub fn parse_deletions(input: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut current_sha = String::new();
    let mut in_hunk = false;

    for line in input.split('\n') {
        if let Some(sha) = line.strip_prefix("prpr-commit ") {
            current_sha = sha.trim().to_string();
            in_hunk = false;
        } else if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk
            && line.starts_with('-')
            && !line.starts_with("--- ")
            && !current_sha.is_empty()
        {
            let text = line[1..].to_string();
            out.insert(text, current_sha.clone());
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_one_commit_with_two_deletions() {
        let input = "\
prpr-commit aaaa
diff --git a/src/sched.rs b/src/sched.rs
--- a/src/sched.rs
+++ b/src/sched.rs
@@ -1,3 +1,3 @@
 ctx
-old line A
-old line B
+new line
";
        let map = parse_deletions(input);
        assert_eq!(map.get("old line A"), Some(&"aaaa".to_string()));
        assert_eq!(map.get("old line B"), Some(&"aaaa".to_string()));
    }

    #[test]
    fn later_commit_overrides_earlier_for_same_text() {
        let input = "\
prpr-commit aaaa
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-foo
+bar
prpr-commit bbbb
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-foo
+baz
";
        let map = parse_deletions(input);
        assert_eq!(map.get("foo"), Some(&"bbbb".to_string()));
    }

    #[test]
    fn ignores_file_header_dashes() {
        // The `--- a/path` line starts with three dashes — must not be
        // treated as a deletion.
        let input = "\
prpr-commit cccc
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-real delete
+add
";
        let map = parse_deletions(input);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("real delete"));
    }

    #[test]
    fn empty_input_returns_empty_map() {
        assert!(parse_deletions("").is_empty());
    }
}
