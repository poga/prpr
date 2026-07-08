//! PR list view rendering. State is small and self-contained.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::data::pr::{CiState, MergeState, Pr, PrState, ReviewDecision};
use crate::data::worker::ListStage;
use crate::render::spinner;
use crate::render::style::*;

/// Inline file data for the selected PR; tagged with the PR number.
#[derive(Debug, Clone)]
pub enum ExpandedFiles {
    Loading { number: u32 },
    Ready { number: u32, files: Vec<crate::data::pr::FileMeta> },
    Error { number: u32, message: String },
}

impl ExpandedFiles {
    pub fn number(&self) -> u32 {
        match self {
            Self::Loading { number }
            | Self::Ready { number, .. }
            | Self::Error { number, .. } => *number,
        }
    }
}

#[derive(Debug, Default)]
pub struct PrListState {
    pub repo_name: String,
    pub branch: String,
    pub prs: Vec<Pr>,
    pub selected: usize,
    pub search: Option<String>,
    pub status: String,
    /// True while the initial `gh pr list` is in flight. The renderer
    /// shows a centered "loading…" placeholder instead of an empty body.
    pub loading: bool,
    /// True between `ListFast` and `ListEnriched` arrivals. Footer shows
    /// `enriching…` so background work is never silent.
    pub enriching: bool,
    /// Most-recent pipeline stage reported by the worker during a refresh.
    /// Renderer prefers this label over the generic "loading PRs…" so the
    /// user sees whether `gh` or `git fetch` is the slow step.
    pub loading_stage: Option<ListStage>,
    /// True from when the user presses `r` (or the initial load fires) until
    /// the full refresh — fast list **and** enrichment — completes. The
    /// renderer hides rows behind a full-area loading placeholder and the
    /// input layer ignores keys other than quit so the user can't act on
    /// stale data mid-refresh.
    pub manual_refresh_in_flight: bool,
    /// Inline files for the selected PR; tagged with the PR number.
    pub expanded: Option<ExpandedFiles>,
}

impl PrListState {
    pub fn visible_prs(&self) -> Vec<&Pr> {
        let q = self.search.as_deref().map(str::to_lowercase);
        self.prs
            .iter()
            .filter(|p| match &q {
                Some(s) => {
                    p.title.to_lowercase().contains(s) || p.author.login.to_lowercase().contains(s)
                }
                None => true,
            })
            .collect()
    }
}

pub fn render(f: &mut Frame, area: Rect, st: &PrListState, now: DateTime<Utc>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(f, chunks[0], st);
    render_rows(f, chunks[1], st, now);
    render_footer(f, chunks[2], st);
}

fn render_header(f: &mut Frame, area: Rect, st: &PrListState) {
    let visible = st.visible_prs();
    let count = visible.len();
    let header = format!(
        "  prpr · {} · {} · {} open",
        st.repo_name, st.branch, count,
    );
    f.render_widget(
        Paragraph::new(header).style(Style::default().fg(OVERLAY1)),
        area,
    );
}

