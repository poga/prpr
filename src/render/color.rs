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
        std::iter::repeat_n(c, 40).collect()
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
            sha('a'),
            sha('b'),
            sha('c'),
            sha('d'),
            sha('e'),
            sha('f'),
            sha('g'),
            sha('h'),
            sha('i'),
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
