//! PR review view: header / commit strip / file bar / diff body / status.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::data::cache::PrPackage;
use crate::data::diff::FileDiff;
use crate::render::attribution::LineColors;
use crate::render::diff::{ext_of, render_line};
use crate::render::style::*;

#[derive(Debug, Default)]
pub struct PrReviewState {
    pub file_index: usize,
    pub cursor_line: usize,
    pub scroll: u16,
    pub show_sha_margin: bool,
    pub status: String,
}

pub fn render(f: &mut Frame, area: Rect, pkg: &PrPackage, st: &PrReviewState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(2), // file bar (title + divider)
            Constraint::Min(1),    // diff body
            Constraint::Length(3), // status (cursor + 2 hint rows)
        ])
        .split(area);

    render_header(f, chunks[0], pkg);
    render_file_bar(f, chunks[1], pkg, st);
    render_diff_body(f, chunks[2], pkg, st);
    render_status(f, chunks[3], pkg, st);
}

fn render_header(f: &mut Frame, area: Rect, pkg: &PrPackage) {
    let d = &pkg.detail;
    let header = format!(
        "  prpr · #{} {} · {} · {} ← {}",
        d.number, d.title, d.author.login, d.base_ref_name, d.head_ref_name,
    );
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(TEXT)),
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
    let ext = ext_of(&file.path);
    file.lines
        .iter()
        .map(|l| {
            let head = l.new_lineno.and_then(|n| {
                lookup
                    .and_then(|lc| lc.head.get(n.saturating_sub(1) as usize).copied())
                    .flatten()
            });
            // Delete lines are looked up by text content — the same line
            // text might appear at different positions in different commits,
            // but `git log -p` records exactly which PR commit's patch
            // removed each unique text. See data::log_patches.
            let base = if l.op == crate::data::diff::DiffOp::Delete {
                lookup.and_then(|lc| lc.delete.get(&l.text).copied())
            } else {
                None
            };
            render_line(l, head, base, ext)
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
            "  Tab/↵ next file   Shift-Tab prev   f files   c commits   m merge   s sha   ? help   q back",
        )
        .style(Style::default().fg(OVERLAY0)),
        chunks[2],
    );
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
            commit_stats: HashMap::new(),
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
        let st = PrReviewState::default();
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
    fn renders_no_commit_strip() {
        let pkg = fixture_pkg();
        let st = PrReviewState::default();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st);
        })
        .unwrap();
        let buf = term.backend().buffer();
        // No row should render the old "commits  " label.
        for y in 0..buf.area.height {
            let row = buffer_line(buf, y);
            assert!(
                !row.starts_with("  commits  "),
                "row {y} unexpectedly rendered the commit strip: {row:?}",
            );
        }
    }

    #[test]
    fn binary_file_renders_placeholder() {
        let mut pkg = fixture_pkg();
        pkg.files = vec![FileDiff {
            path: "img.png".into(),
            lines: vec![],
            binary: true,
        }];
        let st = PrReviewState::default();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &pkg, &st)
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Body starts at row 3 (header row 0; file bar rows 1-2;
        // body rows 3..18; status row 19).
        let body = buffer_line(buf, 3);
        assert!(body.contains("binary file"), "row 3 was: {:?}", body);
    }
}