fn render_rows(f: &mut Frame, area: Rect, st: &PrListState, now: DateTime<Utc>) {
    // Manual refresh (initial load or pressing `r`) blocks the view: the
    // rows are replaced with a centered loading placeholder until the fast
    // list AND enrichment have both arrived. Silent auto-refresh keeps the
    // rows visible — only the user-initiated path is intentionally modal.
    if st.manual_refresh_in_flight {
        let body = st
            .loading_stage
            .map(|s| s.label())
            .unwrap_or("loading PRs");
        f.render_widget(
            Paragraph::new(format!("{} {body}…", spinner::glyph()))
                .style(Style::default().fg(OVERLAY1))
                .alignment(ratatui::layout::Alignment::Center),
            area,
        );
        return;
    }
    let visible = st.visible_prs();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible.len() + 1);
    lines.push(divider(area.width as usize));
    for (i, pr) in visible.iter().enumerate() {
        lines.push(row_for(pr, i == st.selected, now, area.width));
        if i == st.selected {
            match &st.expanded {
                Some(ExpandedFiles::Loading { number }) if *number == pr.number => {
                    lines.push(loading_line(area.width));
                }
                Some(ExpandedFiles::Ready { number, files }) if *number == pr.number => {
                    let total = files.len();
                    for (fi, f) in files.iter().enumerate() {
                        let last = fi + 1 == total;
                        lines.push(file_line(f, last, area.width));
                    }
                }
                Some(ExpandedFiles::Error { number, message }) if *number == pr.number => {
                    lines.push(error_line(message, area.width));
                }
                _ => {}
            }
        }
    }
    // Compute the absolute line index of the selected PR's row, so we
    // can scroll it into view when the expanded block pushes content
    // past the viewport. The selected row's index is:
    //   1 (divider) + selected
    // (only the selected row has expansion lines, and they appear AFTER
    // the row, so they don't shift the selected row's own position).
    let selected_row_idx = 1 + st.selected;
    let h = area.height as usize;
    let total = lines.len();
    let offset = if total <= h {
        0
    } else if selected_row_idx + 2 < h {
        // Selected row already in the upper portion — no scroll needed.
        0
    } else {
        // Keep the selected row ~2 lines from the top of the viewport.
        let target_top = selected_row_idx.saturating_sub(2);
        target_top.min(total.saturating_sub(h))
    };
    let view: Vec<Line<'static>> =
        lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(view), area);
}

fn render_footer(f: &mut Frame, area: Rect, st: &PrListState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(
        Paragraph::new("  ↵ open   o browser   m merge   d draft   r refresh   / search   q quit")
            .style(Style::default().fg(OVERLAY1)),
        chunks[0],
    );
    // When there's a status (e.g. refresh error or in-progress merge),
    // show that instead of the legend. Errors must never be silent — the
    // user should see them. In-progress messages get a spinner prefix.
    if !st.status.is_empty() {
        let (prefix, color) = if spinner::looks_in_progress(&st.status) {
            (format!("{} ", spinner::glyph()), OVERLAY1)
        } else {
            (String::new(), DIFF_DEL_FG)
        };
        f.render_widget(
            Paragraph::new(format!("  {prefix}{}", st.status)).style(Style::default().fg(color)),
            chunks[1],
        );
    } else if st.loading {
        // Refresh in flight while the list is still showing prior rows —
        // keep the spinner visible so background work is never silent.
        let label = st
            .loading_stage
            .map(|s| s.label())
            .unwrap_or("refreshing");
        f.render_widget(
            Paragraph::new(format!("  {} {label}…", spinner::glyph()))
                .style(Style::default().fg(OVERLAY1)),
            chunks[1],
        );
    } else if st.enriching {
        f.render_widget(
            Paragraph::new(format!("  {} enriching…", spinner::glyph()))
                .style(Style::default().fg(OVERLAY1)),
            chunks[1],
        );
    } else {
        f.render_widget(
            Paragraph::new(
                "  state ●open ○draft   ci ✓pass ✗fail …pend   review ✓approved !changes ·pending   ⚠conflict ?checking",
            )
            .style(Style::default().fg(OVERLAY0)),
            chunks[1],
        );
    }
}

fn divider(w: usize) -> Line<'static> {
    Line::from(Span::styled(
        "  ".to_string() + &"─".repeat(w.saturating_sub(2)),
        Style::default().fg(SURFACE2),
    ))
}

