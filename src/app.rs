//! Top-level app: terminal init/teardown, panic hook, event loop, view transitions.

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
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::Config;
use crate::data::cache::Cache;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::keys::{dispatch, mouse_dispatch, Action, FocusedView, MouseAction};
use crate::view::file_picker::FilePickerState;
use crate::view::merge_modal::{MergeMethod, MergeModalState};
use crate::view::pr_list::PrListState;
use crate::view::pr_review::PrReviewState;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub struct App {
    pub cache: Cache,
    pub config: Config,
}

impl App {
    pub fn new(
        repo_root: std::path::PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        config: Config,
    ) -> Self {
        let window = config.window_size;
        Self {
            cache: Cache::new(repo_root, gh, git, window),
            config,
        }
    }

    /// Populate the cache for `number` if not already loaded. Errors are
    /// silently swallowed — they show up in `st.list.status` via the caller.
    pub fn ensure_pr_loaded(&mut self, number: u32) {
        if self.cache.get(number).is_some() {
            return;
        }
        if let Err(e) = self.cache.load_pr(number) {
            eprintln!("cache load #{number}: {e}");
        }
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
    if let Err(e) = app.cache.refresh_list() {
        st.list.status = format!("refresh failed: {e}");
    } else if let Some(prs) = app.cache.list.as_ref() {
        st.list.prs = prs.clone();
    }

    while st.running {
        term.draw(|f| draw(f, app, st))?;
        if event::poll(Duration::from_millis(250))? {
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
                let msg = ratatui::widgets::Paragraph::new("loading…")
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
}

fn handle_key(app: &mut App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    if st.focused == FocusedView::List
        && st.pending_g
        && ev.code == crossterm::event::KeyCode::Char('g')
    {
        st.pending_g = false;
        st.list.selected = 0;
        return;
    }
    st.pending_g = false;

    if st.focused == FocusedView::List {
        if let Some(buf) = st.list.search.as_mut() {
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
                app.ensure_pr_loaded(num);
                let pkg = app.cache.get(num);
                let files_count = pkg.map(|p| p.files.len()).unwrap_or(0);
                st.review = Some(PrReviewState {
                    file_index: 0,
                    cursor_line: 0,
                    scroll: 0,
                    show_commit_strip: app.config.show_commit_strip,
                    show_sha_margin: app.config.show_sha_margin,
                    status: format!("{} files", files_count),
                });
                st.focused = FocusedView::Review;
            }
        }
        Action::ListMerge => open_merge(st),
        Action::ListRefresh => {
            if let Err(e) = app.cache.refresh_list() {
                st.list.status = format!("refresh failed: {e}");
            } else if let Some(prs) = app.cache.list.as_ref() {
                st.list.prs = prs.clone();
            }
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
            }
        }
        Action::CursorUp => {
            if let Some(r) = st.review.as_mut() {
                r.cursor_line = r.cursor_line.saturating_sub(1);
            }
        }
        Action::HalfPageDown => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_add(10);
            }
        }
        Action::HalfPageUp => {
            if let Some(r) = st.review.as_mut() {
                r.scroll = r.scroll.saturating_sub(10);
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
            if let (Some(num), Some(_)) = (st.current_pr, st.review.as_ref()) {
                if let Some(pkg) = app.cache.get(num) {
                    st.picker = Some(FilePickerState {
                        query: String::new(),
                        all_files: pkg.files.iter().map(|f| f.path.clone()).collect(),
                        selected: 0,
                    });
                    st.focused = FocusedView::FilePicker;
                }
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
                let _ = app.cache.load_pr(num);
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
    let Some(pkg) = app.cache.get(num) else { return };
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
                    st.list.selected =
                        (st.list.selected + d as usize).min(n.saturating_sub(1));
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
