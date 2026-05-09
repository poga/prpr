//! File picker overlay (Esc/Enter handled by the app loop).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::style::*;

#[derive(Debug, Default)]
pub struct FilePickerState {
    pub query: String,
    pub all_files: Vec<String>,
    pub selected: usize,
}

impl FilePickerState {
    pub fn matches(&self) -> Vec<&String> {
        let q = self.query.to_lowercase();
        let mut scored: Vec<(i64, &String)> = self
            .all_files
            .iter()
            .filter_map(|f| {
                if q.is_empty() {
                    Some((0, f))
                } else {
                    score(&q, &f.to_lowercase()).map(|s| (s, f))
                }
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        scored.into_iter().map(|(_, f)| f).collect()
    }
}

fn score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let pos = candidate.find(query)?;
    let mut s: i64 = 100 - (pos as i64);
    s += if pos == 0 { 50 } else { 0 };
    s -= candidate.len() as i64 / 8;
    Some(s)
}

/// Overlay sized to ~60% of the area, centered.
pub fn render(f: &mut Frame, area: Rect, st: &FilePickerState) {
    let modal = centered(area, 60, 60);
    f.render_widget(Clear, modal);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(modal);

    let query = Paragraph::new(format!("> {}", st.query))
        .style(Style::default().fg(TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(SURFACE2))
                .title(" file "),
        );
    f.render_widget(query, chunks[0]);

    let matches = st.matches();
    let list_lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let style = if i == st.selected {
                Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(vec![Span::styled(format!("  {}", p), style)])
        })
        .collect();
    // -2 strips the top and bottom border rows.
    let visible = chunks[1].height.saturating_sub(2) as usize;
    let scroll_offset = if visible == 0 {
        0
    } else {
        // Pin selected row at the bottom of the viewport once it would scroll off.
        st.selected.saturating_sub(visible.saturating_sub(1))
    };
    let list = Paragraph::new(list_lines)
        .scroll((scroll_offset as u16, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(SURFACE2)),
        );
    f.render_widget(list, chunks[1]);
}

fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = area.width * pct_w / 100;
    let h = area.height * pct_h / 100;
    let x = (area.width - w) / 2 + area.x;
    let y = (area.height - h) / 2 + area.y;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn st_with(files: &[&str], query: &str) -> FilePickerState {
        FilePickerState {
            query: query.into(),
            all_files: files.iter().map(|s| s.to_string()).collect(),
            selected: 0,
        }
    }

    #[test]
    fn empty_query_keeps_input_order() {
        let st = st_with(&["src/main.rs", "src/lib.rs", "README.md"], "");
        let m = st.matches();
        let names: Vec<_> = m.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, vec!["README.md", "src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn substring_query_filters_and_ranks() {
        let st = st_with(
            &["src/main.rs", "src/lib.rs", "README.md", "tests/main.rs"],
            "main",
        );
        let m = st.matches();
        let names: Vec<_> = m.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, vec!["src/main.rs", "tests/main.rs"]);
    }

    #[test]
    fn selected_row_visible_when_past_viewport() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let files: Vec<String> = (0..50).map(|i| format!("dir/file_{:02}.rs", i)).collect();
        let st = FilePickerState {
            query: String::new(),
            all_files: files,
            selected: 40,
        };

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
            dump.contains("dir/file_40.rs"),
            "selected file not visible in rendered output:\n{dump}"
        );
    }
}
