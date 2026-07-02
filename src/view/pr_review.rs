//! PR review view: header / commit strip / file bar / diff body / status.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::data::diff::FileDiff;
use crate::data::pr::PrDetail;
use crate::render::attribution::{CommitStats, LineColors};
use crate::render::diff::{ext_of, render_line};
use crate::render::style::*;

#[derive(Debug, Default)]
pub struct PrReviewState {
    // Data owned by the review pane (populated by worker responses).
    pub detail: Option<PrDetail>,
    pub files: Vec<FileDiff>,
    pub colors: HashMap<String, ColorState>,
    pub commit_stats: HashMap<String, CommitStats>,

    // View state.
    pub file_index: usize,
    pub cursor_line: usize,
    pub scroll: u16,
    pub show_sha_margin: bool,
    pub status: String,
}

#[derive(Debug, Clone)]
pub enum ColorState {
    Loading,
    Ready(LineColors),
}

pub fn render(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(f, chunks[0], st);
    render_file_bar(f, chunks[2], st);
    render_diff_body(f, chunks[3], st);
    render_status(f, chunks[4], st);
}

fn render_header(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let header = match &st.detail {
        Some(d) => format!(
            "  prpr · #{} {} · {} · {} ← {}{}",
            d.number,
            d.title,
            d.author.login,
            d.base_ref_name,
            d.head_ref_name,
            if d.is_draft { " · draft" } else { "" },
        ),
        None => "  prpr · loading…".to_string(),
    };
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(TEXT)),
        area,
    );
}

fn render_file_bar(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let paths = file_paths(st);
    let total = paths.len();
    let path = paths.get(st.file_index).copied().unwrap_or("");
    let counter = format!("file {}/{}", st.file_index + 1, total.max(1));
    let pad = 40_usize.saturating_sub(path.len()) + 46;
    let line = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            path.to_string(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(pad)),
        Span::styled(counter, Style::default().fg(SUBTEXT0)),
    ]);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(line), chunks[0]);
    f.render_widget(
        Paragraph::new("  ".to_string() + &"─".repeat((area.width as usize).saturating_sub(2)))
            .style(Style::default().fg(SURFACE2)),
        chunks[1],
    );
}

pub fn file_paths(st: &PrReviewState) -> Vec<&str> {
    if st.files.is_empty() {
        st.detail
            .as_ref()
            .map(|d| d.files.iter().map(|f| f.path.as_str()).collect())
            .unwrap_or_default()
    } else {
        st.files.iter().map(|f| f.path.as_str()).collect()
    }
}

pub fn file_count(st: &PrReviewState) -> usize {
    if st.files.is_empty() {
        st.detail.as_ref().map(|d| d.files.len()).unwrap_or(0)
    } else {
        st.files.len()
    }
}

