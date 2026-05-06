//! Top-level app: terminal init/teardown, panic hook, event loop, view transitions.
//!
//! Threading model: the main thread runs the ratatui draw + input loop and
//! never makes a subprocess call. All `gh` and `git` work happens on the
//! `Worker` thread and round-trips through channels (see `data::worker`).
//! The UI drains worker responses each loop iteration.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

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
use crate::view::file_picker::FilePickerState;
use crate::view::merge_modal::{MergeMethod, MergeModalState};
use crate::view::pr_list::PrListState;
use crate::view::pr_review::PrReviewState;

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
    pub pending_g: bool,
    pub running: bool,
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
            pending_g: false,
            running: true,
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

pub fn run(term: &mut Term, app: &mut App, st: &mut AppState) -> Result<()> {
    // Kick off the initial PR list load. The first draw will show
    // "loading PRs…" while the worker thread does the gh subprocess.
    st.list.loading = true;
    app.request(Request::RefreshList);

    while st.running {
        // Drain any worker responses before drawing.
        while let Ok(resp) = app.worker.rx.try_recv() {
            handle_response(app, st, resp);
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
            st.list.prs = prs.clone();
            app.cache.set_list(prs);
            st.list.loading = false;
            st.list.status = String::new();
            // Clamp selection in case the list shrank.
            let n = st.list.visible_prs().len();
            if st.list.selected >= n {
                st.list.selected = n.saturating_sub(1);
            }
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
            st.list.status = format!("merged #{number}");
            // Refresh the list so the merged PR shows its new state.
            st.list.loading = true;
            app.request(Request::RefreshList);
        }
        Response::MergeDone {
            number,
            result: Err(e),
        } => {
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
        FocusedView::Review | FocusedView::FilePicker | FocusedView::MergeModal => {
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
                    show_commit_strip: app.config.show_commit_strip,
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
            st.list.loading = true;
            app.request(Request::RefreshList);
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
        Action::CursorDown => {
            if let Some(r) = st.review.as_mut() {
                r.cursor_line = r.cursor_line.saturating_add(1);
                r.scroll = r.scroll.saturating_add(1);
            }
        }
        Action::CursorUp => {
            if let Some(r) = st.review.as_mut() {
                r.cursor_line = r.cursor_line.saturating_sub(1);
                r.scroll = r.scroll.saturating_sub(1);
            }
        }
        Action::HalfPageDown => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_add(10);
                r.cursor_line = r.cursor_line.saturating_add(10);
            }
        }
        Action::HalfPageUp => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_sub(10);
                r.cursor_line = r.cursor_line.saturating_sub(10);
            }
        }
        Action::PageDown => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_add(20);
                r.cursor_line = r.cursor_line.saturating_add(20);
            }
        }
        Action::PageUp => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_sub(20);
                r.cursor_line = r.cursor_line.saturating_sub(20);
            }
        }
        Action::Top => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = 0;
                r.cursor_line = 0;
            }
        }
        Action::Bottom => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = u16::MAX / 2;
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
        Action::Merge => open_merge(st),
        Action::ToggleCommitStrip => {
            if let Some(r) = st.review.as_mut() {
                r.show_commit_strip = !r.show_commit_strip;
            }
        }
        Action::ToggleShaMargin => {
            if let Some(r) = st.review.as_mut() {
                r.show_sha_margin = !r.show_sha_margin;
            }
        }
        Action::BackToList => {
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
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

fn handle_mouse(_app: &mut App, st: &mut AppState, ev: crossterm::event::MouseEvent) {
    match mouse_dispatch(ev) {
        MouseAction::Scroll(d) => {
            if st.focused == FocusedView::List {
                let n = st.list.visible_prs().len();
                if d > 0 {
                    st.list.selected = (st.list.selected + d as usize).min(n.saturating_sub(1));
                } else {
                    st.list.selected = st.list.selected.saturating_sub((-d) as usize);
                }
            } else if let Some(r) = st.review.as_mut() {
                if d > 0 {
                    r.scroll = r.scroll.saturating_add(d as u16);
                } else {
                    r.scroll = r.scroll.saturating_sub((-d) as u16);
                }
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
            let method = modal.selected.cli_flag().to_string();
            let num = modal.pr_number;
            app.request(Request::Merge {
                number: num,
                method: method.clone(),
            });
            st.list.status = format!("merging #{num} ({method})…");
            close_merge_modal(st);
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
