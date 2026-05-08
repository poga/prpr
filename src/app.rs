//! Top-level app: terminal init/teardown, panic hook, event loop, view transitions.
//!
//! Threading model: the main thread runs the ratatui draw + input loop and
//! never makes a subprocess call. All `gh` and `git` work happens on the
//! `Worker` thread and round-trips through channels (see `data::worker`).
//! The UI drains worker responses each loop iteration.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::Config;
use crate::data::cache::Cache;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::worker::{Request, Response, Worker};
use crate::keys::{Action, FocusedView, MouseAction, dispatch, mouse_dispatch};
use crate::view::commits_modal::{self, CommitsModalState};
use crate::view::file_picker::FilePickerState;
use crate::view::merge_modal::{MergeMethod, MergeModalState, MergingState};
use crate::view::pr_list::PrListState;
use crate::view::pr_review::PrReviewState;

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const RETURN_REFRESH_STALE_AFTER: Duration = Duration::from_secs(30);

fn should_auto_refresh(
    focused: FocusedView,
    merging: bool,
    last_refresh_at: Option<Instant>,
    now: Instant,
    interval: Duration,
) -> bool {
    if focused != FocusedView::List {
        return false;
    }
    if merging {
        return false;
    }
    match last_refresh_at {
        None => false,
        Some(t) => now.duration_since(t) >= interval,
    }
}

fn reselect_by_number(prev: Option<u32>, new_numbers: &[u32], old_idx: usize) -> usize {
    if let Some(n) = prev
        && let Some(i) = new_numbers.iter().position(|m| *m == n)
    {
        return i;
    }
    old_idx.min(new_numbers.len().saturating_sub(1))
}

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub struct App {
    pub cache: Cache,
    pub config: Config,
    pub worker: Worker,
}

impl App {
    pub fn new(
        repo_root: std::path::PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        config: Config,
    ) -> Self {
        let worker = Worker::spawn(repo_root, gh, git, config.window_size);
        Self {
            cache: Cache::new(),
            config,
            worker,
        }
    }

    fn request(&self, req: Request) {
        self.worker.send(req);
    }
}

pub struct AppState {
    pub focused: FocusedView,
    pub list: PrListState,
    pub review: Option<PrReviewState>,
    pub current_pr: Option<u32>,
    pub picker: Option<FilePickerState>,
    pub merge: Option<MergeModalState>,
    pub merging: Option<MergingState>,
    pub commits: Option<CommitsModalState>,
    pub pending_g: bool,
    pub running: bool,
    pub last_refresh_at: Option<Instant>,
}

impl AppState {
    pub fn new(repo_name: String, branch: String) -> Self {
        Self {
            focused: FocusedView::List,
            list: PrListState {
                repo_name,
                branch,
                prs: vec![],
                selected: 0,
                filter_open_only: true,
                search: None,
                status: String::new(),
                loading: false,
            },
            review: None,
            current_pr: None,
            picker: None,
            merge: None,
            merging: None,
            commits: None,
            pending_g: false,
            running: true,
            last_refresh_at: None,
        }
    }
}

pub fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        original(info);
    }));
}

pub fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        )
    );
    let backend = CrosstermBackend::new(stdout);
    let term = Terminal::new(backend)?;
    Ok(term)
}

pub fn restore_terminal() -> Result<()> {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
    Ok(())
}

fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    if !silent {
        st.list.loading = true;
    }
    app.request(Request::RefreshList);
}

