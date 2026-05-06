//! PR list view rendering. State is small and self-contained.

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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
                Some(s) => {
                    p.title.to_lowercase().contains(s)
                        || p.author.login.to_lowercase().contains(s)
                }
                None => true,
            })
            .collect()
    }
}

pub fn render(f: &mut Frame, area: Rect, st: &PrListState, now: DateTime<Utc>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(f, chunks[0], st);
    render_rows(f, chunks[1], st, now);
    render_footer(f, chunks[2], st);
}

fn render_header(f: &mut Frame, area: Rect, st: &PrListState) {
    let visible = st.visible_prs();
    let count = visible.iter().filter(|p| p.state == PrState::Open).count();
    let header = format!(
        "  pprr · {} · {} · {} open                                   filter: {}",
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
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 86400 * 14 {
        format!("{}d", secs / 86400)
    } else {
        format!("{}w", secs / (86400 * 7))
    }
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
            repo_name: "pprr".into(),
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
        assert!(line0.contains("pprr"));
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
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }
}
