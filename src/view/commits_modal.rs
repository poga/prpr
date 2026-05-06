//! Commits modal: read-only vertical list of the PR's commits.
//!
//! Triggered by `c` from the review view. Display-only — selection is
//! visual; Enter/Esc/c just close.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::style::Color;

use crate::render::attribution::CommitStats;

#[derive(Debug, Clone)]
pub struct CommitRow {
    pub color: Color,
    pub short_sha: String,
    pub headline: String,
    pub author: String,
    pub relative_date: String,
    pub adds: u32,
    pub dels: u32,
}

#[derive(Debug, Default)]
pub struct CommitsModalState {
    pub rows: Vec<CommitRow>,
    pub selected: usize,
}

impl CommitsModalState {
    pub fn move_down(&mut self) {
        let last = self.rows.len().saturating_sub(1);
        if self.selected < last {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Build modal rows from PR detail + cached stats. The palette is built
/// the same way the diff body does (`assign_commit_colors`).
pub fn build_rows(
    pr_commits: &[crate::data::pr::Commit],
    stats: &HashMap<String, CommitStats>,
    palette_window: usize,
    now: DateTime<Utc>,
) -> Vec<CommitRow> {
    let oids: Vec<String> = pr_commits.iter().map(|c| c.oid.clone()).collect();
    let palette = crate::render::color::assign_commit_colors(&oids, palette_window);
    pr_commits
        .iter()
        .map(|c| {
            let s = stats.get(&c.oid).copied().unwrap_or_default();
            CommitRow {
                color: palette
                    .get(&c.oid)
                    .copied()
                    .unwrap_or(crate::render::style::OLDER_COMMIT),
                short_sha: c.oid.chars().take(6).collect(),
                headline: c.message_headline.clone(),
                author: c
                    .authors
                    .first()
                    .map(|a| a.login.clone())
                    .unwrap_or_default(),
                relative_date: relative_date(now, c.committed_date),
                adds: s.adds,
                dels: s.dels,
            }
        })
        .collect()
}

/// Format a commit date as a short relative string. Returns "—" for None.
pub fn relative_date(now: DateTime<Utc>, then: Option<DateTime<Utc>>) -> String {
    let Some(then) = then else {
        return "\u{2014}".into();
    };
    let secs = now.signed_duration_since(then).num_seconds();
    if secs < 60 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format!("{weeks}w");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo");
    }
    let years = days / 365;
    format!("{years}y")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn t(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[test]
    fn relative_date_buckets() {
        let now = t(2026, 5, 6, 12);
        assert_eq!(relative_date(now, None), "\u{2014}");
        assert_eq!(relative_date(now, Some(now)), "just now");
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::minutes(5))),
            "5m"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::hours(2))),
            "2h"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(3))),
            "3d"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(14))),
            "2w"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(60))),
            "2mo"
        );
        assert_eq!(
            relative_date(now, Some(now - chrono::Duration::days(800))),
            "2y"
        );
    }

    #[test]
    fn move_down_clamps_at_bottom() {
        let mut st = CommitsModalState {
            rows: vec![
                dummy_row(),
                dummy_row(),
                dummy_row(),
            ],
            selected: 2,
        };
        st.move_down();
        assert_eq!(st.selected, 2);
    }

    #[test]
    fn move_up_clamps_at_top() {
        let mut st = CommitsModalState {
            rows: vec![dummy_row()],
            selected: 0,
        };
        st.move_up();
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn move_up_and_down_in_middle() {
        let mut st = CommitsModalState {
            rows: vec![dummy_row(), dummy_row(), dummy_row()],
            selected: 1,
        };
        st.move_down();
        assert_eq!(st.selected, 2);
        st.move_up();
        st.move_up();
        assert_eq!(st.selected, 0);
    }

    fn dummy_row() -> CommitRow {
        CommitRow {
            color: Color::White,
            short_sha: "abc123".into(),
            headline: "x".into(),
            author: "a".into(),
            relative_date: "1d".into(),
            adds: 0,
            dels: 0,
        }
    }
}