pub fn run(term: &mut Term, app: &mut App, st: &mut AppState) -> Result<()> {
    // Kick off the initial PR list load. The first draw will show
    // "loading PRs…" while the worker thread does the gh subprocess.
    send_refresh(app, st, false);

    while st.running {
        // Drain any worker responses before drawing.
        while let Ok(resp) = app.worker.rx.try_recv() {
            handle_response(app, st, resp);
        }

        // Silent auto-refresh: while the user is on the list and not in
        // the middle of a merge, re-fetch every AUTO_REFRESH_INTERVAL so
        // CI / review / merge-by-others changes show up without pressing r.
        if should_auto_refresh(
            st.focused,
            st.merging.is_some(),
            st.last_refresh_at,
            Instant::now(),
            AUTO_REFRESH_INTERVAL,
        ) {
            send_refresh(app, st, true);
        }

        term.draw(|f| draw(f, app, st))?;

        // Short timeout so we pick up worker responses promptly.
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(k) => handle_key(app, st, k),
                Event::Mouse(m) => handle_mouse(app, st, m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_response(app: &mut App, st: &mut AppState, resp: Response) {
    match resp {
        Response::ListLoaded(Ok(prs)) => {
            // Preserve the user's selected PR across refreshes: capture the
            // previously-selected PR's number, replace rows, then re-find the
            // same number in the new visible list. Falls back to a clamped
            // index if the PR is gone (e.g. closed/merged out of the filter).
            let prev_selected = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number);
            st.list.prs = prs.clone();
            app.cache.set_list(prs);
            st.list.loading = false;
            st.list.status = String::new();
            let new_numbers: Vec<u32> = st
                .list
                .visible_prs()
                .iter()
                .map(|p| p.number)
                .collect();
            st.list.selected =
                reselect_by_number(prev_selected, &new_numbers, st.list.selected);
        }
        Response::ListLoaded(Err(e)) => {
            st.list.loading = false;
            st.list.status = format!("refresh failed: {e}");
        }
        Response::PrLoaded {
            number,
            result: Ok(pkg),
        } => {
            let files_count = pkg.files.len();
            app.cache.insert(pkg);
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("{} files", files_count);
            }
        }
        Response::PrLoaded {
            number,
            result: Err(e),
        } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("load failed: {e}");
            }
            st.list.status = format!("load #{number} failed: {e}");
        }
        Response::MergeDone {
            number,
            result: Ok(()),
        } => {
            // Drop the user back into the PR list. The cached rows still
            // include the just-merged PR, so clear them — the loading
            // placeholder is shown until the auto-refresh repopulates the
            // list with the new PR states. Manual `r` refreshes from the
            // list deliberately keep rows visible (see pr_list::render_rows);
            // post-merge is different because the visible data is known
            // stale, not just possibly outdated.
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
            st.merge = None;
            st.merging = None;
            st.picker = None;
            st.list.status = format!("merged #{number}");
            st.list.prs.clear();
            st.list.selected = 0;
            send_refresh(app, st, false);
        }
        Response::MergeDone {
            number,
            result: Err(e),
        } => {
            st.merging = None;
            st.list.status = format!("merge #{number} failed: {e}");
        }
    }
}

fn draw(f: &mut ratatui::Frame, app: &App, st: &AppState) {
    let area = f.area();
    if area.width < 80 || area.height < 24 {
        let msg = ratatui::widgets::Paragraph::new("terminal too small (need ≥80×24)")
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(msg, area);
        return;
    }
    match st.focused {
        FocusedView::List | FocusedView::HelpOverlay => {
            crate::view::pr_list::render(f, area, &st.list, Utc::now());
        }
        FocusedView::Review
        | FocusedView::FilePicker
        | FocusedView::MergeModal
        | FocusedView::CommitsModal => {
            let pkg = st.current_pr.and_then(|n| app.cache.get(n));
            if let (Some(pkg), Some(review)) = (pkg, st.review.as_ref()) {
                crate::view::pr_review::render(f, area, pkg, review);
            } else {
                let text = format!("{} loading…", crate::render::spinner::glyph());
                let msg = ratatui::widgets::Paragraph::new(text)
                    .style(ratatui::style::Style::default().fg(crate::render::style::OVERLAY1))
                    .alignment(ratatui::layout::Alignment::Center);
                f.render_widget(msg, area);
            }
        }
    }

    if let Some(p) = &st.picker {
        crate::view::file_picker::render(f, area, p);
    }
    if let Some(m) = &st.merge {
        crate::view::merge_modal::render(f, area, m);
    }
    if let Some(m) = &st.merging {
        crate::view::merge_modal::render_progress(f, area, m);
    }
    if let Some(c) = &st.commits {
        crate::view::commits_modal::render(f, area, c);
    }
    if st.focused == FocusedView::HelpOverlay {
        crate::view::help::render(f, area);
    }
}

