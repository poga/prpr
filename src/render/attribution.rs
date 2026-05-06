//! End-to-end commit attribution: produces the line→color map a renderer needs.

use std::collections::HashMap;

use ratatui::style::Color;

use crate::data::blame::Blame;
use crate::render::color::assign_commit_colors;
use crate::render::style::OLDER_COMMIT;

/// One file's worth of attribution.
///
/// `head` is indexed by `head_lineno - 1` — the line numbers in the PR's
/// head version of the file, used for context and added lines.
///
/// `delete` maps the literal text of a removed line to the PR commit that
/// removed it. Deleted lines are matched by content, not line number, since
/// line numbers shift across commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineColors {
    pub head: Vec<Option<Color>>,
    pub delete: HashMap<String, Color>,
}

/// Build the color lookup for one file.
///
/// `head_blame` is `git blame --porcelain <head>` output (already parsed).
/// `delete_text_to_sha` comes from walking the PR commits' patches and
/// recording, for each `-` line, the commit SHA that removed it.
pub fn attribute_file(
    commits: &[String],
    window_size: usize,
    head_blame: &Blame,
    delete_text_to_sha: &HashMap<String, String>,
) -> LineColors {
    let palette = assign_commit_colors(commits, window_size);

    let head: Vec<Option<Color>> = head_blame
        .line_shas
        .iter()
        .map(|sha| {
            if sha.is_empty() {
                None
            } else {
                Some(palette.get(sha).copied().unwrap_or(OLDER_COMMIT))
            }
        })
        .collect();

    let delete: HashMap<String, Color> = delete_text_to_sha
        .iter()
        .map(|(text, sha)| {
            let color = palette.get(sha).copied().unwrap_or(OLDER_COMMIT);
            (text.clone(), color)
        })
        .collect();

    LineColors { head, delete }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::style::COMMIT_PALETTE;
    use pretty_assertions::assert_eq;

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    #[test]
    fn maps_blame_to_palette_colors() {
        let commits = vec![sha('a'), sha('b'), sha('c')];
        let head_blame = Blame {
            line_shas: vec![sha('a'), sha('b'), sha('c'), sha('a')],
        };
        let colors = attribute_file(&commits, 7, &head_blame, &HashMap::new());
        assert_eq!(colors.head[0], Some(COMMIT_PALETTE[0]));
        assert_eq!(colors.head[1], Some(COMMIT_PALETTE[1]));
        assert_eq!(colors.head[2], Some(COMMIT_PALETTE[2]));
        assert_eq!(colors.head[3], Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn lines_from_pre_pr_commits_get_older_gray() {
        let commits = vec![sha('a')];
        let head_blame = Blame {
            line_shas: vec![sha('z')],
        };
        let colors = attribute_file(&commits, 7, &head_blame, &HashMap::new());
        assert_eq!(colors.head[0], Some(OLDER_COMMIT));
    }

    #[test]
    fn empty_sha_means_no_color() {
        let commits = vec![sha('a')];
        let head_blame = Blame {
            line_shas: vec![String::new(), sha('a')],
        };
        let colors = attribute_file(&commits, 7, &head_blame, &HashMap::new());
        assert_eq!(colors.head[0], None);
        assert_eq!(colors.head[1], Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn deletion_text_maps_to_owning_commit_color() {
        let commits = vec![sha('a'), sha('b')];
        let head_blame = Blame { line_shas: vec![] };
        let mut deletes = HashMap::new();
        deletes.insert("removed by a".to_string(), sha('a'));
        deletes.insert("removed by b".to_string(), sha('b'));
        let colors = attribute_file(&commits, 7, &head_blame, &deletes);
        assert_eq!(
            colors.delete.get("removed by a").copied(),
            Some(COMMIT_PALETTE[0])
        );
        assert_eq!(
            colors.delete.get("removed by b").copied(),
            Some(COMMIT_PALETTE[1])
        );
    }

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
}
