//! Syntax highlighting for diff bodies.
//!
//! Uses `syntect`'s bundled `base16-mocha.dark` theme — the closest fit to
//! Catppuccin Mocha among the defaults. The `SyntaxSet` and `Theme` are
//! loaded lazily on first call and cached for the rest of the process.
//!
//! GDScript and GDShader are not in syntect's default syntaxes, so minimal
//! `.sublime-syntax` files are bundled in `assets/` and added at startup.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxDefinition, SyntaxSet};

/// Tab stops for diff body rendering. ratatui's text widgets render `\t` as a
/// single cell, so without expansion any tab-indented diff (Go, Makefiles, …)
/// loses its indentation.
const TAB_WIDTH: usize = 4;

const GDSCRIPT_SYNTAX: &str = include_str!("../../assets/gdscript.sublime-syntax");
const GDSHADER_SYNTAX: &str = include_str!("../../assets/gdshader.sublime-syntax");

fn syntax_set() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(|| {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        // Best-effort: if any bundled syntax fails to parse, fall through
        // with the rest rather than crashing.
        for (yaml, name) in [(GDSCRIPT_SYNTAX, "GDScript"), (GDSHADER_SYNTAX, "GDShader")] {
            if let Ok(def) = SyntaxDefinition::load_from_str(yaml, true, Some(name)) {
                builder.add(def);
            }
        }
        builder.build()
    })
}

fn theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-mocha.dark")
            .cloned()
            .unwrap_or_else(|| ts.themes.values().next().unwrap().clone())
    })
}

/// Highlight one line of code. Returns owned Spans (each `Span<'static>`)
/// suitable for assembling a `Line<'static>`. If the language can't be
/// determined or syntect errors out, returns a single un-styled span — the
/// caller falls back to its default color.
pub fn highlight_line(text: &str, ext: &str) -> Vec<Span<'static>> {
    if text.is_empty() {
        return vec![Span::raw(String::new())];
    }
    let ss = syntax_set();
    let theme = theme();
    let syntax = ss
        .find_syntax_by_extension(ext)
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, theme);
    // syntect's parser expects a trailing newline.
    let with_nl = format!("{text}\n");
    let regions = match h.highlight_line(&with_nl, ss) {
        Ok(r) => r,
        Err(_) => return vec![Span::raw(expand_tabs(text, &mut 0))],
    };
    let mut col = 0usize;
    regions
        .into_iter()
        .filter_map(|(style, frag)| {
            let frag = frag.trim_end_matches('\n');
            if frag.is_empty() {
                return None;
            }
            let expanded = expand_tabs(frag, &mut col);
            let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            Some(Span::styled(expanded, Style::default().fg(fg)))
        })
        .collect()
}

/// Replace `\t` with the right number of spaces to advance to the next tab
/// stop, threading the running column across calls so inline tabs still align.
fn expand_tabs(frag: &str, col: &mut usize) -> String {
    if !frag.contains('\t') {
        *col += frag.chars().count();
        return frag.to_string();
    }
    let mut out = String::with_capacity(frag.len());
    for ch in frag.chars() {
        if ch == '\t' {
            let pad = TAB_WIDTH - (*col % TAB_WIDTH);
            for _ in 0..pad {
                out.push(' ');
            }
            *col += pad;
        } else {
            out.push(ch);
            *col += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keyword_gets_a_distinct_color() {
        let spans = highlight_line("fn main() { 42 }", "rs");
        // At minimum, syntect should produce more than one span for tokenized code.
        assert!(spans.len() > 1, "got {} spans: {:?}", spans.len(), spans);
        // All spans together must reproduce the original text.
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "fn main() { 42 }");
    }

    #[test]
    fn unknown_extension_returns_plain_text_spans() {
        let spans = highlight_line("hello world", "xyznotalanguage");
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "hello world");
    }

    #[test]
    fn tab_indent_is_expanded_to_tab_stops() {
        let spans = highlight_line("\tfoo", "rs");
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "    foo");
    }

    #[test]
    fn inline_tab_aligns_to_next_tab_stop() {
        // After "ab" (col 2), a tab should advance to col 4 — i.e. 2 spaces.
        let spans = highlight_line("ab\tcd", "txt");
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "ab  cd");
    }

    #[test]
    fn empty_input_returns_one_empty_span() {
        let spans = highlight_line("", "rs");
        assert_eq!(spans.len(), 1);
        assert!(spans[0].content.is_empty());
    }

    #[test]
    fn gdshader_keyword_gets_a_distinct_color() {
        let spans = highlight_line("uniform vec4 albedo : source_color;", "gdshader");
        assert!(spans.len() > 1, "got {} spans: {:?}", spans.len(), spans);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "uniform vec4 albedo : source_color;");
        let fgs: std::collections::HashSet<_> = spans.iter().map(|s| s.style.fg).collect();
        assert!(fgs.len() > 1, "expected multiple fg colors, got {:?}", fgs);
    }

    #[test]
    fn gdscript_keyword_gets_a_distinct_color() {
        let spans = highlight_line("func _ready() -> void:", "gd");
        assert!(spans.len() > 1, "got {} spans: {:?}", spans.len(), spans);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "func _ready() -> void:");
        // The keyword 'func' must end up styled with a different fg from
        // the function name '_ready' — otherwise the highlighter is just
        // returning one span for the whole line (i.e. plain text).
        let fns: Vec<_> = spans.iter().map(|s| s.style.fg).collect();
        assert!(
            fns.iter().collect::<std::collections::HashSet<_>>().len() > 1,
            "expected multiple distinct foreground colors, got {:?}",
            fns
        );
    }
}