fn handle_key(app: &mut App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    // Kitty's enhanced keyboard protocol emits Press AND Release for every
    // key. We only want Press (and Repeat for held keys); Release would
    // double-fire actions.
    if !matches!(
        ev.kind,
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
    ) {
        return;
    }

    // While a merge is in flight, the worker is busy and we don't want
    // the user to stack further actions on top of it. The progress
    // overlay still redraws each tick so the spinner stays animated.
    // Ctrl-C is the one escape hatch — if the subprocess hangs, the
    // user must still be able to quit.
    if st.merging.is_some() {
        if ev.code == crossterm::event::KeyCode::Char('c')
            && ev
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            st.running = false;
        }
        return;
    }

    if st.focused == FocusedView::HelpOverlay {
        match ev.code {
            crossterm::event::KeyCode::Char('?')
            | crossterm::event::KeyCode::Esc
            | crossterm::event::KeyCode::Char('q') => {
                st.focused = if st.review.is_some() {
                    FocusedView::Review
                } else {
                    FocusedView::List
                };
            }
            _ => {}
        }
        return;
    }

    if st.focused == FocusedView::FilePicker {
        handle_file_picker(app, st, ev);
        return;
    }

    if st.focused == FocusedView::MergeModal {
        handle_merge_modal(app, st, ev);
        return;
    }

    if st.focused == FocusedView::CommitsModal {
        handle_commits_modal(st, ev);
        return;
    }

    if st.focused == FocusedView::List
        && st.pending_g
        && ev.code == crossterm::event::KeyCode::Char('g')
    {
        st.pending_g = false;
        st.list.selected = 0;
        return;
    }
    st.pending_g = false;

    if st.focused == FocusedView::List
        && let Some(buf) = st.list.search.as_mut()
    {
        match ev.code {
            crossterm::event::KeyCode::Esc => st.list.search = None,
            crossterm::event::KeyCode::Enter => {}
            crossterm::event::KeyCode::Backspace => {
                buf.pop();
            }
            crossterm::event::KeyCode::Char(c) => buf.push(c),
            _ => {}
        }
        return;
    }

    let action = dispatch(st.focused, ev);
    match action {
        Action::Quit => st.running = false,
        Action::ListUp => {
            if st.list.selected > 0 {
                st.list.selected -= 1;
            }
        }
        Action::ListDown => {
            let n = st.list.visible_prs().len();
            if st.list.selected + 1 < n {
                st.list.selected += 1;
            }
        }
        Action::ListTop => {
            st.pending_g = true;
        }
        Action::ListBottom => {
            let n = st.list.visible_prs().len();
            st.list.selected = n.saturating_sub(1);
        }
        Action::ListOpen => {
            if let Some(pr) = st.list.visible_prs().get(st.list.selected).copied() {
                let num = pr.number;
                st.current_pr = Some(num);
                st.review = Some(PrReviewState {
                    file_index: 0,
                    cursor_line: 0,
                    scroll: 0,
                    show_sha_margin: app.config.show_sha_margin,
                    status: "loading…".into(),
                });
                st.focused = FocusedView::Review;
                if app.cache.get(num).is_none() {
                    app.request(Request::LoadPr(num));
                }
            }
        }
        Action::ListMerge => open_merge(st),
        Action::ListRefresh => {
            send_refresh(app, st, false);
        }
        Action::ListSearch => {
            st.list.search = Some(String::new());
        }
        Action::ListCycleFilter => {
            st.list.filter_open_only = !st.list.filter_open_only;
        }
        Action::ListClearFilter => {
            st.list.search = None;
        }
        Action::CursorDown => move_review(app, st, 1),
        Action::CursorUp => move_review(app, st, -1),
        Action::HalfPageDown => move_review(app, st, 10),
        Action::HalfPageUp => move_review(app, st, -10),
        Action::PageDown => move_review(app, st, 20),
        Action::PageUp => move_review(app, st, -20),
        Action::Top => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = 0;
                r.cursor_line = 0;
            }
        }
        Action::Bottom => {
            if let Some((num, r)) = st.current_pr.zip(st.review.as_mut())
                && let Some(pkg) = app.cache.get(num)
                && let Some(file) = pkg.files.get(r.file_index)
            {
                r.scroll = max_scroll(file.lines.len());
                r.cursor_line = max_cursor_line(file);
            }
        }
        Action::NextFile => cycle_file(app, st, 1),
        Action::PrevFile => cycle_file(app, st, -1),
        Action::OpenFilePicker => {
            if let (Some(num), Some(_)) = (st.current_pr, st.review.as_ref())
                && let Some(pkg) = app.cache.get(num)
            {
                st.picker = Some(FilePickerState {
                    query: String::new(),
                    all_files: pkg.files.iter().map(|f| f.path.clone()).collect(),
                    selected: 0,
                });
                st.focused = FocusedView::FilePicker;
            }
        }
        Action::OpenCommitsModal => {
            if let (Some(num), Some(_)) = (st.current_pr, st.review.as_ref())
                && let Some(pkg) = app.cache.get(num)
            {
                let rows = commits_modal::build_rows(
                    &pkg.detail.commits,
                    &pkg.commit_stats,
                    app.config.window_size,
                    Utc::now(),
                );
                st.commits = Some(CommitsModalState { rows, selected: 0 });
                st.focused = FocusedView::CommitsModal;
            }
        }
        Action::Merge => open_merge(st),
        Action::ToggleShaMargin => {
            if let Some(r) = st.review.as_mut() {
                r.show_sha_margin = !r.show_sha_margin;
            }
        }
        Action::BackToList => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
            // If the cached list is older than RETURN_REFRESH_STALE_AFTER,
            // kick off a silent refresh so the user lands on fresh data.
            // Bouncing in/out of a PR review within the threshold reuses
            // the existing rows.
            let stale = st
                .last_refresh_at
                .is_none_or(|t| t.elapsed() >= RETURN_REFRESH_STALE_AFTER);
            if stale {
                send_refresh(app, st, true);
            }
        }
        Action::Help => {
            st.focused = FocusedView::HelpOverlay;
        }
        Action::Refresh => {
            if let Some(num) = st.current_pr {
                if let Some(r) = st.review.as_mut() {
                    r.status = "loading…".into();
                }
                app.request(Request::LoadPr(num));
            }
        }
        Action::Nothing => {}
    }
}

