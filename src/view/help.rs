//! Static help overlay.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::render::style::*;

pub fn render(f: &mut Frame, area: Rect) {
    let modal = centered(area, 70, 24);
    f.render_widget(Clear, modal);
    let lines: Vec<Line<'static>> = HELP_TEXT
        .iter()
        .map(|s| Line::styled(s.to_string(), Style::default().fg(TEXT)))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(SURFACE2))
        .title(" help · ? to close ");
    f.render_widget(Paragraph::new(lines).block(block), modal);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

const HELP_TEXT: &[&str] = &[
    "",
    "  Global",
    "    Ctrl-C       quit",
    "    ?            toggle this help",
    "    r            refresh current view",
    "",
    "  PR list",
    "    j/k or ↓/↑   move",
    "    g g / G      top / bottom",
    "    ↵            open PR",
    "    m            merge modal",
    "    /            search",
    "    f            cycle filter",
    "    Esc          clear filter",
    "    q            quit",
    "",
    "  PR review",
    "    j/k          cursor",
    "    Ctrl-d/u     half-page",
    "    Tab/Shift-Tab next/prev file",
    "    f            file picker      m  merge modal",
    "    c            toggle commit strip",
    "    s            toggle SHA margin",
    "    q / Esc      back to list",
    "",
];
