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
use crate::view::pr_list::{ExpandedFiles, PrListState};
use crate::view::pr_review::PrReviewState;

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

fn should_auto_refresh(
    focused: FocusedView,
    merging: bool,
    in_flight: bool,
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
    if in_flight {
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
    pub repo_root: std::path::PathBuf,
}

impl App {
    pub fn new(
        repo_root: std::path::PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        config: Config,
    ) -> Self {
        let worker = Worker::spawn(repo_root.clone(), gh, git, config.window_size);
        Self {
            cache: Cache::new(),
            config,
            worker,
            repo_root,
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
    pub list_refresh_in_flight: bool,
    /// Monotonically-incrementing refresh cycle id. Used to drop stale
    /// `ListFast`/`ListEnriched` responses from a superseded refresh.
    pub list_gen: u32,
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
                search: None,
                status: String::new(),
                loading: false,
                enriching: false,
                loading_stage: None,
                manual_refresh_in_flight: false,
                expanded: None,
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
            list_refresh_in_flight: false,
            list_gen: 0,
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

/// Trigger a fresh `ListFiles` request for whatever row is currently
/// selected. Always re-issues (no cache).
fn after_selection_change(app: &App, st: &mut AppState) {
    let Some((number, base_ref)) = st
        .list
        .visible_prs()
        .get(st.list.selected)
        .map(|p| (p.number, p.base_ref_name.clone()))
    else {
        st.list.expanded = None;
        return;
    };
    st.list.expanded = Some(ExpandedFiles::Loading { number });
    app.request(Request::ListFiles { number, base_ref });
}

fn send_refresh(app: &App, st: &mut AppState, silent: bool) {
    st.last_refresh_at = Some(Instant::now());
    st.list.expanded = None;
    st.list_refresh_in_flight = true;
    // Arm at refresh start so ListFast can't re-arm after ListEnriched clears.
    st.list.enriching = true;
    if !silent {
        st.list.loading = true;
        st.list.manual_refresh_in_flight = true;
    }
    // Seed the stage so the very first frame after a manual `r` already
    // shows what step is running. The worker's own ListProgress arrives a
    // moment later and may overwrite this — that's fine.
    st.list.loading_stage = Some(crate::data::worker::ListStage::FetchingList);
    st.list_gen = st.list_gen.wrapping_add(1);
    let g = st.list_gen;
    app.request(Request::RefreshList { generation: g });
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
            st.list_refresh_in_flight,
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

fn ensure_blame(app: &App, st: &mut AppState, number: u32, path: &str) {
    let Some(r) = st.review.as_mut() else { return };
    let Some(d) = r.detail.as_ref() else { return };
    if r.colors.contains_key(path) { return; }
    let commits: Vec<String> = d.commits.iter().map(|c| c.oid.clone()).collect();
    let head_oid = d.head_ref_oid.clone();
    let base_oid = d.base_ref_oid.clone();
    r.colors.insert(path.to_string(), crate::view::pr_review::ColorState::Loading);
    app.request(Request::BlameFile {
        number,
        head_oid,
        base_oid,
        path: path.to_string(),
        commits,
    });
}

fn handle_response(app: &mut App, st: &mut AppState, resp: Response) {
    match resp {
        Response::ListProgress { generation, stage } if generation == st.list_gen => {
            st.list.loading_stage = Some(stage);
        }
        Response::ListProgress { .. } => { /* stale; drop */ }
        Response::ListFast { generation, result } if generation == st.list_gen => match result {
            Ok(prs) => {
                let prev_selected = st
                    .list
                    .visible_prs()
                    .get(st.list.selected)
                    .map(|p| p.number);
                st.list.prs = prs.clone();
                app.cache.set_list(prs);
                st.list.loading = false;
                st.list.loading_stage = None;
                st.list.status = String::new();
                let new_numbers: Vec<u32> = st
                    .list
                    .visible_prs()
                    .iter()
                    .map(|p| p.number)
                    .collect();
                st.list.selected =
                    reselect_by_number(prev_selected, &new_numbers, st.list.selected);
                st.list.expanded = None;
                after_selection_change(app, st);
            }
            Err(e) => {
                st.list_refresh_in_flight = false;
                st.list.enriching = false;
                st.list.loading = false;
                st.list.loading_stage = None;
                st.list.manual_refresh_in_flight = false;
                st.list.status = format!("refresh failed: {e}");
            }
        },
        Response::ListFast { .. } => { /* stale; drop */ }
        Response::ListEnriched { generation, result } if generation == st.list_gen => {
            st.list_refresh_in_flight = false;
            st.list.enriching = false;
            st.list.loading_stage = None;
            st.list.manual_refresh_in_flight = false;
            if let Ok(es) = result {
                for e in &es {
                    if let Some(p) =
                        st.list.prs.iter_mut().find(|p| p.number == e.number)
                    {
                        p.apply_enrichment(e);
                    }
                }
            }
            // Enrichment errors are non-fatal: rows already render with
            // light-fields-only glyphs.
        }
        Response::ListEnriched { .. } => { /* stale; drop */ }
        Response::PrDetail { number, result: Ok(detail) } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                let zero_stats = detail
                    .commits
                    .iter()
                    .map(|c| (c.oid.clone(), crate::render::attribution::CommitStats::default()))
                    .collect();
                r.detail = Some(detail);
                r.commit_stats = zero_stats;
                r.status = "loading diff…".into();
            }
        }
        Response::PrDetail { number, result: Err(e) } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("load failed: {e}");
            }
            st.list.status = format!("load #{number} failed: {e}");
        }
        Response::PrDiff { number, result: Ok(files) } => {
            if st.current_pr == Some(number) {
                let path = if let Some(r) = st.review.as_mut() {
                    r.files = files;
                    r.status = format!("{} files", r.files.len());
                    r.files.get(r.file_index).map(|f| f.path.clone())
                } else {
                    None
                };
                if let Some(path) = path {
                    ensure_blame(app, st, number, &path);
                }
            }
        }
        Response::PrDiff { number, result: Err(e) } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("diff failed: {e}");
            }
        }
        Response::PrFileColors {
            number,
            head_oid: _,
            path,
            colors,
            stats,
        } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.colors.insert(path, crate::view::pr_review::ColorState::Ready(colors));
                for (oid, s) in stats {
                    let entry = r.commit_stats.entry(oid).or_default();
                    entry.adds += s.adds;
                    entry.dels += s.dels;
                }
            }
        }
        Response::PrLoadError { number, error } => {
            if let Some(r) = st.review.as_mut()
                && st.current_pr == Some(number)
            {
                r.status = format!("load failed: {error}");
            }
            st.list.status = format!("load #{number} failed: {error}");
        }
        Response::MergeDone { number, result: Ok(()) } => {
            // Remove the merged PR locally. No network refresh — fresh data
            // only arrives via startup, manual refresh, or auto-refresh.
            let prev_selected = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number);
            let prev_idx = st.list.selected;
            st.focused = FocusedView::List;
            st.review = None;
            st.current_pr = None;
            st.merge = None;
            st.merging = None;
            st.picker = None;
            st.list.status = format!("merged #{number}");
            st.list.prs.retain(|p| p.number != number);
            let new_numbers: Vec<u32> =
                st.list.visible_prs().iter().map(|p| p.number).collect();
            st.list.selected = reselect_by_number(prev_selected, &new_numbers, prev_idx);
            st.list.expanded = None;
            after_selection_change(app, st);
        }
        Response::MergeDone { number, result: Err(e) } => {
            st.merging = None;
            st.list.status = format!("merge #{number} failed: {e}");
        }
        Response::ListFiles { number, result } => {
            let sel_number = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number);
            if sel_number != Some(number) {
                return;
            }
            let exp_number = st.list.expanded.as_ref().map(ExpandedFiles::number);
            if exp_number != Some(number) {
                return;
            }
            st.list.expanded = Some(match result {
                Ok(files) => ExpandedFiles::Ready { number, files },
                Err(e) => ExpandedFiles::Error { number, message: format!("{e:#}") },
            });
        }
    }
}

