//! Render a single diff line (line number, gutter, op, code) as a ratatui Line.
//!
//! Code text inside the diff body is syntax-highlighted via `render::syntax`,
//! using the file extension as the language hint. The diff add/remove
//! background tint is layered on top so the line still reads as added/removed
//! while individual tokens get their syntax colors.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::data::diff::{DiffLine, DiffOp};
use crate::render::style::*;
use crate::render::syntax;

pub fn render_line<'a>(
    line: &'a DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
    file_ext: &str,
) -> Line<'a> {
    if line.is_hunk_header {
        return Line::from(vec![Span::styled(
            line.text.clone(),
            Style::default().fg(OVERLAY1).add_modifier(Modifier::DIM),
        )]);
    }

    let lineno_str = match (line.old_lineno, line.new_lineno) {
        (_, Some(n)) => format!("{n:>4}"),
        (Some(n), None) => format!("{n:>4}"),
        (None, None) => "    ".to_string(),
    };

    // Pick the gutter color from head for context/add lines, base for delete.
    let gutter_color = match line.op {
        DiffOp::Add | DiffOp::Context => head_color,
        DiffOp::Delete => base_color,
        DiffOp::Hunk => None,
    };
    let gutter_glyph = if gutter_color.is_some() { "█" } else { " " };

    let (op_glyph, op_style) = match line.op {
        DiffOp::Add => ("+", Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG)),
        DiffOp::Delete => ("-", Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG)),
        DiffOp::Context => (" ", Style::default().fg(SUBTEXT0)),
        DiffOp::Hunk => unreachable!(),
    };

    // Background tint to overlay on every body span, so add/remove rows stay
    // visually distinct even when syntax colors take over the foreground.
    let body_bg: Option<Color> = match line.op {
        DiffOp::Add => Some(DIFF_ADD_BG),
        DiffOp::Delete => Some(DIFF_DEL_BG),
        DiffOp::Context => None,
        DiffOp::Hunk => None,
    };

    let mut highlighted = syntax::highlight_line(&line.text, file_ext);
    if let Some(bg) = body_bg {
        for span in &mut highlighted {
            span.style = span.style.bg(bg);
        }
    }

    let mut spans = Vec::with_capacity(7 + highlighted.len());
    spans.push(Span::styled(lineno_str, Style::default().fg(OVERLAY0)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        gutter_glyph.to_string(),
        gutter_color
            .map(|c| Style::default().fg(c))
            .unwrap_or_default(),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(op_glyph.to_string(), op_style));
    spans.push(Span::raw(" "));
    spans.extend(highlighted);

    Line::from(spans)
}

/// Extract the lowercase extension of a path, or `""` if there is none.
pub fn ext_of(path: &str) -> &str {
    path.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::diff::{DiffLine, DiffOp};
    use pretty_assertions::assert_eq;

    fn ctx(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Context,
            old_lineno: Some(ln),
            new_lineno: Some(ln),
            text: text.into(),
            is_hunk_header: false,
        }
    }
    fn add(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Add,
            old_lineno: None,
            new_lineno: Some(ln),
            text: text.into(),
            is_hunk_header: false,
        }
    }
    fn del(text: &str, ln: u32) -> DiffLine {
        DiffLine {
            op: DiffOp::Delete,
            old_lineno: Some(ln),
            new_lineno: None,
            text: text.into(),
            is_hunk_header: false,
        }
    }

    #[test]
    fn context_line_uses_head_gutter_color() {
        let line = ctx("    let x = 1;", 42);
        let rendered = render_line(&line, Some(COMMIT_PALETTE[0]), None, "rs");
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.content, "█");
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn add_line_keeps_diff_add_background_under_syntax_fg() {
        let line = add("    let x = 2;", 45);
        let rendered = render_line(&line, Some(COMMIT_PALETTE[1]), None, "rs");
        // The last span is a syntax-highlighted body span; its background
        // must still be DIFF_ADD_BG so the row reads as "added".
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.bg, Some(DIFF_ADD_BG));
    }

    #[test]
    fn delete_line_uses_base_gutter_color_and_remove_bg() {
        let line = del("    let x = 1;", 42);
        let rendered = render_line(&line, None, Some(COMMIT_PALETTE[2]), "rs");
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[2]));
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.bg, Some(DIFF_DEL_BG));
    }

    #[test]
    fn missing_color_renders_blank_gutter() {
        let line = ctx("    // ancient code", 1);
        let rendered = render_line(&line, None, None, "rs");
        assert_eq!(rendered.spans[2].content, " ");
    }

    #[test]
    fn hunk_header_renders_dim() {
        let line = DiffLine {
            op: DiffOp::Hunk,
            old_lineno: None,
            new_lineno: None,
            text: "@@ -42,7 +42,11 @@".into(),
            is_hunk_header: true,
        };
        let rendered = render_line(&line, None, None, "rs");
        assert_eq!(rendered.spans.len(), 1);
        assert!(rendered.spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn tab_indent_expands_to_tab_stop() {
        // ratatui renders `\t` as a single cell, so without expansion any
        // tab-indented language (Go, Makefile, …) reads as un-indented.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::Paragraph;

        let line = add("\tlet x = 2;", 45);
        let rendered = render_line(&line, None, None, "rs");
        let mut term = Terminal::new(TestBackend::new(40, 1)).unwrap();
        term.draw(|f| f.render_widget(Paragraph::new(vec![rendered.clone()]), f.area()))
            .unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..buf.area.width)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect();
        // Layout: "  45" + " " + gutter(" ") + " " + "+" + " " + body.
        // Body must start with at least four cells of indent before "let".
        assert!(
            row.contains("+     let"),
            "tab-indented body did not expand to a 4-wide tab stop: {row:?}"
        );
    }

    #[test]
    fn ext_of_strips_path_and_dot() {
        assert_eq!(ext_of("src/lib.rs"), "rs");
        assert_eq!(ext_of("README.md"), "md");
        assert_eq!(ext_of("Makefile"), "");
        assert_eq!(ext_of("a/b/c.tar.gz"), "gz");
    }
}