fn render_diff_body(f: &mut Frame, area: Rect, st: &PrReviewState) {
    if st.files.is_empty() {
        f.render_widget(
            Paragraph::new(format!(
                "  {} loading diff…",
                crate::render::spinner::glyph()
            ))
            .style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let Some(file) = st.files.get(st.file_index) else {
        return;
    };
    if file.binary {
        f.render_widget(
            Paragraph::new("  binary file, not displayed").style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let lines = body_lines(file, &st.colors);
    f.render_widget(Paragraph::new(lines).scroll((st.scroll, 0)), area);
}

fn body_lines<'a>(file: &'a FileDiff, colors: &'a HashMap<String, ColorState>) -> Vec<Line<'a>> {
    let lookup = colors.get(&file.path).and_then(|c| match c {
        ColorState::Ready(lc) => Some(lc),
        ColorState::Loading => None,
    });
    let ext = ext_of(&file.path);
    file.lines
        .iter()
        .map(|l| {
            let head = l.new_lineno.and_then(|n| {
                lookup
                    .and_then(|lc| lc.head.get(n.saturating_sub(1) as usize).copied())
                    .flatten()
            });
            let base = if l.op == crate::data::diff::DiffOp::Delete {
                lookup.and_then(|lc| lc.delete.get(&l.text).copied())
            } else {
                None
            };
            render_line(l, head, base, ext)
        })
        .collect()
}

fn render_status(f: &mut Frame, area: Rect, st: &PrReviewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let cursor_info = st
        .files
        .get(st.file_index)
        .and_then(|file| {
            file.lines
                .iter()
                .filter(|l| !l.is_hunk_header)
                .nth(st.cursor_line)
                .and_then(|l| l.new_lineno.or(l.old_lineno))
        })
        .map(|n| format!("line {n}"))
        .unwrap_or_default();
    let status_text = if crate::render::spinner::looks_in_progress(&st.status) {
        format!("{} {}", crate::render::spinner::glyph(), st.status)
    } else if cursor_info.is_empty() {
        st.status.clone()
    } else {
        String::new()
    };
    let line = match (cursor_info.is_empty(), status_text.is_empty()) {
        (true, true) => String::new(),
        (false, true) => cursor_info,
        (true, false) => status_text,
        (false, false) => format!("{cursor_info}    {status_text}"),
    };
    f.render_widget(
        Paragraph::new(format!("  {line}")).style(Style::default().fg(SUBTEXT0)),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(
            "  j/k or ↑/↓ scroll   Ctrl-d/u half-page   PgUp/PgDn page   Home/End top/bottom",
        )
        .style(Style::default().fg(OVERLAY1)),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(
            "  Tab/↵ next file   Shift-Tab prev   f files   c commits   m merge   s sha   ? help   q back",
        )
        .style(Style::default().fg(OVERLAY0)),
        chunks[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::diff::parse_diff;
    use crate::data::pr::PrDetail;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn fixture_review_state() -> PrReviewState {
        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let files = parse_diff(include_str!("../../tests/fixtures/diff_basic.patch")).unwrap();
        PrReviewState {
            detail: Some(detail),
            files,
            colors: HashMap::new(),
            commit_stats: HashMap::new(),
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
        }
    }

    fn buffer_line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn renders_pr_number_in_header() {
        let r = fixture_review_state();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let header = buffer_line(buf, 0);
        assert!(header.contains("#482"));
        assert!(header.contains("fix-race"));
    }

    #[test]
    fn renders_no_commit_strip() {
        let r = fixture_review_state();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r);
        })
        .unwrap();
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let row = buffer_line(buf, y);
            assert!(
                !row.starts_with("  commits  "),
                "row {y} unexpectedly rendered the commit strip: {row:?}",
            );
        }
    }

    #[test]
    fn file_bar_uses_detail_files_when_files_not_yet_parsed() {
        let mut r = fixture_review_state();
        let detail_file_count = r.detail.as_ref().unwrap().files.len();
        r.files = vec![];
        let mut term = Terminal::new(TestBackend::new(120, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bar = buffer_line(buf, 2);
        assert!(bar.contains("src/sched.rs"), "bar was: {bar:?}");
        assert!(bar.contains(&format!("file 1/{detail_file_count}")), "bar was: {bar:?}");
    }

    #[test]
    fn diff_body_shows_loading_when_files_not_yet_parsed() {
        let mut r = fixture_review_state();
        r.files = vec![];
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let body = buffer_line(buf, 4);
        assert!(body.contains("loading diff"), "body was: {body:?}");
    }

    #[test]
    fn binary_file_renders_placeholder() {
        let mut r = fixture_review_state();
        r.files = vec![FileDiff {
            path: "img.png".into(),
            lines: vec![],
            binary: true,
        }];
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let body = buffer_line(buf, 4);
        assert!(body.contains("binary file"), "row 4 was: {:?}", body);
    }

    #[test]
    fn header_shows_draft_marker_when_draft() {
        let mut r = fixture_review_state();
        r.detail.as_mut().unwrap().is_draft = true;
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r)
        })
        .unwrap();
        let header = buffer_line(term.backend().buffer(), 0);
        assert!(header.contains("· draft"), "expected draft marker, got {header:?}");
    }

    #[test]
    fn header_hides_draft_marker_when_ready() {
        let mut r = fixture_review_state();
        r.detail.as_mut().unwrap().is_draft = false;
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &r)
        })
        .unwrap();
        let header = buffer_line(term.backend().buffer(), 0);
        assert!(!header.contains("· draft"), "ready PR must not show marker, got {header:?}");
    }

    #[test]
    fn pr_review_state_default_has_empty_data_fields() {
        let st = PrReviewState::default();
        assert!(st.detail.is_none());
        assert!(st.files.is_empty());
        assert!(st.colors.is_empty());
        assert!(st.commit_stats.is_empty());
    }
}
