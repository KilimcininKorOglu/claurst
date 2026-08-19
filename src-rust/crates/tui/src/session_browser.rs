//! Session browser overlay (/session, /resume, /rename, /export).
//! Mirrors TS session management in REPL.tsx

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::overlays::centered_rect;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The interaction mode of the session browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBrowserMode {
    /// Default: list sessions, navigate with arrow keys.
    Browse,
    /// User is typing a new name for the selected session.
    Rename,
    /// Waiting for the user to confirm a destructive action (delete / export).
    Confirm,
}

/// A single session entry shown in the browser list.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    /// Human-readable relative time, e.g. "2 hours ago".
    pub last_updated: String,
    pub message_count: usize,
    /// Estimated USD cost for the session.
    pub cost_usd: f64,
    /// Directory the session was working in.
    ///
    /// Sessions are stored in one flat directory shared by every project, so
    /// without this the list gives no way to tell which project a row belongs
    /// to. `None` for a session saved before the field existed.
    pub working_dir: Option<String>,
}

/// State for the session browser overlay.
pub struct SessionBrowserState {
    pub visible: bool,
    pub selected_idx: usize,
    pub sessions: Vec<SessionEntry>,
    pub mode: SessionBrowserMode,
    /// Input buffer used while in `Rename` mode.
    pub rename_input: String,
    /// Show each session's working directory under its row.
    pub show_paths: bool,
    /// Show a detail panel for the selected session under the list.
    pub show_preview: bool,
    /// How many files in the sessions directory could not be read.
    ///
    /// Shown in the title: a session that will not parse is otherwise absent
    /// from the list with nothing said anywhere, and the user is left looking
    /// at an empty browser over a full directory.
    pub unreadable: usize,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl SessionBrowserState {
    /// Create a new, hidden browser with an empty session list.
    pub fn new() -> Self {
        Self {
            visible: false,
            selected_idx: 0,
            sessions: Vec::new(),
            mode: SessionBrowserMode::Browse,
            rename_input: String::new(),
            show_paths: false,
            show_preview: false,
            unreadable: 0,
        }
    }

    /// Open the browser with the provided session list.
    pub fn open(&mut self, sessions: Vec<SessionEntry>) {
        self.sessions = sessions;
        self.selected_idx = 0;
        self.mode = SessionBrowserMode::Browse;
        self.rename_input.clear();
        self.show_paths = false;
        self.show_preview = false;
        self.visible = true;
    }

    /// Close the browser entirely.
    pub fn close(&mut self) {
        self.visible = false;
        self.mode = SessionBrowserMode::Browse;
        self.rename_input.clear();
        // A toggle left on from a previous visit reads as the browser having
        // changed shape on its own.
        self.show_paths = false;
        self.show_preview = false;
    }

    /// Show or hide the working directory under each row.
    pub fn toggle_paths(&mut self) {
        self.show_paths = !self.show_paths;
    }

    /// Show or hide the detail panel for the selected session.
    pub fn toggle_preview(&mut self) {
        self.show_preview = !self.show_preview;
    }

    /// Move selection up one row, wrapping to the end.
    pub fn select_prev(&mut self) {
        let count = self.sessions.len();
        if count == 0 {
            return;
        }
        if self.selected_idx == 0 {
            self.selected_idx = count - 1;
        } else {
            self.selected_idx -= 1;
        }
    }

    /// Move selection down one row, wrapping to the start.
    pub fn select_next(&mut self) {
        let count = self.sessions.len();
        if count == 0 {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % count;
    }

    /// Return a reference to the currently selected session, if any.
    pub fn selected_session(&self) -> Option<&SessionEntry> {
        self.sessions.get(self.selected_idx)
    }

    /// Switch to rename mode, pre-populating the input with the current title.
    pub fn start_rename(&mut self) {
        if let Some(session) = self.sessions.get(self.selected_idx) {
            self.rename_input = session.title.clone();
            self.mode = SessionBrowserMode::Rename;
        }
    }

    /// Append a character to the rename input buffer.
    pub fn push_rename_char(&mut self, c: char) {
        if self.mode == SessionBrowserMode::Rename {
            self.rename_input.push(c);
        }
    }

    /// Remove the last character from the rename input buffer.
    pub fn pop_rename_char(&mut self) {
        if self.mode == SessionBrowserMode::Rename {
            self.rename_input.pop();
        }
    }

    /// Confirm the rename. Returns `(session_id, new_name)` when in rename mode
    /// with a non-empty name and a valid selection. Resets to browse mode.
    pub fn confirm_rename(&mut self) -> Option<(String, String)> {
        if self.mode != SessionBrowserMode::Rename {
            return None;
        }
        let new_name = self.rename_input.trim().to_string();
        if new_name.is_empty() {
            return None;
        }
        let session_id = self.sessions.get(self.selected_idx)?.id.clone();
        // Apply the rename in the local list immediately for UI consistency.
        if let Some(session) = self.sessions.get_mut(self.selected_idx) {
            session.title = new_name.clone();
        }
        self.mode = SessionBrowserMode::Browse;
        self.rename_input.clear();
        Some((session_id, new_name))
    }

    /// Cancel the current mode:
    /// - In `Rename` or `Confirm` mode: return to `Browse`.
    /// - In `Browse` mode: close the overlay.
    pub fn cancel(&mut self) {
        match self.mode {
            SessionBrowserMode::Browse => self.close(),
            SessionBrowserMode::Rename | SessionBrowserMode::Confirm => {
                self.mode = SessionBrowserMode::Browse;
                self.rename_input.clear();
            }
        }
    }
}

impl Default for SessionBrowserState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Columns of blank left margin before a row's first column.
const ROW_MARGIN: usize = 2;
/// Columns of blank space between two adjacent columns.
const COLUMN_GAP: usize = 2;

/// Lines the detail panel takes: a blank separator plus one line per fact.
const PREVIEW_LINES: usize = 3;

/// How many sessions fit in a modal of `modal_height`.
///
/// The header, the blank line under it, the blank line above the hint bar and
/// the hint bar itself all take a line first, the rename field takes one more
/// than the other modes, the detail panel takes its own block, and a session
/// takes two lines instead of one while its working directory is showing.
fn row_capacity(state: &SessionBrowserState, modal_height: u16) -> usize {
    let hint_lines = match state.mode {
        SessionBrowserMode::Rename => 2,
        SessionBrowserMode::Browse | SessionBrowserMode::Confirm => 1,
    };
    let preview_lines = if state.show_preview { PREVIEW_LINES } else { 0 };
    let chrome = 3 + hint_lines + preview_lines;
    let lines = (modal_height as usize)
        .saturating_sub(2) // top and bottom border
        .saturating_sub(chrome);
    let lines_per_session = if state.show_paths { 2 } else { 1 };
    (lines / lines_per_session).max(1)
}

/// The half-open range of rows to draw so that `selected` stays on screen.
///
/// Derived from the selection each frame rather than kept as scroll state,
/// which is how the model picker does it too.
fn visible_window(selected: usize, total: usize, capacity: usize) -> (usize, usize) {
    if total <= capacity {
        return (0, total);
    }
    let first = selected
        .saturating_sub(capacity.saturating_sub(1))
        .min(total - capacity);
    (first, first + capacity)
}

/// Format a cost as a dollar string with 4 decimal places.
fn fmt_cost(usd: f64) -> String {
    if usd < 0.0001 {
        "$0.0000".to_string()
    } else {
        format!("${:.4}", usd)
    }
}

/// Truncate `s` to fit within `max_width` display columns, appending `…` if cut.
fn truncate_display(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + ch.len_utf8() + 1 > max_width {
            break;
        }
        out.push(ch);
    }
    format!("{}…", out)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the session browser overlay directly into `buf`.
///
/// Draws a centred modal (≈70 wide × ≈20 tall) with:
/// - A scrollable list of sessions (id, title, date, messages, cost)
/// - Selection highlight on the focused row
/// - Mode-sensitive hint bar at the bottom
/// - A rename input field shown when in `Rename` mode
pub fn render_session_browser(state: &SessionBrowserState, area: Rect, buf: &mut Buffer) {
    if !state.visible {
        return;
    }

    const MODAL_W: u16 = 70;
    const MODAL_H: u16 = 20;

    let dialog_area = centered_rect(
        MODAL_W.min(area.width.saturating_sub(2)),
        MODAL_H.min(area.height.saturating_sub(2)),
        area,
    );

    // --- Clear background -------------------------------------------------
    for y in dialog_area.y..dialog_area.y + dialog_area.height {
        for x in dialog_area.x..dialog_area.x + dialog_area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }

    let inner_w = dialog_area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // --- Session list -----------------------------------------------------
    if state.sessions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  No sessions found.",
            Style::default().fg(Color::DarkGray),
        )]));
    } else {
        // Column widths:
        //   title: fills  |  date: 14  |  msgs: 5  |  cost: 9
        let date_w: usize = 14;
        let msgs_w: usize = 5;
        let cost_w: usize = 9;
        // Two columns of left margin plus three two-column separators. Counting
        // only the separators leaves each row two columns wider than the modal,
        // which wraps the cost onto a line of its own.
        let fixed = date_w + msgs_w + cost_w + ROW_MARGIN + 3 * COLUMN_GAP;
        let title_w = inner_w.saturating_sub(fixed).max(10);

        // Header row
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  {:<title_w$}  {:<date_w$}  {:>msgs_w$}  {:>cost_w$}",
                "Title",
                "Last Updated",
                "Msgs",
                "Cost",
                title_w = title_w,
                date_w = date_w,
                msgs_w = msgs_w,
                cost_w = cost_w
            ),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::UNDERLINED),
        )]));
        lines.push(Line::from(""));

        // Rows only exist on screen if the modal has room for them. Drawing the
        // whole list into a fixed-height box loses everything past the bottom
        // border, and the selection can walk into that invisible part.
        let (first, last) = visible_window(
            state.selected_idx,
            state.sessions.len(),
            row_capacity(state, dialog_area.height),
        );

        for (i, session) in state.sessions.iter().enumerate().take(last).skip(first) {
            let is_selected = i == state.selected_idx;

            let title_cell = truncate_display(&session.title, title_w);
            let date_cell = truncate_display(&session.last_updated, date_w);
            let msgs_cell = format!("{:>msgs_w$}", session.message_count, msgs_w = msgs_w);
            let cost_cell = format!("{:>cost_w$}", fmt_cost(session.cost_usd), cost_w = cost_w);

            let row_bg = if is_selected {
                Color::Rgb(40, 60, 80)
            } else {
                // transparent — ratatui uses reset/default for "no background"
                Color::Reset
            };

            let title_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let meta_style = if is_selected {
                Style::default().fg(Color::Rgb(180, 200, 220)).bg(row_bg)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let prefix_style = Style::default().bg(row_bg);

            lines.push(Line::from(vec![
                Span::styled("  ", prefix_style),
                Span::styled(
                    format!("{:<title_w$}", title_cell, title_w = title_w),
                    title_style,
                ),
                Span::styled("  ", meta_style),
                Span::styled(
                    format!("{:<date_w$}", date_cell, date_w = date_w),
                    meta_style,
                ),
                Span::styled("  ", meta_style),
                Span::styled(msgs_cell, meta_style),
                Span::styled("  ", meta_style),
                Span::styled(cost_cell, meta_style),
            ]));

            // The path goes on a line of its own rather than into a column:
            // the four columns already fill the modal's width, and a directory
            // squeezed into what is left would be all ellipsis.
            if state.show_paths {
                let path = session.working_dir.as_deref().unwrap_or("(no directory)");
                lines.push(Line::from(vec![
                    Span::styled("    ", prefix_style),
                    Span::styled(
                        truncate_display(path, inner_w.saturating_sub(4)),
                        Style::default().fg(Color::DarkGray).bg(row_bg),
                    ),
                ]));
            }
        }
    }

    // --- Detail panel for the selected session ---------------------------
    // Only what the table cannot carry. Title, date, message count and cost
    // are columns already; repeating them here would draw the same facts
    // twice and cost rows that the list needs.
    if state.show_preview {
        if let Some(session) = state.selected_session() {
            let label_style = Style::default().fg(Color::DarkGray);
            let value_style = Style::default().fg(Color::Rgb(180, 200, 220));
            let value_w = inner_w.saturating_sub(10);

            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  ID    ", label_style),
                Span::styled(truncate_display(&session.id, value_w), value_style),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Path  ", label_style),
                Span::styled(
                    truncate_display(
                        session.working_dir.as_deref().unwrap_or("(no directory)"),
                        value_w,
                    ),
                    value_style,
                ),
            ]));
        }
    }

    lines.push(Line::from(""));

    // --- Mode-sensitive bottom section -----------------------------------
    match &state.mode {
        SessionBrowserMode::Browse => {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "\u{2191}\u{2193}",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                // "move", not "navigate": the bar carries six keys now and the
                // longer word pushes the last one onto a second line, which
                // costs the list a row.
                Span::styled(" move  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=resume  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "r",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=rename  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "a",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=paths  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "p",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=details  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=close", Style::default().fg(Color::DarkGray)),
            ]));
        }
        SessionBrowserMode::Rename => {
            // Show rename input field.
            let label = "  Rename: ";
            let cursor = "\u{2588}"; // block cursor
            let input_display = format!("{}{}", state.rename_input, cursor);
            lines.push(Line::from(vec![
                Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    input_display,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=confirm  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=cancel", Style::default().fg(Color::DarkGray)),
            ]));
        }
        SessionBrowserMode::Confirm => {
            lines.push(Line::from(vec![
                Span::styled(
                    "  Confirm? ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("=yes  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    "Esc",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled("=no", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    // Say where in the list the cursor is whenever the list outgrows the modal,
    // so rows scrolled out of sight do not read as rows that are not there.
    let position = if state.sessions.len() > row_capacity(state, dialog_area.height) {
        format!(" {}/{}", state.selected_idx + 1, state.sessions.len())
    } else {
        String::new()
    };
    let unreadable = if state.unreadable > 0 {
        format!(" ({} unreadable)", state.unreadable)
    } else {
        String::new()
    };
    let title = format!(" Sessions{position}{unreadable} ");

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Cyan));

    let para = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });

    use ratatui::widgets::Widget;
    para.render(dialog_area, buf);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sessions() -> Vec<SessionEntry> {
        vec![
            SessionEntry {
                id: "sess-001".to_string(),
                title: "Refactor auth module".to_string(),
                last_updated: "2 hours ago".to_string(),
                message_count: 34,
                cost_usd: 0.0124,
                working_dir: Some("/home/dev/api-gateway".to_string()),
            },
            SessionEntry {
                id: "sess-002".to_string(),
                title: "Write unit tests".to_string(),
                last_updated: "yesterday".to_string(),
                message_count: 12,
                cost_usd: 0.0045,
                working_dir: Some("/home/dev/claurst".to_string()),
            },
            SessionEntry {
                id: "sess-003".to_string(),
                title: "Debug memory leak".to_string(),
                last_updated: "3 days ago".to_string(),
                message_count: 57,
                cost_usd: 0.0289,
                working_dir: None,
            },
        ]
    }

    // 1. new() starts hidden with no sessions.
    #[test]
    fn new_starts_hidden() {
        let s = SessionBrowserState::new();
        assert!(!s.visible);
        assert!(s.sessions.is_empty());
        assert_eq!(s.mode, SessionBrowserMode::Browse);
    }

    // 2. open() populates sessions and becomes visible.
    #[test]
    fn open_populates_and_shows() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        assert!(s.visible);
        assert_eq!(s.sessions.len(), 3);
        assert_eq!(s.selected_idx, 0);
        assert_eq!(s.mode, SessionBrowserMode::Browse);
    }

    // 3. select_next() advances selection and wraps to the start.
    #[test]
    fn select_next_wraps_to_start() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.select_next();
        assert_eq!(s.selected_idx, 1);
        s.select_next();
        assert_eq!(s.selected_idx, 2);
        s.select_next();
        assert_eq!(s.selected_idx, 0);
    }

    // 4. select_prev() decrements and wraps to the end.
    #[test]
    fn select_prev_wraps_to_end() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.select_prev();
        assert_eq!(s.selected_idx, 2);
    }

    // 5. selected_session() returns correct entry.
    #[test]
    fn selected_session_correct() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.selected_idx = 1;
        let sess = s.selected_session().unwrap();
        assert_eq!(sess.id, "sess-002");
    }

    // 6. start_rename() switches mode and pre-fills input.
    #[test]
    fn start_rename_prefills_title() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.selected_idx = 0;
        s.start_rename();
        assert_eq!(s.mode, SessionBrowserMode::Rename);
        assert_eq!(s.rename_input, "Refactor auth module");
    }

    // 7. push_rename_char / pop_rename_char edit the input buffer.
    #[test]
    fn rename_char_editing() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.start_rename();
        s.rename_input.clear(); // clear prefill for clean test
        s.push_rename_char('H');
        s.push_rename_char('i');
        assert_eq!(s.rename_input, "Hi");
        s.pop_rename_char();
        assert_eq!(s.rename_input, "H");
    }

    // 8. confirm_rename() returns (id, new_name) and resets mode.
    #[test]
    fn confirm_rename_returns_pair() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.selected_idx = 0;
        s.start_rename();
        s.rename_input = "  New Title  ".to_string(); // intentional whitespace
        let result = s.confirm_rename();
        assert_eq!(
            result,
            Some(("sess-001".to_string(), "New Title".to_string()))
        );
        assert_eq!(s.mode, SessionBrowserMode::Browse);
        assert!(s.rename_input.is_empty());
        // Also check local title was updated
        assert_eq!(s.sessions[0].title, "New Title");
    }

    // 9. confirm_rename() with empty input returns None.
    #[test]
    fn confirm_rename_empty_returns_none() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.start_rename();
        s.rename_input = "   ".to_string(); // whitespace only
        let result = s.confirm_rename();
        assert!(result.is_none());
    }

    // 10. cancel() in Rename mode returns to Browse without closing.
    #[test]
    fn cancel_rename_goes_to_browse() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.start_rename();
        s.cancel();
        assert_eq!(s.mode, SessionBrowserMode::Browse);
        assert!(
            s.visible,
            "overlay should remain visible after cancel-from-rename"
        );
    }

    // 11. cancel() in Browse mode closes the overlay.
    #[test]
    fn cancel_browse_closes() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        assert_eq!(s.mode, SessionBrowserMode::Browse);
        s.cancel();
        assert!(!s.visible);
    }

    // 12. render_session_browser does not panic.
    #[test]
    fn render_does_not_panic() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        render_session_browser(&s, area, &mut buf);
    }

    // 13. render is a no-op when hidden.
    #[test]
    fn render_noop_when_hidden() {
        let s = SessionBrowserState::new(); // visible = false
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render_session_browser(&s, area, &mut buf);
        for cell in buf.content() {
            assert_eq!(
                cell.symbol(),
                " ",
                "buffer should be empty when browser is hidden"
            );
        }
    }

    // 14. fmt_cost formats correctly.
    #[test]
    fn fmt_cost_formats() {
        assert_eq!(fmt_cost(0.0), "$0.0000");
        assert_eq!(fmt_cost(0.0124), "$0.0124");
        assert_eq!(fmt_cost(1.5), "$1.5000");
    }

    // 15. truncate_display trims long strings.
    #[test]
    fn truncate_display_trims() {
        let long = "abcdefghij"; // 10 chars
        let result = truncate_display(long, 5);
        assert!(
            result.width() <= 6,
            "truncated string should fit within budget"
        );
        assert!(result.ends_with('…'));
    }

    /// The rows the browser drew, trimmed of the modal's blank surroundings.
    fn drawn_rows(state: &SessionBrowserState, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render_session_browser(state, area, &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|cell| cell.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn many_sessions(count: usize) -> Vec<SessionEntry> {
        (0..count)
            .map(|i| SessionEntry {
                id: format!("sess-{i:03}"),
                title: format!("Session number {i}"),
                last_updated: "just now".to_string(),
                message_count: i,
                cost_usd: 0.0,
                working_dir: Some(format!("/home/dev/project-{i}")),
            })
            .collect()
    }

    #[test]
    fn a_list_that_fits_is_drawn_whole() {
        let (first, last) = visible_window(0, 3, 14);
        assert_eq!((first, last), (0, 3));
    }

    #[test]
    fn the_window_follows_the_selection_down_the_list() {
        // Nothing scrolls until the selection would leave the box.
        assert_eq!(visible_window(13, 40, 14), (0, 14));
        assert_eq!(visible_window(14, 40, 14), (1, 15));
        assert_eq!(visible_window(39, 40, 14), (26, 40));
    }

    #[test]
    fn the_window_never_runs_past_the_end_of_the_list() {
        // Wrapping from the last row back to the first must not leave the
        // window parked beyond the rows that exist.
        let (first, last) = visible_window(0, 40, 14);
        assert_eq!((first, last), (0, 14));
        let (first, last) = visible_window(39, 40, 14);
        assert!(last <= 40 && first + 14 == last);
    }

    #[test]
    fn the_capacity_shrinks_for_the_taller_rename_bar() {
        let mut s = SessionBrowserState::new();
        assert_eq!(row_capacity(&s, 20), 14);
        s.mode = SessionBrowserMode::Rename;
        assert_eq!(row_capacity(&s, 20), 13);
        // A modal with no room at all still reports one row rather than zero,
        // so the selected session is never invisible.
        s.mode = SessionBrowserMode::Browse;
        assert_eq!(row_capacity(&s, 4), 1);
    }

    #[test]
    fn showing_paths_halves_how_many_sessions_fit() {
        let mut s = SessionBrowserState::new();
        assert_eq!(row_capacity(&s, 20), 14);
        s.show_paths = true;
        assert_eq!(row_capacity(&s, 20), 7);
    }

    #[test]
    fn the_detail_panel_takes_its_rows_from_the_list() {
        // Without this the panel would be drawn over the bottom of the list
        // and the last sessions would disappear behind the border.
        let mut s = SessionBrowserState::new();
        assert_eq!(row_capacity(&s, 20), 14);
        s.show_preview = true;
        assert_eq!(row_capacity(&s, 20), 14 - PREVIEW_LINES);
    }

    #[test]
    fn the_detail_panel_shows_what_the_columns_cannot() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        assert!(!drawn_rows(&s, 80, 24)
            .iter()
            .any(|r| r.contains("sess-001")));

        s.toggle_preview();
        let rows = drawn_rows(&s, 80, 24);
        assert!(
            rows.iter().any(|r| r.contains("sess-001")),
            "the id is not a column: {rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("/home/dev/api-gateway")),
            "nor is the full path: {rows:#?}"
        );
    }

    #[test]
    fn the_detail_panel_follows_the_selection() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.toggle_preview();
        s.select_next();

        let rows = drawn_rows(&s, 80, 24);
        assert!(rows.iter().any(|r| r.contains("sess-002")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("sess-001")));
    }

    #[test]
    fn the_hint_bar_still_fits_on_one_line() {
        // Six keys is as many as the 70-column modal holds. A seventh, or a
        // longer word, wraps the last hint onto a line the list needed.
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        let rows = drawn_rows(&s, 80, 24);

        let hint_row = rows
            .iter()
            .find(|row| row.contains("=rename"))
            .expect("the hint bar is drawn");
        assert!(
            hint_row.contains("Esc=close"),
            "the last hint has to share the line: {hint_row:?}"
        );
        assert!(
            !rows.iter().any(|row| row.trim() == "Esc=close"),
            "and nothing is left on a line of its own: {rows:#?}"
        );
    }

    #[test]
    fn the_detail_panel_is_forgotten_on_close() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.toggle_preview();
        s.close();
        assert!(!s.show_preview);
    }

    #[test]
    fn an_empty_list_has_no_session_to_detail() {
        let mut s = SessionBrowserState::new();
        s.open(vec![]);
        s.toggle_preview();

        let rows = drawn_rows(&s, 80, 24);
        assert!(rows.iter().any(|r| r.contains("No sessions found")));
        assert!(!rows.iter().any(|r| r.contains("ID")), "{rows:#?}");
    }

    #[test]
    fn the_selected_row_stays_on_screen_in_a_long_list() {
        let mut s = SessionBrowserState::new();
        s.open(many_sessions(40));
        s.selected_idx = 39;
        let rows = drawn_rows(&s, 80, 24);

        assert!(
            rows.iter().any(|row| row.contains("Session number 39")),
            "the selection has to be drawn: {rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("Session number 0 ")),
            "and the top of the list scrolls away to make room: {rows:#?}"
        );
    }

    #[test]
    fn a_list_too_long_for_the_modal_says_where_the_cursor_is() {
        let mut s = SessionBrowserState::new();
        s.open(many_sessions(40));
        s.selected_idx = 20;
        let rows = drawn_rows(&s, 80, 24);
        assert!(
            rows.iter().any(|row| row.contains("Sessions 21/40")),
            "{rows:#?}"
        );

        // A list that fits keeps the plain title.
        let mut short = SessionBrowserState::new();
        short.open(sample_sessions());
        let rows = drawn_rows(&short, 80, 24);
        assert!(rows.iter().any(|row| row.contains("Sessions")));
        assert!(!rows.iter().any(|row| row.contains("/3")), "{rows:#?}");
    }

    #[test]
    fn paths_are_hidden_until_asked_for_and_forgotten_on_close() {
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        assert!(!s.show_paths, "the plain list is what opens");

        s.toggle_paths();
        assert!(s.show_paths);

        s.close();
        assert!(
            !s.show_paths,
            "a toggle left on reads as the browser changing shape by itself"
        );
    }

    #[test]
    fn the_working_directory_is_drawn_under_the_session_it_belongs_to() {
        // The session store is one flat directory shared by every project, so
        // without this the list gives no way to tell them apart.
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        let before = drawn_rows(&s, 80, 24);
        assert!(!before.iter().any(|row| row.contains("api-gateway")));

        s.toggle_paths();
        let after = drawn_rows(&s, 80, 24);
        let title_row = after
            .iter()
            .position(|row| row.contains("Refactor auth module"))
            .expect("the session is drawn");
        assert!(
            after[title_row + 1].contains("/home/dev/api-gateway"),
            "the path belongs directly under its own row: {:?}",
            &after[title_row..title_row + 2]
        );
    }

    #[test]
    fn a_session_with_no_recorded_directory_says_so_rather_than_leaving_a_gap() {
        // A blank line under one row and a path under the next reads as a
        // rendering fault, not as a missing field.
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        s.toggle_paths();
        let rows = drawn_rows(&s, 80, 24);

        let title_row = rows
            .iter()
            .position(|row| row.contains("Debug memory leak"))
            .expect("the session is drawn");
        assert!(rows[title_row + 1].contains("(no directory)"), "{rows:#?}");
    }

    #[test]
    fn a_row_fits_on_one_line_instead_of_wrapping_its_cost() {
        // The column budget counted the separators but not the left margin, so
        // every row came out two columns wider than the modal and the cost
        // wrapped onto a line of its own.
        let mut s = SessionBrowserState::new();
        s.open(sample_sessions());
        let rows = drawn_rows(&s, 80, 24);

        let title_rows: Vec<&String> = rows
            .iter()
            .filter(|row| row.contains("Refactor auth module"))
            .collect();
        assert_eq!(title_rows.len(), 1, "{rows:#?}");
        assert!(
            title_rows[0].contains("$0.0124"),
            "the cost belongs on its own row's line: {:?}",
            title_rows[0]
        );
        assert!(
            !rows.iter().any(|row| row.trim() == "$0.0124"),
            "no line carries the cost alone: {rows:#?}"
        );
    }
}
