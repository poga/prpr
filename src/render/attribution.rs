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
