//! Render a single diff line (line number, gutter, op, code) as a ratatui Line.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::data::diff::{DiffLine, DiffOp};
use crate::render::style::*;

pub fn render_line<'a>(
    line: &'a DiffLine,
    head_color: Option<Color>,
    base_color: Option<Color>,
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

    let body_style = match line.op {
        DiffOp::Add => Style::default().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG),
        DiffOp::Delete => Style::default().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG),
        DiffOp::Context => Style::default().fg(TEXT),
        DiffOp::Hunk => unreachable!(),
    };

    Line::from(vec![
        Span::styled(lineno_str, Style::default().fg(OVERLAY0)),
        Span::raw(" "),
        Span::styled(
            gutter_glyph.to_string(),
            gutter_color
                .map(|c| Style::default().fg(c))
                .unwrap_or_default(),
        ),
        Span::raw(" "),
        Span::styled(op_glyph.to_string(), op_style),
        Span::raw(" "),
        Span::styled(line.text.clone(), body_style),
    ])
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
        let rendered = render_line(&line, Some(COMMIT_PALETTE[0]), None);
        // Find the gutter span: it should be "█" with the palette color.
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.content, "█");
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[0]));
    }

    #[test]
    fn add_line_uses_diff_add_styling() {
        let line = add("    let x = 2;", 45);
        let rendered = render_line(&line, Some(COMMIT_PALETTE[1]), None);
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.fg, Some(DIFF_ADD_FG));
        assert_eq!(body.style.bg, Some(DIFF_ADD_BG));
    }

    #[test]
    fn delete_line_uses_base_gutter_color() {
        let line = del("    let x = 1;", 42);
        let rendered = render_line(&line, None, Some(COMMIT_PALETTE[2]));
        let gutter = &rendered.spans[2];
        assert_eq!(gutter.style.fg, Some(COMMIT_PALETTE[2]));
        let body = rendered.spans.last().unwrap();
        assert_eq!(body.style.fg, Some(DIFF_DEL_FG));
    }

    #[test]
    fn missing_color_renders_blank_gutter() {
        let line = ctx("    // ancient code", 1);
        let rendered = render_line(&line, None, None);
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
        let rendered = render_line(&line, None, None);
        assert_eq!(rendered.spans.len(), 1);
        assert!(rendered.spans[0].style.add_modifier.contains(Modifier::DIM));
    }
}
