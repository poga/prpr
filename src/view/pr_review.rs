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
use crate::render::diff::{ext_of, render_line_with_spans};
use crate::render::style::*;

#[derive(Debug, Default)]
pub struct PrReviewState {
    // Data owned by the review pane (populated by worker responses).
    pub detail: Option<PrDetail>,
    pub files: Vec<FileDiff>,
    pub colors: HashMap<String, ColorState>,
    pub commit_stats: HashMap<String, CommitStats>,
    /// Memoized syntax spans keyed by (file_index, line index). Cleared
    /// whenever `files` is replaced.
    pub syntax_cache: HashMap<(usize, usize), Vec<Span<'static>>>,

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

pub fn render(f: &mut Frame, area: Rect, st: &mut PrReviewState) {
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

fn render_diff_body(f: &mut Frame, area: Rect, st: &mut PrReviewState) {
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
    let file_index = st.file_index;
    let scroll = st.scroll as usize;
    let PrReviewState { files, colors, syntax_cache, .. } = st;
    let Some(file) = files.get(file_index) else {
        return;
    };
    if file.binary {
        f.render_widget(
            Paragraph::new("  binary file, not displayed").style(Style::default().fg(OVERLAY1)),
            area,
        );
        return;
    }
    let lines =
        visible_body_lines(file, file_index, colors, syntax_cache, scroll, area.height as usize);
    f.render_widget(Paragraph::new(lines), area);
}

/// Rows for the visible window only — frame cost scales with screen height,
/// not file size. Syntax spans are memoized per line in `cache`.
fn visible_body_lines(
    file: &FileDiff,
    file_index: usize,
    colors: &HashMap<String, ColorState>,
    cache: &mut HashMap<(usize, usize), Vec<Span<'static>>>,
    scroll: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let lookup = colors.get(&file.path).and_then(|c| match c {
        ColorState::Ready(lc) => Some(lc),
        ColorState::Loading => None,
    });
    let ext = ext_of(&file.path);
    let start = scroll.min(file.lines.len());
    let end = (start + height).min(file.lines.len());
    file.lines[start..end]
        .iter()
        .enumerate()
        .map(|(off, l)| {
            let idx = start + off;
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
            if l.is_hunk_header {
                return render_line_with_spans(l, head, base, vec![]);
            }
            let spans = cache
                .entry((file_index, idx))
                .or_insert_with(|| crate::render::syntax::highlight_line(&l.text, ext))
                .clone();
            render_line_with_spans(l, head, base, spans)
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
            "  Tab/↵ next file   Shift-Tab prev   f files   c commits   m merge   d draft   s sha   ? help   q back",
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
            syntax_cache: HashMap::new(),
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

    fn big_file(n: u32) -> FileDiff {
        let lines = (1..=n)
            .map(|i| crate::data::diff::DiffLine {
                op: crate::data::diff::DiffOp::Context,
                old_lineno: Some(i),
                new_lineno: Some(i),
                text: format!("let x{i} = {i};"),
                is_hunk_header: false,
            })
            .collect();
        FileDiff { path: "src/big.rs".into(), lines, binary: false }
    }

    #[test]
    fn scrolled_body_starts_at_the_scroll_offset() {
        let mut r = fixture_review_state();
        r.files = vec![big_file(200)];
        r.scroll = 50;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        let buf = term.backend().buffer();
        // Layout rows: 0 header, 1 spacer, 2-3 file bar, 4.. body.
        let first_body = buffer_line(buf, 4);
        assert!(
            first_body.contains("let x51 = 51;"),
            "body must start at line index 50, got: {first_body:?}"
        );
    }

    #[test]
    fn render_highlights_only_the_visible_window() {
        let mut r = fixture_review_state();
        r.files = vec![big_file(500)];
        r.scroll = 0;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        // Body height is 24 - 4 (header/bar) - 3 (status) = 17 rows.
        assert!(
            !r.syntax_cache.is_empty() && r.syntax_cache.len() <= 17,
            "cache must cover exactly the visible window, got {} entries",
            r.syntax_cache.len()
        );
        assert!(r.syntax_cache.keys().all(|(_, idx)| *idx < 17));

        // Scrolling exposes new lines; already-seen ones are not redone.
        r.scroll = 100;
        let before = r.syntax_cache.len();
        term.draw(|f| render(f, f.area(), &mut r)).unwrap();
        assert!(r.syntax_cache.len() <= before + 17);
        assert!(r.syntax_cache.contains_key(&(0, 100)));
    }

    #[test]
    fn renders_pr_number_in_header() {
        let mut r = fixture_review_state();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &mut r)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let header = buffer_line(buf, 0);
        assert!(header.contains("#482"));
        assert!(header.contains("fix-race"));
    }

    #[test]
    fn renders_no_commit_strip() {
        let mut r = fixture_review_state();
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &mut r);
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
            render(f, area, &mut r);
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
            render(f, area, &mut r);
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
            render(f, area, &mut r)
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
            render(f, area, &mut r)
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
            render(f, area, &mut r)
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