fn row_for(pr: &Pr, selected: bool, now: DateTime<Utc>, area_width: u16) -> Line<'static> {
    let row_bg = if selected {
        Style::default().bg(SURFACE0)
    } else {
        Style::default()
    };

    let state_glyph = match pr.state {
        _ if pr.is_draft => Span::styled("○", Style::default().fg(DRAFT_ACCENT)),
        PrState::Open => Span::styled("●", Style::default().fg(DIFF_ADD_FG)),
        PrState::Closed => Span::styled("●", Style::default().fg(DIFF_DEL_FG)),
        PrState::Merged => Span::styled("●", Style::default().fg(COMMIT_PALETTE[1])),
    };
    let ci_glyph = match pr.ci_state() {
        CiState::Pass => Span::styled("✓", Style::default().fg(DIFF_ADD_FG)),
        CiState::Fail => Span::styled("✗", Style::default().fg(DIFF_DEL_FG)),
        CiState::Pending => Span::styled("…", Style::default().fg(COMMIT_PALETTE[4])),
        CiState::None => Span::styled(" ", Style::default()),
    };
    let review_glyph = match pr.review_decision {
        Some(ReviewDecision::Approved) => Span::styled("✓", Style::default().fg(DIFF_ADD_FG)),
        Some(ReviewDecision::ChangesRequested) => {
            Span::styled("!", Style::default().fg(COMMIT_PALETTE[4]))
        }
        _ => Span::styled("·", Style::default().fg(COMMIT_PALETTE[1])),
    };
    // Merge marker for OPEN PRs only; stale mergeability isn't actionable.
    let conflict_glyph = if pr.state == PrState::Open {
        match pr.merge_state() {
            Some(MergeState::Conflicting) => Span::styled("⚠", Style::default().fg(DIFF_DEL_FG)),
            Some(MergeState::Unknown) => Span::styled("?", Style::default().fg(OVERLAY0)),
            _ => Span::styled(" ", Style::default()),
        }
    } else {
        Span::styled(" ", Style::default())
    };

    let pr_num = format!(" #{} ", pr.number);
    let label_str = pr
        .labels
        .first()
        .map(|l| format!("  [{}]  ", l.name))
        .unwrap_or_else(|| "  ".to_string());
    let draft_str = if pr.is_draft { "draft  ".to_string() } else { String::new() };
    let author_str = format!("{} ", pr.author.login);
    let age = format!(
        "c{} · u{}",
        humanize_age(pr.created_at, now),
        humanize_age(pr.updated_at, now),
    );

    // Rail is 1 cell (▎ for drafts, blank otherwise); row stays 2 cells wide.
    let rail = if pr.is_draft {
        Span::styled("▎", row_bg.fg(DRAFT_ACCENT))
    } else {
        Span::styled(" ", row_bg)
    };

    // Layout widths. Title takes whatever's left after the fixed-width
    // glyphs and the variable-width right side, so a wide terminal shows
    // long titles in full and a narrow terminal truncates with "…".
    // Fixed left = 9 cells: rail, state, ci, review, conflict + 4 gap spaces.
    let left_cols = 9 + pr_num.chars().count();
    let right_cols = label_str.chars().count()
        + draft_str.chars().count()
        + author_str.chars().count()
        + age.chars().count();
    let title_budget = (area_width as usize)
        .saturating_sub(left_cols)
        .saturating_sub(right_cols)
        .max(8);

    Line::from(vec![
        rail,
        Span::styled(" ", row_bg),
        state_glyph,
        Span::styled(" ", row_bg),
        ci_glyph,
        Span::styled(" ", row_bg),
        review_glyph,
        Span::styled(" ", row_bg),
        conflict_glyph,
        Span::styled(pr_num, row_bg.fg(COMMIT_PALETTE[1])),
        Span::styled(truncate(&pr.title, title_budget), row_bg.fg(TEXT)),
        Span::styled(label_str, row_bg.fg(COMMIT_PALETTE[4])),
        Span::styled(draft_str, row_bg.fg(DRAFT_ACCENT)),
        Span::styled(author_str, row_bg.fg(COMMIT_PALETTE[0])),
        Span::styled(age, row_bg.fg(OVERLAY0)),
    ])
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{:width$}", s, width = max)
    } else {
        let cut: String = s.chars().take(max - 1).collect();
        format!("{}…", cut)
    }
}