fn open_merge(st: &mut AppState) {
    if let Some(num) = st
        .list
        .visible_prs()
        .get(st.list.selected)
        .map(|p| p.number)
        .or(st.current_pr)
    {
        st.merge = Some(MergeModalState {
            pr_number: num,
            default: MergeMethod::Merge,
            selected: MergeMethod::Merge,
        });
        st.focused = FocusedView::MergeModal;
    }
}

/// We don't know the precise body height from inside `handle_key`, so we use
/// a conservative fudge so the last lines stay visible after End / Bottom.
const APPROX_BODY_HEIGHT: usize = 15;

fn max_scroll(total_lines: usize) -> u16 {
    total_lines
        .saturating_sub(APPROX_BODY_HEIGHT)
        .min(u16::MAX as usize) as u16
}

fn max_cursor_line(file: &crate::data::diff::FileDiff) -> usize {
    file.lines
        .iter()
        .filter(|l| !l.is_hunk_header)
        .count()
        .saturating_sub(1)
}

/// Move the review cursor + scroll by `delta` lines, clamping at both ends so
/// scrolling can't blank out the buffer.
fn move_review(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let Some(r) = st.review.as_mut() else { return };
    let Some(file) = pkg.files.get(r.file_index) else {
        return;
    };
    let max_scr = max_scroll(file.lines.len()) as i64;
    let max_cur = max_cursor_line(file) as i64;
    let new_scroll = (r.scroll as i64 + delta as i64).clamp(0, max_scr);
    let new_cursor = (r.cursor_line as i64 + delta as i64).clamp(0, max_cur);
    r.scroll = new_scroll as u16;
    r.cursor_line = new_cursor as usize;
}

fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let Some(pkg) = app.cache.get(num) else {
        return;
    };
    let n = pkg.files.len() as i32;
    if n == 0 {
        return;
    }
    if let Some(r) = st.review.as_mut() {
        let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
        r.file_index = new_idx;
        r.cursor_line = 0;
        r.scroll = 0;
    }
}

fn handle_mouse(app: &mut App, st: &mut AppState, ev: crossterm::event::MouseEvent) {
    if st.merging.is_some() {
        return;
    }
    match mouse_dispatch(ev) {
        MouseAction::Scroll(d) => {
            if st.focused == FocusedView::List {
                let n = st.list.visible_prs().len();
                if d > 0 {
                    st.list.selected = (st.list.selected + d as usize).min(n.saturating_sub(1));
                } else {
                    st.list.selected = st.list.selected.saturating_sub((-d) as usize);
                }
            } else {
                move_review(app, st, d as i32);
            }
        }
        MouseAction::ClickAt { col: _, row } => {
            if st.focused == FocusedView::List && row >= 2 {
                let idx = (row - 2) as usize;
                if idx < st.list.visible_prs().len() {
                    st.list.selected = idx;
                }
            }
        }
        MouseAction::DoubleClickAt { .. } | MouseAction::Nothing => {}
    }
}

