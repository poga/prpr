//! Key dispatch. Pure logic: given the current view and a key event,
//! return an `Action`. The event loop interprets actions.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Quit,

    // PR list
    ListUp,
    ListDown,
    ListTop,
    ListBottom,
    ListOpen,
    ListMerge,
    ListRefresh,
    ListSearch,
    ListCycleFilter,
    ListClearFilter,

    // PR review
    CursorUp,
    CursorDown,
    HalfPageUp,
    HalfPageDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    NextFile,
    PrevFile,
    OpenFilePicker,
    OpenCommitsModal,
    Merge,
    ToggleShaMargin,
    BackToList,

    // Global
    Help,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedView {
    List,
    Review,
    HelpOverlay,
    FilePicker,
    MergeModal,
    CommitsModal,
}

pub fn dispatch(view: FocusedView, ev: KeyEvent) -> Action {
    if ev.code == KeyCode::Char('c') && ev.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit;
    }
    match view {
        FocusedView::List => list(ev),
        FocusedView::Review => review(ev),
        FocusedView::HelpOverlay => Action::Nothing, // swallowed by caller
        FocusedView::FilePicker | FocusedView::MergeModal | FocusedView::CommitsModal => Action::Nothing, // overlay impls
    }
}

fn list(ev: KeyEvent) -> Action {
    match ev.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('r') => Action::ListRefresh,
        KeyCode::Char('j') | KeyCode::Down => Action::ListDown,
        KeyCode::Char('k') | KeyCode::Up => Action::ListUp,
        KeyCode::Char('G') => Action::ListBottom,
        KeyCode::Char('g') => Action::ListTop, // second `g` handled by stateful caller
        KeyCode::Enter => Action::ListOpen,
        KeyCode::Char('m') => Action::ListMerge,
        KeyCode::Char('/') => Action::ListSearch,
        KeyCode::Char('f') => Action::ListCycleFilter,
        KeyCode::Esc => Action::ListClearFilter,
        _ => Action::Nothing,
    }
}

fn review(ev: KeyEvent) -> Action {
    match ev.code {
        KeyCode::Char('q') | KeyCode::Esc => Action::BackToList,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('r') => Action::Refresh,
        KeyCode::Char('j') | KeyCode::Down => Action::CursorDown,
        KeyCode::Char('k') | KeyCode::Up => Action::CursorUp,
        KeyCode::Char('d') if ev.modifiers.contains(KeyModifiers::CONTROL) => Action::HalfPageDown,
        KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => Action::HalfPageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::Home => Action::Top,
        KeyCode::End => Action::Bottom,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::Char('g') => Action::Top,
        KeyCode::Tab | KeyCode::Enter => Action::NextFile,
        KeyCode::BackTab => Action::PrevFile,
        KeyCode::Char('f') => Action::OpenFilePicker,
        KeyCode::Char('m') => Action::Merge,
        KeyCode::Char('c') => Action::OpenCommitsModal,
        KeyCode::Char('s') => Action::ToggleShaMargin,
        _ => Action::Nothing,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseAction {
    Nothing,
    /// Scroll the focused region by `delta` (negative = up).
    Scroll(i16),
    /// Move selection / cursor to the given cell coordinates.
    ClickAt {
        col: u16,
        row: u16,
    },
    /// Treat as the same as Enter (open / confirm).
    DoubleClickAt {
        col: u16,
        row: u16,
    },
}

pub fn mouse_dispatch(ev: MouseEvent) -> MouseAction {
    match ev.kind {
        MouseEventKind::ScrollUp => MouseAction::Scroll(-3),
        MouseEventKind::ScrollDown => MouseAction::Scroll(3),
        MouseEventKind::Down(MouseButton::Left) => MouseAction::ClickAt {
            col: ev.column,
            row: ev.row,
        },
        // crossterm doesn't natively report double-click; the event loop
        // detects it by timing two ClickAt events on the same cell.
        _ => MouseAction::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn k_ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn list_q_quits() {
        assert_eq!(dispatch(FocusedView::List, k('q')), Action::Quit);
    }

    #[test]
    fn ctrl_c_quits_anywhere() {
        assert_eq!(dispatch(FocusedView::Review, k_ctrl('c')), Action::Quit);
        assert_eq!(dispatch(FocusedView::List, k_ctrl('c')), Action::Quit);
    }

    #[test]
    fn list_enter_opens_pr() {
        assert_eq!(
            dispatch(
                FocusedView::List,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            ),
            Action::ListOpen,
        );
    }

    #[test]
    fn review_q_returns_to_list() {
        assert_eq!(dispatch(FocusedView::Review, k('q')), Action::BackToList);
    }

    #[test]
    fn review_ctrl_d_pages_down() {
        assert_eq!(
            dispatch(FocusedView::Review, k_ctrl('d')),
            Action::HalfPageDown,
        );
    }

    #[test]
    fn review_tab_next_file() {
        assert_eq!(
            dispatch(
                FocusedView::Review,
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            ),
            Action::NextFile,
        );
    }

    #[test]
    fn wheel_scroll_up() {
        let ev = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(mouse_dispatch(ev), MouseAction::Scroll(-3));
    }

    #[test]
    fn left_click_yields_click_at() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(mouse_dispatch(ev), MouseAction::ClickAt { col: 12, row: 7 });
    }

    #[test]
    fn review_c_opens_commits_modal() {
        assert_eq!(
            dispatch(FocusedView::Review, k('c')),
            Action::OpenCommitsModal,
        );
    }
}
