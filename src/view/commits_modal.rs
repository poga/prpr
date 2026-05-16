//! Commits modal: read-only vertical list of the PR's commits.
//!
//! Triggered by `c` from the review view. Display-only — selection is
//! visual; Enter/Esc/c just close.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::attribution::CommitStats;
use crate::render::style::*;

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

    pub fn page_down(&mut self, page: usize) {
        let last = self.rows.len().saturating_sub(1);
        self.selected = (self.selected + page).min(last);
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn to_top(&mut self) {
        self.selected = 0;
    }

    pub fn to_bottom(&mut self) {
        self.selected = self.rows.len().saturating_sub(1);
    }
}

/// Centered ~60% × 60% overlay listing the PR's commits, one per row.
pub fn render(f: &mut Frame, area: Rect, st: &CommitsModalState) {
    let modal = centered(area, 60, 60);
    f.render_widget(Clear, modal);

    let lines: Vec<Line> = st
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let row_style = if i == st.selected {
                Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(vec![
                Span::styled(" █ ", Style::default().fg(r.color)),
                Span::styled(format!("{}  ", r.short_sha), Style::default().fg(SUBTEXT0)),
                Span::styled(truncate(&r.headline, 36), row_style),
                Span::styled(
                    format!("  {} · {}  ", r.author, r.relative_date),
                    Style::default().fg(OVERLAY1),
                ),
                Span::styled(format!("+{}", r.adds), Style::default().fg(DIFF_ADD_FG)),
                Span::raw(" "),
                Span::styled(format!("−{}", r.dels), Style::default().fg(DIFF_DEL_FG)),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(" commits ");
    // -2 strips the top and bottom border rows.
    let visible = modal.height.saturating_sub(2) as usize;
    let scroll_offset = if visible == 0 {
        0
    } else {
        // Pin selected row at the bottom of the viewport once it would scroll off.
        st.selected.saturating_sub(visible.saturating_sub(1))
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll_offset as u16, 0)).block(block), modal);
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = (area.width - w) / 2 + area.x;
    let y = (area.height - h) / 2 + area.y;
    Rect::new(x, y, w, h)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut)
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
    fn page_down_jumps_by_page_size() {
        let mut st = CommitsModalState {
            rows: (0..30).map(|_| dummy_row()).collect(),
            selected: 5,
        };
        st.page_down(10);
        assert_eq!(st.selected, 15);
    }

    #[test]
    fn page_down_clamps_at_bottom() {
        let mut st = CommitsModalState {
            rows: (0..10).map(|_| dummy_row()).collect(),
            selected: 5,
        };
        st.page_down(20);
        assert_eq!(st.selected, 9);
    }

    #[test]
    fn page_up_jumps_by_page_size() {
        let mut st = CommitsModalState {
            rows: (0..30).map(|_| dummy_row()).collect(),
            selected: 25,
        };
        st.page_up(10);
        assert_eq!(st.selected, 15);
    }

    #[test]
    fn page_up_clamps_at_top() {
        let mut st = CommitsModalState {
            rows: (0..30).map(|_| dummy_row()).collect(),
            selected: 3,
        };
        st.page_up(10);
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn to_top_goes_to_first_row() {
        let mut st = CommitsModalState {
            rows: (0..10).map(|_| dummy_row()).collect(),
            selected: 7,
        };
        st.to_top();
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn to_bottom_goes_to_last_row() {
        let mut st = CommitsModalState {
            rows: (0..10).map(|_| dummy_row()).collect(),
            selected: 2,
        };
        st.to_bottom();
        assert_eq!(st.selected, 9);
    }

    #[test]
    fn to_bottom_on_empty_stays_at_zero() {
        let mut st = CommitsModalState { rows: vec![], selected: 0 };
        st.to_bottom();
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

    #[test]
    fn render_draws_one_row_per_commit() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let st = CommitsModalState {
            rows: vec![
                CommitRow {
                    color: Color::Red,
                    short_sha: "abc123".into(),
                    headline: "first commit".into(),
                    author: "alice".into(),
                    relative_date: "3d".into(),
                    adds: 5,
                    dels: 1,
                },
                CommitRow {
                    color: Color::Green,
                    short_sha: "def456".into(),
                    headline: "second commit".into(),
                    author: "bob".into(),
                    relative_date: "2d".into(),
                    adds: 12,
                    dels: 0,
                },
            ],
            selected: 1,
        };

        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();

        let dump: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();

        assert!(dump.contains("abc123"), "missing first sha:\n{dump}");
        assert!(dump.contains("def456"), "missing second sha:\n{dump}");
        assert!(dump.contains("first commit"), "missing first headline:\n{dump}");
        assert!(dump.contains("second commit"), "missing second headline:\n{dump}");
        assert!(dump.contains("alice"), "missing author:\n{dump}");
        assert!(dump.contains("+5"), "missing adds:\n{dump}");
        assert!(dump.contains("commits"), "missing title:\n{dump}");
    }

    fn row_with_sha(sha: &str) -> CommitRow {
        CommitRow {
            color: Color::White,
            short_sha: sha.to_string(),
            headline: "x".into(),
            author: "a".into(),
            relative_date: "1d".into(),
            adds: 0,
            dels: 0,
        }
    }

    #[test]
    fn selected_row_visible_when_past_viewport() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let rows: Vec<CommitRow> = (0..50).map(|i| row_with_sha(&format!("c{:04}", i))).collect();
        let st = CommitsModalState { rows, selected: 40 };

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();

        let dump: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();

        assert!(
            dump.contains("c0040"),
            "selected commit not visible in rendered output:\n{dump}"
        );
    }

    #[test]
    fn render_highlights_selected_row() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use crate::render::style::SURFACE0;

        let st = CommitsModalState {
            rows: vec![dummy_row(), dummy_row()],
            selected: 1,
        };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();

        // Find a row that contains the selected highlight bg. We don't
        // hard-code the row index because the modal is centered.
        let mut found_highlighted = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].style().bg == Some(SURFACE0) {
                    found_highlighted = true;
                }
            }
        }
        assert!(found_highlighted, "no cell with SURFACE0 bg found");
    }
}
