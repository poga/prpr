//! Merge modal: pick Merge / Squash / Rebase, confirm with Enter.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::render::style::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn cli_flag(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
    pub fn letter(self) -> char {
        match self {
            MergeMethod::Merge => 'M',
            MergeMethod::Squash => 'S',
            MergeMethod::Rebase => 'R',
        }
    }
}

pub fn from_letter(c: char) -> Option<MergeMethod> {
    match c.to_ascii_uppercase() {
        'M' => Some(MergeMethod::Merge),
        'S' => Some(MergeMethod::Squash),
        'R' => Some(MergeMethod::Rebase),
        _ => None,
    }
}

#[derive(Debug)]
pub struct MergeModalState {
    pub pr_number: u32,
    pub default: MergeMethod,
    pub selected: MergeMethod,
}

pub fn render(f: &mut Frame, area: Rect, st: &MergeModalState) {
    let modal = centered(area, 56, 9);
    f.render_widget(Clear, modal);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    for m in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
        let prefix = format!("   [{}] ", m.letter());
        let label = match m {
            MergeMethod::Merge => "Merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        };
        let mut text = format!("{}{}", prefix, label);
        if m == st.default {
            text.push_str("       (repo default)");
        }
        let style = if m == st.selected {
            Style::default().bg(SURFACE0).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT)
        };
        lines.push(Line::styled(text, style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "   ↵ confirm     letter to pick     Esc cancel".to_string(),
        Style::default().fg(OVERLAY1),
    ));

    let title = format!(" Merge #{}? ", st.pr_number);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(title);
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn letter_mapping_round_trip() {
        for m in [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase] {
            assert_eq!(from_letter(m.letter()), Some(m));
        }
    }

    #[test]
    fn cli_flags_match_gh_options() {
        assert_eq!(MergeMethod::Merge.cli_flag(), "merge");
        assert_eq!(MergeMethod::Squash.cli_flag(), "squash");
        assert_eq!(MergeMethod::Rebase.cli_flag(), "rebase");
    }
}