fn humanize_age(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - t).num_seconds().max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else if secs < 86400 * 14 {
        format!("{}d", secs / 86400)
    } else {
        format!("{}w", secs / (86400 * 7))
    }
}

fn loading_line(width: u16) -> Line<'static> {
    let body = format!("  {} loading files…", crate::render::spinner::glyph());
    Line::from(Span::styled(
        format!("{:<width$}", body, width = width as usize),
        Style::default().fg(OVERLAY1),
    ))
}

fn error_line(message: &str, width: u16) -> Line<'static> {
    let max = (width as usize).saturating_sub(10).max(8);
    let trimmed = truncate(message, max);
    let body = format!("  error: {trimmed}");
    Line::from(Span::styled(
        format!("{:<width$}", body, width = width as usize),
        Style::default().fg(DIFF_DEL_FG),
    ))
}

fn file_line(f: &crate::data::pr::FileMeta, last: bool, width: u16) -> Line<'static> {
    let glyph = if last { "└" } else { "├" };
    // Compute stats length for layout — same content colored separately below.
    let mut stats_len = 0;
    if f.additions > 0 { stats_len += format!("+{}", f.additions).chars().count(); }
    if f.additions > 0 && f.deletions > 0 { stats_len += 1; }
    if f.deletions > 0 { stats_len += format!("-{}", f.deletions).chars().count(); }
    let left_cols = 4; // "  ├ " or "  └ "
    let path_budget = (width as usize)
        .saturating_sub(left_cols)
        .saturating_sub(stats_len + 2)
        .max(8);
    let path = if f.path.chars().count() <= path_budget {
        f.path.clone()
    } else {
        let skip = f.path.chars().count() - (path_budget - 1);
        format!("…{}", f.path.chars().skip(skip).collect::<String>())
    };
    let pad_cols = (width as usize)
        .saturating_sub(left_cols)
        .saturating_sub(path.chars().count())
        .saturating_sub(stats_len);
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        format!("  {glyph} "),
        Style::default().fg(SURFACE2),
    )];
    // Split at last '/' so directory prefix renders dim and filename pops.
    match path.rfind('/') {
        Some(i) => {
            let (dir, name) = path.split_at(i + 1);
            spans.push(Span::styled(dir.to_string(), Style::default().fg(OVERLAY1)));
            spans.push(Span::styled(name.to_string(), Style::default().fg(TEXT)));
        }
        None => {
            spans.push(Span::styled(path.clone(), Style::default().fg(TEXT)));
        }
    }
    spans.push(Span::styled(" ".repeat(pad_cols), Style::default()));
    if f.additions > 0 {
        spans.push(Span::styled(
            format!("+{}", f.additions),
            Style::default().fg(DIFF_ADD_FG),
        ));
    }
    if f.additions > 0 && f.deletions > 0 {
        spans.push(Span::styled(" ".to_string(), Style::default()));
    }
    if f.deletions > 0 {
        spans.push(Span::styled(
            format!("-{}", f.deletions),
            Style::default().fg(DIFF_DEL_FG),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pr::Pr;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn fixture_state() -> PrListState {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        PrListState {
            repo_name: "prpr".into(),
            branch: "main".into(),
            prs,
            selected: 0,
            search: None,
            loading: false,
            enriching: false,
            loading_stage: None,
            status: String::new(),
            manual_refresh_in_flight: false,
            expanded: None,
        }
    }

    #[test]
    fn renders_header_with_repo_and_count() {
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let st = fixture_state();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let line0 = buffer_line(buf, 0);
        assert!(line0.contains("prpr"));
        assert!(line0.contains("2 open"));
    }

    #[test]
    fn search_filters_rows() {
        let mut st = fixture_state();
        st.search = Some("metrics".into());
        assert_eq!(st.visible_prs().len(), 1);
        assert_eq!(st.visible_prs()[0].number, 479);
    }

    fn buffer_line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    }

    #[test]
    fn footer_shows_enriching_when_flag_set() {
        let mut st = fixture_state();
        st.enriching = true;
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bottom = buffer_line(buf, 9);
        assert!(bottom.contains("enriching"), "footer was: {bottom:?}");
    }

    #[test]
    fn cold_load_body_shows_current_stage_label() {
        let mut st = fixture_state();
        st.prs.clear();
        st.loading = true;
        st.manual_refresh_in_flight = true;
        st.loading_stage = Some(ListStage::FetchingRefs);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Body is centered around row 5 in an 80x10 area; just scan all rows.
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            all.contains("fetching branches (git)"),
            "body should show the FetchingRefs label; got:\n{all}"
        );
    }

    #[test]
    fn refresh_footer_shows_current_stage_label() {
        let mut st = fixture_state();
        st.loading = true;
        st.loading_stage = Some(ListStage::FetchingList);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bottom = buffer_line(buf, 9);
        assert!(
            bottom.contains("fetching PR list (gh)"),
            "footer should show the FetchingList label; got: {bottom:?}"
        );
        // Generic "refreshing…" must not appear when a stage is known.
        assert!(
            !bottom.contains("refreshing"),
            "footer should not fall back to generic refreshing; got: {bottom:?}"
        );
    }

    #[test]
    fn manual_refresh_hides_existing_rows() {
        let mut st = fixture_state();
        // Rows are present (warm refresh), but a user-initiated `r` is in
        // flight — body must be replaced with a loading placeholder so the
        // user cannot act on stale data mid-refresh.
        assert!(!st.prs.is_empty(), "fixture should have rows");
        st.manual_refresh_in_flight = true;
        st.loading = true;
        st.loading_stage = Some(ListStage::FetchingList);
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        // No PR rows leaked through the placeholder.
        for pr in &st.prs {
            let needle = format!("#{}", pr.number);
            assert!(
                !all.contains(&needle),
                "row {needle} should be hidden while manual refresh is in flight; got:\n{all}"
            );
        }
        assert!(
            all.contains("fetching PR list (gh)"),
            "body should show stage label; got:\n{all}"
        );
    }

    #[test]
    fn expanded_files_number_accessor_works_for_all_variants() {
        let l = ExpandedFiles::Loading { number: 7 };
        let r = ExpandedFiles::Ready { number: 8, files: vec![] };
        let e = ExpandedFiles::Error { number: 9, message: "x".into() };
        assert_eq!(l.number(), 7);
        assert_eq!(r.number(), 8);
        assert_eq!(e.number(), 9);
    }

    #[test]
    fn footer_omits_enriching_when_flag_clear() {
        let st = fixture_state();
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| {
            let area = f.area();
            render(f, area, &st, now)
        })
        .unwrap();
        let buf = term.backend().buffer();
        let bottom = buffer_line(buf, 9);
        assert!(!bottom.contains("enriching"), "footer was: {bottom:?}");
    }

    #[test]
    fn expanded_ready_renders_file_paths_under_selected_row() {
        use crate::data::pr::FileMeta;
        let mut st = fixture_state();
        st.selected = 0;
        let sel_number = st.visible_prs()[0].number;
        st.expanded = Some(ExpandedFiles::Ready {
            number: sel_number,
            files: vec![
                FileMeta { path: "src/foo.rs".into(), additions: 12, deletions: 3 },
                FileMeta { path: "tests/bar.rs".into(), additions: 4, deletions: 0 },
            ],
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("src/foo.rs"), "missing src/foo.rs in:\n{all}");
        assert!(all.contains("tests/bar.rs"), "missing tests/bar.rs in:\n{all}");
        assert!(all.contains("+12"), "missing +12 in:\n{all}");
        assert!(all.contains("-3"),  "missing -3 in:\n{all}");
        assert!(all.contains("+4"),  "missing +4 in:\n{all}");
    }

    #[test]
    fn expanded_loading_renders_loading_files_text() {
        let mut st = fixture_state();
        st.selected = 0;
        let sel_number = st.visible_prs()[0].number;
        st.expanded = Some(ExpandedFiles::Loading { number: sel_number });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("loading files"), "missing loading text in:\n{all}");
    }

    #[test]
    fn expanded_mismatched_number_does_not_render_files() {
        use crate::data::pr::FileMeta;
        let mut st = fixture_state();
        st.selected = 0;
        st.expanded = Some(ExpandedFiles::Ready {
            number: 999_999,
            files: vec![FileMeta { path: "stale.rs".into(), additions: 1, deletions: 0 }],
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("stale.rs"), "stale row leaked into:\n{all}");
    }

    #[test]
    fn expanded_error_renders_error_message_under_selected_row() {
        let mut st = fixture_state();
        st.selected = 0;
        let sel_number = st.visible_prs()[0].number;
        st.expanded = Some(ExpandedFiles::Error {
            number: sel_number,
            message: "ref missing locally".into(),
        });
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("error:"), "expected 'error:' prefix in:\n{all}");
        assert!(all.contains("ref missing locally"), "expected error message in:\n{all}");
    }

    #[test]
    fn selected_row_stays_visible_when_expanded_block_is_tall() {
        // 20 PRs; selected = 19 (last); expanded with 20 files; body
        // area is 14 lines (18 terminal - 4 header/footer). Selected row
        // sits at line index 20 (1 divider + 19 prior rows) — off-screen
        // without scrolling. Scroll must bring it into view.
        use crate::data::pr::{Author, FileMeta, Pr, PrState};
        let st = PrListState {
            repo_name: "prpr".into(),
            branch: "main".into(),
            prs: (0..20).map(|i| Pr {
                number: 100 + i, title: format!("p{i}"), is_draft: false, state: PrState::Open,
                author: Author { login: "a".into() }, created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                base_ref_name: "main".into(), head_ref_name: "f".into(),
                labels: vec![], status_check_rollup: vec![],
                review_decision: None, mergeable: None,
            }).collect(),
            selected: 19,
            expanded: Some(ExpandedFiles::Ready {
                number: 119,
                files: (0..20).map(|i| FileMeta {
                    path: format!("file{i}.rs"), additions: 1, deletions: 0,
                }).collect(),
            }),
            ..Default::default()
        };
        let mut term = Terminal::new(TestBackend::new(80, 18)).unwrap();
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        term.draw(|f| { let area = f.area(); render(f, area, &st, now); }).unwrap();
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| buffer_line(buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("#119"), "selected PR #119 must be visible in:\n{all}");
    }

    #[test]
    fn file_line_dims_directory_and_brightens_filename() {
        use crate::data::pr::FileMeta;
        let line = file_line(
            &FileMeta { path: "src/foo/bar.rs".into(), additions: 1, deletions: 0 },
            false,
            80,
        );
        let dim_span = line
            .spans
            .iter()
            .find(|s| s.content == "src/foo/")
            .expect("expected a span with text 'src/foo/'");
        assert_eq!(dim_span.style.fg, Some(OVERLAY1), "dir prefix must be OVERLAY1");
        let bright_span = line
            .spans
            .iter()
            .find(|s| s.content == "bar.rs")
            .expect("expected a span with text 'bar.rs'");
        assert_eq!(bright_span.style.fg, Some(TEXT), "filename must be TEXT");
    }

    #[test]
    fn file_line_top_level_file_is_all_bright() {
        use crate::data::pr::FileMeta;
        let line = file_line(
            &FileMeta { path: "Cargo.toml".into(), additions: 1, deletions: 0 },
            false,
            80,
        );
        let bright_span = line
            .spans
            .iter()
            .find(|s| s.content == "Cargo.toml")
            .expect("expected a span with text 'Cargo.toml'");
        assert_eq!(bright_span.style.fg, Some(TEXT), "filename must be TEXT");
        assert!(
            !line.spans.iter().any(|s| s.style.fg == Some(OVERLAY1) && s.content.contains("Cargo.toml")),
            "top-level file should not have a dim path span"
        );
    }

    #[test]
    fn draft_pr_shows_draft_badge() {
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        let mk = |is_draft: bool| Pr {
            number: 1, title: "t".into(), is_draft, state: PrState::Open,
            author: crate::data::pr::Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(), head_ref_name: "f".into(),
            labels: vec![], status_check_rollup: vec![],
            review_decision: None, mergeable: None,
        };
        let has_badge = |line: &Line| line.spans.iter().any(|s| s.content == "draft  ");

        let draft = row_for(&mk(true), false, now, 80);
        assert!(has_badge(&draft), "draft PR should show the 'draft' badge");

        let not_draft = row_for(&mk(false), false, now, 80);
        assert!(!has_badge(&not_draft), "non-draft rows must not show the badge");
    }

    #[test]
    fn draft_row_shows_peach_rail() {
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        let mk = |is_draft: bool| Pr {
            number: 1, title: "t".into(), is_draft, state: PrState::Open,
            author: crate::data::pr::Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(), head_ref_name: "f".into(),
            labels: vec![], status_check_rollup: vec![],
            review_decision: None, mergeable: None,
        };
        // Draft row leads with the rail glyph, painted in the draft accent.
        let draft = row_for(&mk(true), false, now, 80);
        assert_eq!(draft.spans[0].content, "▎", "draft row must lead with the rail glyph");
        assert_eq!(draft.spans[0].style.fg, Some(DRAFT_ACCENT), "rail must use DRAFT_ACCENT");
        // Ready row does not.
        let ready = row_for(&mk(false), false, now, 80);
        assert_ne!(ready.spans[0].content, "▎", "ready row must not show the rail");
    }

    #[test]
    fn draft_state_glyph_is_peach() {
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        let draft = Pr {
            number: 1, title: "t".into(), is_draft: true, state: PrState::Open,
            author: crate::data::pr::Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(), head_ref_name: "f".into(),
            labels: vec![], status_check_rollup: vec![],
            review_decision: None, mergeable: None,
        };
        let line = row_for(&draft, false, now, 80);
        let circle = line.spans.iter().find(|s| s.content == "○").expect("draft shows ○");
        assert_eq!(circle.style.fg, Some(DRAFT_ACCENT), "draft ○ must use DRAFT_ACCENT");
    }

    #[test]
    fn unknown_mergeable_open_pr_shows_checking_marker() {
        let now: DateTime<Utc> = "2026-05-06T00:00:00Z".parse().unwrap();
        let mk = |m: &str| Pr {
            number: 1, title: "t".into(), is_draft: false, state: PrState::Open,
            author: crate::data::pr::Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(), head_ref_name: "f".into(),
            labels: vec![], status_check_rollup: vec![],
            review_decision: None, mergeable: Some(m.into()),
        };
        let has = |line: &Line, glyph: &str| line.spans.iter().any(|s| s.content == glyph);

        let unknown = row_for(&mk("UNKNOWN"), false, now, 80);
        assert!(has(&unknown, "?"), "UNKNOWN row should show '?'");
        assert!(!has(&unknown, "⚠"), "UNKNOWN row must not show '⚠'");

        let conflicting = row_for(&mk("CONFLICTING"), false, now, 80);
        assert!(has(&conflicting, "⚠"), "CONFLICTING row should show '⚠'");

        let mergeable = row_for(&mk("MERGEABLE"), false, now, 80);
        assert!(!has(&mergeable, "?"), "MERGEABLE row must not show '?'");
        assert!(!has(&mergeable, "⚠"), "MERGEABLE row must not show '⚠'");
    }
}