fn handle_file_picker(app: &App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;
    let Some(picker) = st.picker.as_mut() else {
        return;
    };
    match ev.code {
        KeyCode::Esc => {
            st.picker = None;
            st.focused = FocusedView::Review;
        }
        KeyCode::Enter => {
            let chosen = picker.matches().get(picker.selected).map(|s| (*s).clone());
            if let (Some(path), Some(num)) = (chosen, st.current_pr)
                && let Some(pkg) = app.cache.get(num)
                && let Some(idx) = pkg.files.iter().position(|f| f.path == path)
                && let Some(r) = st.review.as_mut()
            {
                r.file_index = idx;
                r.cursor_line = 0;
                r.scroll = 0;
            }
            st.picker = None;
            st.focused = FocusedView::Review;
        }
        KeyCode::Down => {
            let n = picker.matches().len();
            if picker.selected + 1 < n {
                picker.selected += 1;
            }
        }
        KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
        }
        KeyCode::Backspace => {
            picker.query.pop();
            picker.selected = 0;
        }
        KeyCode::Char(c) => {
            picker.query.push(c);
            picker.selected = 0;
        }
        _ => {}
    }
}

fn handle_merge_modal(app: &mut App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;
    let Some(modal) = st.merge.as_mut() else {
        return;
    };
    match ev.code {
        KeyCode::Esc => {
            close_merge_modal(st);
        }
        KeyCode::Enter => {
            let method = modal.selected;
            let num = modal.pr_number;
            app.request(Request::Merge {
                number: num,
                method: method.cli_flag().to_string(),
            });
            st.merging = Some(MergingState {
                pr_number: num,
                method,
            });
            close_merge_modal(st);
        }
        KeyCode::Up | KeyCode::BackTab => {
            modal.selected = modal.selected.cycle(-1);
        }
        KeyCode::Down | KeyCode::Tab => {
            modal.selected = modal.selected.cycle(1);
        }
        KeyCode::Char('j') => {
            modal.selected = modal.selected.cycle(1);
        }
        KeyCode::Char('k') => {
            modal.selected = modal.selected.cycle(-1);
        }
        KeyCode::Char(c) => {
            if let Some(method) = crate::view::merge_modal::from_letter(c) {
                modal.selected = method;
            }
        }
        _ => {}
    }
}

fn close_merge_modal(st: &mut AppState) {
    st.merge = None;
    st.focused = if st.review.is_some() {
        FocusedView::Review
    } else {
        FocusedView::List
    };
}

fn handle_commits_modal(st: &mut AppState, ev: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    if ev.code == KeyCode::Char('c') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        st.running = false;
        return;
    }
    let Some(modal) = st.commits.as_mut() else {
        return;
    };
    match ev.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('C') => {
            st.commits = None;
            st.focused = FocusedView::Review;
        }
        KeyCode::Down | KeyCode::Char('j') => modal.move_down(),
        KeyCode::Up | KeyCode::Char('k') => modal.move_up(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn auto_refresh_blocked_when_not_on_list() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::Review,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_merging() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::List,
            true,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_last_refresh_unset() {
        let now = Instant::now();
        assert!(!should_auto_refresh(
            FocusedView::List,
            false,
            None,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_interval_not_elapsed() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(30));
        assert!(!should_auto_refresh(
            FocusedView::List,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_fires_when_interval_elapsed() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(61));
        assert!(should_auto_refresh(
            FocusedView::List,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_fires_exactly_at_interval_boundary() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(60));
        assert!(should_auto_refresh(
            FocusedView::List,
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn reselect_keeps_position_when_pr_still_present() {
        let new = [101u32, 99, 42, 7];
        // prev = 42, was at index 1; now at index 2
        assert_eq!(reselect_by_number(Some(42), &new, 1), 2);
    }

    #[test]
    fn reselect_falls_back_to_clamped_old_idx_when_pr_gone() {
        let new = [101u32, 99, 7];
        // prev = 42 no longer in the list; old_idx 1 stays valid
        assert_eq!(reselect_by_number(Some(42), &new, 1), 1);
    }

    #[test]
    fn reselect_clamps_old_idx_when_list_shrinks() {
        let new = [101u32, 99];
        // prev = 42 gone, old_idx 5 clamped to len-1 = 1
        assert_eq!(reselect_by_number(Some(42), &new, 5), 1);
    }

    #[test]
    fn reselect_handles_empty_list() {
        let new: [u32; 0] = [];
        assert_eq!(reselect_by_number(Some(42), &new, 3), 0);
    }

    #[test]
    fn reselect_with_no_prev_clamps_old_idx() {
        let new = [101u32, 99, 7];
        assert_eq!(reselect_by_number(None, &new, 5), 2);
    }
}