fn draw(f: &mut ratatui::Frame, _app: &App, st: &AppState) {
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
            if let Some(review) = st.review.as_ref() {
                if review.detail.is_some() {
                    crate::view::pr_review::render(f, area, review);
                } else {
                    let text = format!("{} loading…", crate::render::spinner::glyph());
                    let msg = ratatui::widgets::Paragraph::new(text)
                        .style(ratatui::style::Style::default().fg(crate::render::style::OVERLAY1))
                        .alignment(ratatui::layout::Alignment::Center);
                    f.render_widget(msg, area);
                }
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

    // While the user-initiated list refresh is in flight, the view is
    // blocked — rows are hidden behind a loading placeholder and acting
    // on stale state would be confusing. Keep the same Ctrl-C escape
    // hatch as the merge-in-flight branch above.
    if st.list.manual_refresh_in_flight {
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
        after_selection_change(app, st);
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
        after_selection_change(app, st);
        return;
    }

    let action = dispatch(st.focused, ev);
    handle_action(app, st, action);
}

fn handle_action(app: &mut App, st: &mut AppState, action: Action) {
    match action {
        Action::Quit => st.running = false,
        Action::ListUp => {
            if st.list.selected > 0 {
                st.list.selected -= 1;
                after_selection_change(app, st);
            }
        }
        Action::ListDown => {
            let n = st.list.visible_prs().len();
            if st.list.selected + 1 < n {
                st.list.selected += 1;
                after_selection_change(app, st);
            }
        }
        Action::ListTop => {
            st.pending_g = true;
        }
        Action::ListBottom => {
            let n = st.list.visible_prs().len();
            st.list.selected = n.saturating_sub(1);
            after_selection_change(app, st);
        }
        Action::ListOpen => {
            if let Some(pr) = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| (*p).clone())
            {
                let num = pr.number;
                st.current_pr = Some(num);
                st.review = Some(PrReviewState {
                    file_index: 0,
                    cursor_line: 0,
                    scroll: 0,
                    show_sha_margin: app.config.show_sha_margin,
                    status: "loading…".into(),
                    ..Default::default()
                });
                st.focused = FocusedView::Review;
                app.request(Request::OpenPr(pr));
            }
        }
        Action::ListOpenInBrowser => {
            if let Some(num) = st
                .list
                .visible_prs()
                .get(st.list.selected)
                .map(|p| p.number)
            {
                open_pr_in_browser(&app.repo_root, num);
            }
        }
        Action::ListMerge => open_merge(st),
        Action::ListRefresh => {
            send_refresh(app, st, false);
        }
        Action::ListSearch => {
            st.list.search = Some(String::new());
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
            if let Some(r) = st.review.as_mut()
                && let Some(file) = r.files.get(r.file_index)
            {
                r.scroll = max_scroll(file.lines.len());
                r.cursor_line = max_cursor_line(file);
            }
        }
        Action::NextFile => cycle_file(app, st, 1),
        Action::PrevFile => cycle_file(app, st, -1),
        Action::OpenFilePicker => {
            if let Some(r) = st.review.as_ref() {
                let paths_vec = crate::view::pr_review::file_paths(r);
                let current = paths_vec.get(r.file_index).copied();
                let paths: Vec<String> = paths_vec.into_iter().map(String::from).collect();
                st.picker = Some(FilePickerState::new(paths, current));
                st.focused = FocusedView::FilePicker;
            }
        }
        Action::OpenCommitsModal => {
            if let Some(r) = st.review.as_ref()
                && let Some(d) = r.detail.as_ref()
            {
                let rows = commits_modal::build_rows(
                    &d.commits,
                    &r.commit_stats,
                    app.config.window_size,
                    Utc::now(),
                );
                st.commits = Some(CommitsModalState {
                    rows,
                    selected: 0,
                    ..Default::default()
                });
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
        }
        Action::Help => {
            st.focused = FocusedView::HelpOverlay;
        }
        Action::Refresh => {
            if let Some(num) = st.current_pr
                && let Some(pr) = st.list.prs.iter().find(|p| p.number == num).cloned()
            {
                if let Some(r) = st.review.as_mut() {
                    r.detail = None;
                    r.files.clear();
                    r.colors.clear();
                    r.commit_stats.clear();
                    r.status = "loading…".into();
                }
                app.request(Request::OpenPr(pr));
            }
        }
        Action::Nothing => {}
    }
}

fn open_pr_in_browser(repo_root: &std::path::Path, number: u32) {
    let _ = std::process::Command::new("gh")
        .current_dir(repo_root)
        .args(["pr", "view", "--web", &number.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
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
fn move_review(_app: &App, st: &mut AppState, delta: i32) {
    let Some(r) = st.review.as_mut() else { return };
    let Some(file) = r.files.get(r.file_index) else { return };
    let max_scr = max_scroll(file.lines.len()) as i64;
    let max_cur = max_cursor_line(file) as i64;
    let new_scroll = (r.scroll as i64 + delta as i64).clamp(0, max_scr);
    let new_cursor = (r.cursor_line as i64 + delta as i64).clamp(0, max_cur);
    r.scroll = new_scroll as u16;
    r.cursor_line = new_cursor as usize;
}

fn cycle_file(app: &App, st: &mut AppState, delta: i32) {
    let Some(num) = st.current_pr else { return };
    let path_for_blame = {
        let Some(r) = st.review.as_mut() else { return };
        let n = crate::view::pr_review::file_count(r) as i32;
        if n == 0 { return; }
        let new_idx = ((r.file_index as i32 + delta).rem_euclid(n)) as usize;
        r.file_index = new_idx;
        r.cursor_line = 0;
        r.scroll = 0;
        r.files.get(new_idx).map(|f| f.path.clone())
    };
    if let Some(path) = path_for_blame {
        ensure_blame(app, st, num, &path);
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
                let prev = st.list.selected;
                if d > 0 {
                    st.list.selected = (st.list.selected + d as usize).min(n.saturating_sub(1));
                } else {
                    st.list.selected = st.list.selected.saturating_sub((-d) as usize);
                }
                if st.list.selected != prev {
                    after_selection_change(app, st);
                }
            } else {
                move_review(app, st, d as i32);
            }
        }
        MouseAction::ClickAt { col: _, row } => {
            if st.focused == FocusedView::List && row >= 2 {
                let idx = (row - 2) as usize;
                if idx < st.list.visible_prs().len() && st.list.selected != idx {
                    st.list.selected = idx;
                    after_selection_change(app, st);
                }
            }
        }
        MouseAction::DoubleClickAt { .. } | MouseAction::Nothing => {}
    }
}

fn handle_file_picker(app: &App, st: &mut AppState, ev: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some(picker) = st.picker.as_mut() else {
        return;
    };

    // Keys that work the same in vim mode and filter mode: directional keys
    // and confirm/quit on Enter. We handle those first so the per-mode
    // branches don't have to repeat them.
    let n = picker.matches().len();
    match ev.code {
        KeyCode::Enter => {
            let chosen = picker.matches().get(picker.selected).map(|s| (*s).clone());
            let blame_target = if let (Some(path), Some(r)) = (chosen, st.review.as_mut()) {
                let idx = crate::view::pr_review::file_paths(r)
                    .iter()
                    .position(|p| *p == path.as_str());
                if let Some(idx) = idx {
                    r.file_index = idx;
                    r.cursor_line = 0;
                    r.scroll = 0;
                    r.files.get(idx).map(|f| f.path.clone())
                } else { None }
            } else { None };
            if let (Some(path), Some(num)) = (blame_target, st.current_pr) {
                ensure_blame(app, st, num, &path);
            }
            st.picker = None;
            st.focused = FocusedView::Review;
            return;
        }
        KeyCode::Down => {
            picker.move_down(n);
            picker.pending_g = false;
            return;
        }
        KeyCode::Up => {
            picker.move_up();
            picker.pending_g = false;
            return;
        }
        KeyCode::PageDown => {
            picker.page_down(10, n);
            picker.pending_g = false;
            return;
        }
        KeyCode::PageUp => {
            picker.page_up(10);
            picker.pending_g = false;
            return;
        }
        KeyCode::Home => {
            picker.to_top();
            picker.pending_g = false;
            return;
        }
        KeyCode::End => {
            picker.to_bottom(n);
            picker.pending_g = false;
            return;
        }
        KeyCode::Char('d') | KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
            if ev.code == KeyCode::Char('d') {
                picker.page_down(10, n);
            } else {
                picker.page_up(10);
            }
            picker.pending_g = false;
            return;
        }
        _ => {}
    }

    if picker.filter_active {
        match ev.code {
            KeyCode::Esc => picker.exit_filter_reset(),
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
        return;
    }

    // Vim navigation mode.
    match ev.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            st.picker = None;
            st.focused = FocusedView::Review;
        }
        KeyCode::Char('/') => picker.enter_filter(),
        KeyCode::Char('j') => {
            picker.move_down(n);
            picker.pending_g = false;
        }
        KeyCode::Char('k') => {
            picker.move_up();
            picker.pending_g = false;
        }
        KeyCode::Char('G') => {
            picker.to_bottom(n);
            picker.pending_g = false;
        }
        KeyCode::Char('g') => {
            if picker.pending_g {
                picker.to_top();
                picker.pending_g = false;
            } else {
                picker.pending_g = true;
            }
        }
        _ => {
            picker.pending_g = false;
        }
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

    let n = modal.matches().len();
    // Keys shared by both modes: directional input and Enter-as-close.
    match ev.code {
        KeyCode::Enter => {
            st.commits = None;
            st.focused = FocusedView::Review;
            return;
        }
        KeyCode::Down => {
            modal.move_down(n);
            modal.pending_g = false;
            return;
        }
        KeyCode::Up => {
            modal.move_up();
            modal.pending_g = false;
            return;
        }
        KeyCode::PageDown => {
            modal.page_down(10, n);
            modal.pending_g = false;
            return;
        }
        KeyCode::PageUp => {
            modal.page_up(10);
            modal.pending_g = false;
            return;
        }
        KeyCode::Home => {
            modal.to_top();
            modal.pending_g = false;
            return;
        }
        KeyCode::End => {
            modal.to_bottom(n);
            modal.pending_g = false;
            return;
        }
        KeyCode::Char('d') | KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
            if ev.code == KeyCode::Char('d') {
                modal.page_down(10, n);
            } else {
                modal.page_up(10);
            }
            modal.pending_g = false;
            return;
        }
        _ => {}
    }

    if modal.filter_active {
        match ev.code {
            KeyCode::Esc => modal.exit_filter_reset(),
            KeyCode::Backspace => {
                modal.query.pop();
                modal.selected = 0;
            }
            KeyCode::Char(c) => {
                modal.query.push(c);
                modal.selected = 0;
            }
            _ => {}
        }
        return;
    }

    // Vim navigation mode.
    match ev.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') | KeyCode::Char('C') => {
            st.commits = None;
            st.focused = FocusedView::Review;
        }
        KeyCode::Char('/') => modal.enter_filter(),
        KeyCode::Char('j') => {
            modal.move_down(n);
            modal.pending_g = false;
        }
        KeyCode::Char('k') => {
            modal.move_up();
            modal.pending_g = false;
        }
        KeyCode::Char('G') => {
            modal.to_bottom(n);
            modal.pending_g = false;
        }
        KeyCode::Char('g') => {
            if modal.pending_g {
                modal.to_top();
                modal.pending_g = false;
            } else {
                modal.pending_g = true;
            }
        }
        _ => {
            modal.pending_g = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use crate::data::cache::Cache;
    use crate::data::pr::{Author, FileMeta, Pr, PrEnrichment, PrState, StatusCheck};
    use crate::data::worker::Response;
    use crate::view::pr_list::ExpandedFiles;

    fn make_pr(n: u32) -> Pr {
        Pr {
            number: n,
            title: format!("pr-{n}"),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: format!("f{n}"),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    fn make_app() -> App {
        use crate::data::gh::fakes::FakeGh;
        use crate::data::git::fakes::FakeGit;
        App::new(
            "/tmp/repo".into(),
            Arc::new(FakeGh::new()),
            Arc::new(FakeGit::new("/tmp/repo")),
            Config::default(),
        )
    }

    #[test]
    fn list_files_response_matching_selection_transitions_to_ready() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7), make_pr(8)];
        st.list.selected = 0;
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        let files = vec![FileMeta { path: "a.rs".into(), additions: 1, deletions: 0 }];
        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 7, result: Ok(files.clone()) },
        );
        match st.list.expanded {
            Some(ExpandedFiles::Ready { number: 7, files: ref f }) => assert_eq!(f, &files),
            ref other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn list_files_response_for_other_pr_is_dropped() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7), make_pr(8)];
        st.list.selected = 0;
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 8, result: Ok(vec![]) },
        );
        assert!(matches!(
            st.list.expanded,
            Some(ExpandedFiles::Loading { number: 7 })
        ));
    }

    #[test]
    fn list_files_error_transitions_to_error_variant() {
        let mut app = make_app();
        let mut st = AppState::new("prpr".into(), "main".into());
        st.list.prs = vec![make_pr(7)];
        st.list.selected = 0;
        st.list.expanded = Some(ExpandedFiles::Loading { number: 7 });

        handle_response(
            &mut app,
            &mut st,
            Response::ListFiles { number: 7, result: Err(anyhow::anyhow!("ref missing")) },
        );
        match st.list.expanded {
            Some(ExpandedFiles::Error { number: 7, ref message }) => {
                assert!(message.contains("ref missing"));
            }
            ref other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn cycle_file_uses_detail_files_count_when_files_empty() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let json = include_str!("../tests/fixtures/pr_view.json");
        let detail: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        let n_detail_files = detail.files.len();
        let number = detail.number;
        let app = test_app_for_state(&mut cache);
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
            ..Default::default()
        });

        cycle_file(&app, &mut st, 1);
        assert_eq!(st.review.as_ref().unwrap().file_index, 1 % n_detail_files);

        // Wrap to last.
        cycle_file(&app, &mut st, -2);
        let expected = ((1i32 - 2).rem_euclid(n_detail_files as i32)) as usize;
        assert_eq!(st.review.as_ref().unwrap().file_index, expected);
    }

    #[test]
    fn move_review_is_noop_when_files_not_yet_parsed() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let json = include_str!("../tests/fixtures/pr_view.json");
        let detail: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        let number = detail.number;
        let app = test_app_for_state(&mut cache);
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            file_index: 0,
            cursor_line: 0,
            scroll: 0,
            show_sha_margin: false,
            status: String::new(),
            ..Default::default()
        });
        move_review(&app, &mut st, 10);
        let r = st.review.as_ref().unwrap();
        assert_eq!(r.cursor_line, 0);
        assert_eq!(r.scroll, 0);
    }

    #[test]
    fn cycle_file_reads_files_from_review_state_not_cache() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2, "fixture needs at least 2 files for this test");

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: files.clone(),
            file_index: 0,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        assert_eq!(st.review.as_ref().unwrap().file_index, 1);
    }

    fn dummy_app_state() -> AppState {
        AppState::new("repo".into(), "main".into())
    }

    fn fixture_pr_detail() -> crate::data::pr::PrDetail {
        let json = include_str!("../tests/fixtures/pr_view.json");
        serde_json::from_str(json).unwrap()
    }

    fn open_pr(n: u32) -> Pr {
        Pr {
            number: n,
            title: format!("#{n}"),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: format!("feature-{n}"),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    fn test_app_for_state(cache: &mut Cache) -> App {
        use crate::data::gh::fakes::FakeGh;
        use crate::data::git::fakes::FakeGit;
        let gh: std::sync::Arc<dyn crate::data::gh::GhClient> = std::sync::Arc::new(FakeGh::new());
        let git: std::sync::Arc<dyn crate::data::git::GitClient> =
            std::sync::Arc::new(FakeGit::new("/tmp/repo"));
        let mut app = App::new("/tmp/repo".into(), gh, git, Config::default());
        std::mem::swap(&mut app.cache, cache);
        app
    }

    #[test]
    fn list_progress_updates_stage_then_clears_on_list_fast() {
        use crate::data::worker::ListStage;

        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 3;
        // Worker reports the first stage.
        handle_response(
            &mut app,
            &mut st,
            Response::ListProgress {
                generation: 3,
                stage: ListStage::FetchingList,
            },
        );
        assert_eq!(st.list.loading_stage, Some(ListStage::FetchingList));
        // Then the second stage replaces it.
        handle_response(
            &mut app,
            &mut st,
            Response::ListProgress {
                generation: 3,
                stage: ListStage::FetchingRefs,
            },
        );
        assert_eq!(st.list.loading_stage, Some(ListStage::FetchingRefs));
        // ListFast clears the stage so the body can render rows.
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                generation: 3,
                result: Ok(vec![]),
            },
        );
        assert_eq!(st.list.loading_stage, None);
    }

    #[test]
    fn stale_list_progress_is_dropped() {
        use crate::data::worker::ListStage;

        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 5;
        st.list.loading_stage = Some(ListStage::FetchingList);
        // A leftover progress event from an older cycle must not stomp on
        // the current cycle's stage.
        handle_response(
            &mut app,
            &mut st,
            Response::ListProgress {
                generation: 1,
                stage: ListStage::FetchingRefs,
            },
        );
        assert_eq!(st.list.loading_stage, Some(ListStage::FetchingList));
    }

    #[test]
    fn list_fast_error_clears_stage() {
        use crate::data::worker::ListStage;

        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 2;
        st.list.loading = true;
        st.list.loading_stage = Some(ListStage::FetchingRefs);
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                generation: 2,
                result: Err(anyhow::anyhow!("boom")),
            },
        );
        assert_eq!(st.list.loading_stage, None);
        assert!(st.list.status.starts_with("refresh failed"));
    }

    #[test]
    fn stale_list_fast_is_dropped() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 5;
        // A response from a much older generation arrives.
        let stale = Response::ListFast {
            generation: 1,
            result: Ok(vec![open_pr(1)]),
        };
        handle_response(&mut app, &mut st, stale);
        // Nothing applied: rows still empty.
        assert!(st.list.prs.is_empty());
    }

    #[test]
    fn stale_list_enriched_is_dropped() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 5;
        st.list_refresh_in_flight = true;
        st.list.enriching = true;
        // Seed a row so we'd notice if enrichment was applied.
        st.list.prs = vec![open_pr(7)];
        let stale = Response::ListEnriched {
            generation: 1,
            result: Ok(vec![PrEnrichment {
                number: 7,
                status_check_rollup: vec![StatusCheck {
                    status: Some("COMPLETED".into()),
                    conclusion: Some("FAILURE".into()),
                }],
                review_decision: None,
                mergeable: Some("CONFLICTING".into()),
            }]),
        };
        handle_response(&mut app, &mut st, stale);
        // Row not enriched (stale generation dropped).
        assert!(st.list.prs[0].status_check_rollup.is_empty());
        assert!(st.list.prs[0].mergeable.is_none());
        // In-flight flags not cleared either.
        assert!(st.list_refresh_in_flight);
        assert!(st.list.enriching);
    }

    #[test]
    fn enrichment_merges_by_number() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                generation: 1,
                result: Ok(vec![open_pr(7), open_pr(8)]),
            },
        );
        handle_response(
            &mut app,
            &mut st,
            Response::ListEnriched {
                generation: 1,
                result: Ok(vec![PrEnrichment {
                    number: 7,
                    status_check_rollup: vec![StatusCheck {
                        status: Some("COMPLETED".into()),
                        conclusion: Some("FAILURE".into()),
                    }],
                    review_decision: None,
                    mergeable: Some("CONFLICTING".into()),
                }]),
            },
        );
        let by_num: std::collections::HashMap<u32, &Pr> = st
            .list
            .prs
            .iter()
            .map(|p| (p.number, p))
            .collect();
        assert_eq!(by_num[&7].status_check_rollup.len(), 1);
        assert_eq!(by_num[&7].mergeable.as_deref(), Some("CONFLICTING"));
        assert!(by_num[&8].status_check_rollup.is_empty());
    }

    #[test]
    fn list_refresh_in_flight_clears_only_after_enriched() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        st.list_refresh_in_flight = true;
        st.list.enriching = true;
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast {
                generation: 1,
                result: Ok(vec![]),
            },
        );
        // After fast, still in flight, still enriching.
        assert!(st.list_refresh_in_flight);
        assert!(st.list.enriching);
        handle_response(
            &mut app,
            &mut st,
            Response::ListEnriched {
                generation: 1,
                result: Ok(vec![]),
            },
        );
        assert!(!st.list_refresh_in_flight);
        assert!(!st.list.enriching);
    }

    #[test]
    fn enriching_clears_when_enriched_arrives_before_fast() {
        // ListEnriched can beat ListFast; footer must not stick on enriching.
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        send_refresh(&app, &mut st, false);
        let g = st.list_gen;
        handle_response(
            &mut app,
            &mut st,
            Response::ListEnriched { generation: g, result: Ok(vec![]) },
        );
        handle_response(
            &mut app,
            &mut st,
            Response::ListFast { generation: g, result: Ok(vec![]) },
        );
        assert!(!st.list.enriching);
    }

    #[test]
    fn auto_refresh_blocked_when_not_on_list() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::Review,
            false,
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
            false,
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
            false,
            last,
            now,
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn auto_refresh_blocked_when_in_flight() {
        let now = Instant::now();
        let last = Some(now - Duration::from_secs(120));
        assert!(!should_auto_refresh(
            FocusedView::List,
            false,
            true,
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

    #[test]
    fn end_to_end_open_pr_progresses_through_partial_states() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();

        let gh = crate::data::gh::fakes::FakeGh::new();
        let mut git = crate::data::git::fakes::FakeGit::new("/tmp/repo");
        git.refs.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs.insert(format!("origin/{}", detail.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut app = App::new(
            "/tmp/repo".into(),
            std::sync::Arc::new(gh),
            std::sync::Arc::new(git),
            crate::config::Config::default(),
        );
        let mut st = AppState::new("repo".into(), "main".into());
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            status: "loading…".into(),
            ..Default::default()
        });

        let pr = crate::data::pr::Pr {
            number: detail.number,
            title: detail.title.clone(),
            is_draft: detail.is_draft,
            state: detail.state,
            author: detail.author.clone(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: detail.base_ref_name.clone(),
            head_ref_name: detail.head_ref_name.clone(),
            labels: vec![],
            status_check_rollup: detail.status_check_rollup.clone(),
            review_decision: detail.review_decision,
            mergeable: detail.mergeable.clone(),
        };
        app.request(Request::OpenPr(pr));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut saw_detail = false;
        let mut saw_diff = false;
        while std::time::Instant::now() < deadline && !(saw_detail && saw_diff) {
            if let Ok(resp) = app.worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                let is_detail = matches!(resp, Response::PrDetail { .. });
                let is_diff = matches!(resp, Response::PrDiff { .. });
                handle_response(&mut app, &mut st, resp);
                if is_detail {
                    saw_detail = true;
                    let r = st.review.as_ref().unwrap();
                    assert!(r.detail.is_some());
                    assert_eq!(r.status, "loading diff…");
                }
                if is_diff {
                    saw_diff = true;
                    let r = st.review.as_ref().unwrap();
                    assert!(!r.files.is_empty());
                    assert_eq!(r.status, format!("{} files", r.files.len()));
                }
            }
        }
        assert!(saw_detail && saw_diff, "missed an event");
    }

    #[test]
    fn merge_done_ok_removes_pr_locally_without_refresh() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        st.list.prs = vec![open_pr(5), open_pr(7), open_pr(8)];
        st.list.selected = 1; // pointing at #7
        st.current_pr = Some(7);
        let prior_gen = st.list_gen;
        let prior_last_refresh = st.last_refresh_at;

        handle_response(
            &mut app,
            &mut st,
            Response::MergeDone { number: 7, result: Ok(()) },
        );

        let nums: Vec<u32> = st.list.prs.iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![5, 8], "merged PR removed locally");
        // No network refresh triggered.
        assert!(!st.list_refresh_in_flight);
        assert!(!st.list.loading);
        assert!(!st.list.enriching);
        assert_eq!(st.list_gen, prior_gen, "no new refresh generation");
        assert_eq!(st.last_refresh_at, prior_last_refresh);
        // UI returns to list, transient state cleared.
        assert_eq!(st.focused, FocusedView::List);
        assert!(st.review.is_none());
        assert!(st.current_pr.is_none());
        assert!(st.merge.is_none());
        assert!(st.merging.is_none());
        assert!(st.list.status.contains("merged #7"));
        // Selection follows: was on #7, falls onto next visible row (#8 at idx 1).
        assert_eq!(st.list.selected, 1);
    }

    #[test]
    fn merge_done_clamps_selection_when_last_row_merged() {
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        st.list.prs = vec![open_pr(5), open_pr(7), open_pr(8)];
        st.list.selected = 2; // pointing at #8 (last)

        handle_response(
            &mut app,
            &mut st,
            Response::MergeDone { number: 8, result: Ok(()) },
        );

        let nums: Vec<u32> = st.list.prs.iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![5, 7]);
        assert_eq!(st.list.selected, 1, "selection clamped to last remaining row");
    }

    #[test]
    fn merge_done_keeps_selection_when_other_pr_merged() {
        // Merge initiated from a review of a non-selected PR. The list
        // selection should follow its PR by number, not by index.
        let mut st = dummy_app_state();
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);
        st.list_gen = 1;
        st.list.prs = vec![open_pr(5), open_pr(7), open_pr(8)];
        st.list.selected = 0; // pointing at #5
        st.current_pr = Some(7); // but viewing #7 in review

        handle_response(
            &mut app,
            &mut st,
            Response::MergeDone { number: 7, result: Ok(()) },
        );

        let nums: Vec<u32> = st.list.prs.iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![5, 8]);
        // #5 is still at index 0.
        assert_eq!(st.list.selected, 0);
    }

    #[test]
    fn pr_detail_response_populates_review_state_detail() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            status: "loading…".into(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDetail { number, result: Ok(detail.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.detail.as_ref().unwrap().number, number);
        assert_eq!(r.commit_stats.len(), detail.commits.len(),
            "commit_stats zero-filled for every PR commit");
    }

    #[test]
    fn pr_diff_response_populates_review_state_files() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            status: "loading diff…".into(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDiff { number, result: Ok(files.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.files.len(), files.len());
    }

    #[test]
    fn pr_diff_response_dispatches_blame_file_for_current_file_and_marks_loading() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let first_path = files[0].path.clone();

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrDiff { number, result: Ok(files.clone()) },
        );

        let r = st.review.as_ref().unwrap();
        assert!(matches!(
            r.colors.get(&first_path),
            Some(crate::view::pr_review::ColorState::Loading)
        ));
    }

    #[test]
    fn cycle_file_marks_new_file_loading_when_not_yet_blamed() {
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2);
        let second_path = files[1].path.clone();

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            files: files.clone(),
            file_index: 0,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        let r = st.review.as_ref().unwrap();
        assert_eq!(r.file_index, 1);
        assert!(matches!(
            r.colors.get(&second_path),
            Some(crate::view::pr_review::ColorState::Loading)
        ));
    }

    #[test]
    fn cycle_file_does_not_remark_loading_or_ready_file() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        assert!(files.len() >= 2);
        let second_path = files[1].path.clone();

        let ready = crate::view::pr_review::ColorState::Ready(LineColors {
            head: vec![], delete: std::collections::HashMap::new(),
        });
        let mut colors = std::collections::HashMap::new();
        colors.insert(second_path.clone(), ready);

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail),
            files,
            colors,
            file_index: 0,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let app = test_app_for_state(&mut cache);

        cycle_file(&app, &mut st, 1);

        let r = st.review.as_ref().unwrap();
        assert!(matches!(
            r.colors.get(&second_path),
            Some(crate::view::pr_review::ColorState::Ready(_))
        ), "ready entry must not be reset to Loading");
    }

    #[test]
    fn refresh_action_in_review_clears_data_and_dispatches_open_pr() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let pr = crate::data::pr::Pr {
            number, title: detail.title.clone(), is_draft: detail.is_draft,
            state: detail.state, author: detail.author.clone(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: detail.base_ref_name.clone(),
            head_ref_name: detail.head_ref_name.clone(),
            labels: vec![], status_check_rollup: detail.status_check_rollup.clone(),
            review_decision: detail.review_decision, mergeable: detail.mergeable.clone(),
        };

        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.list.prs = vec![pr.clone()];
        let mut colors = std::collections::HashMap::new();
        colors.insert("src/sched.rs".into(), crate::view::pr_review::ColorState::Ready(
            LineColors { head: vec![], delete: std::collections::HashMap::new() }
        ));
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: vec![crate::data::diff::FileDiff {
                path: "src/sched.rs".into(),
                lines: vec![], binary: false,
            }],
            colors,
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_action(&mut app, &mut st, Action::Refresh);

        let r = st.review.as_ref().unwrap();
        assert!(r.detail.is_none());
        assert!(r.files.is_empty());
        assert!(r.colors.is_empty());
        assert!(r.commit_stats.is_empty());
        assert_eq!(r.status, "loading…");
    }

    #[test]
    fn pr_file_colors_response_marks_path_ready_in_review() {
        use crate::render::attribution::LineColors;
        let detail = fixture_pr_detail();
        let number = detail.number;
        let head_oid = detail.head_ref_oid.clone();
        let files = crate::data::diff::parse_diff(
            include_str!("../tests/fixtures/diff_basic.patch")
        ).unwrap();
        let path = files[0].path.clone();
        let mut st = dummy_app_state();
        st.current_pr = Some(number);
        st.review = Some(PrReviewState {
            detail: Some(detail.clone()),
            files: files.clone(),
            ..Default::default()
        });
        let mut cache = Cache::new();
        let mut app = test_app_for_state(&mut cache);

        handle_response(
            &mut app,
            &mut st,
            Response::PrFileColors {
                number,
                head_oid: head_oid.clone(),
                path: path.clone(),
                colors: LineColors { head: vec![], delete: std::collections::HashMap::new() },
                stats: std::collections::HashMap::new(),
            },
        );

        let r = st.review.as_ref().unwrap();
        assert!(matches!(r.colors.get(&path), Some(crate::view::pr_review::ColorState::Ready(_))));
    }
}
