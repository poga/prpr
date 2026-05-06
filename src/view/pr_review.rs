//! PR review view: header / commit strip / file bar / diff body / status.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::data::cache::PrPackage;
use crate::data::diff::FileDiff;
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
            Constraint::Length(1),       // header
            Constraint::Length(strip_h), // commit strip (0 if hidden)
            Constraint::Length(2),       // file bar (title + divider)
            Constraint::Min(1),          // diff body
            Constraint::Length(3),       // status (cursor + 2 hint rows)
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
        "  pprr · #{} {} · {} · {} ← {}",
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
        let color = palette.get(&c.oid).copied().unwrap_or(OLDER_COMMIT);
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
    f.render_widget(
        Paragraph::new(Line::from(spans)).wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

fn render_file_bar(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let total = pkg.files.len();
    let path = pkg
        .files
        .get(st.file_index)
        .map(|f| f.path.as_str())
        .unwrap_or("");
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
    f.render_widget(
        Paragraph::new(label).style(Style::default().fg(SUBTEXT0)),
        chunks[0],
    );
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

fn body_lines<'a>(file: &'a FileDiff, colors: &'a HashMap<String, LineColors>) -> Vec<Line<'a>> {
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // cursor / status info
            Constraint::Length(1), // hints row 1: scrolling
            Constraint::Length(1), // hints row 2: actions
        ])
        .split(area);

    let cursor_info = if let Some(file) = pkg.files.get(st.file_index) {
        let cursor_lineno = file
            .lines
            .iter()
            .filter(|l| !l.is_hunk_header)
            .nth(st.cursor_line)
            .and_then(|l| l.new_lineno.or(l.old_lineno));
        match cursor_lineno {
            Some(n) => format!("line {n}"),
            None => st.status.clone(),
        }
    } else {
        st.status.clone()
    };
    f.render_widget(
        Paragraph::new(format!("  {cursor_info}")).style(Style::default().fg(SUBTEXT0)),
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
            "  Tab/↵ next file   Shift-Tab prev   f files   m merge   c strip   s sha   ? help   q back",
        )
        .style(Style::default().fg(OVERLAY0)),
        chunks[2],
    );
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn fixture_pkg() -> PrPackage {
        let detail: PrDetail =
            serde_json::from_str(include_str!("../../tests/fixtures/pr_view.json")).unwrap();
        let files = parse_diff(include_str!("../../tests/fixtures/diff_basic.patch")).unwrap();
        PrPackage {
            detail,
            files,
            colors: HashMap::new(),
        }
    }

    fn buffer_line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn renders_pr_number_in_header() {
        let pkg = fixture_pkg();
        let st = PrReviewState {
            show_commit_strip: false,
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let header = buffer_line(buf, 0);
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
        let st = PrReviewState {
            show_commit_strip: false,
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st)
        })
        .unwrap();
        let buf = term.backend().buffer();
        // With strip hidden, body starts at row 3 (header row 0; strip 0 rows;
        // file bar rows 1-2; body rows 3..18; status row 19).
        let body = buffer_line(buf, 3);
        assert!(body.contains("binary file"), "row 3 was: {:?}", body);
    }
}
