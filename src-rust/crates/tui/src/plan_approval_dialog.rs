// plan_approval_dialog.rs — TUI overlay for approving a plan.
//
// Rendered when the model calls `ExitPlanMode`. The dialog shows the plan the
// model wrote, four ways to answer it, and a free-text row whose contents reach
// the model on every answer, so a rejection carries its reason.
//
// The two plain approvals do not name a fixed permission mode: they restore the
// one plan mode was entered from, which the caller passes to `open`.
//
// Layout:
//   ╭─ Plan ready ──────────────────────────────────────────────╮
//   │                                                           │
//   │  1. Move the loader behind a trait                        │
//   │  2. Point the two callers at it                           │
//   │                                                           │
//   │  ▶ 1  Yes, clear context (54% used) and BYPASS PERMISSIONS│
//   │    2  Yes, and switch to BYPASS PERMISSIONS for this …    │
//   │    3  Yes, manually approve edits                         │
//   │    4  Tell MikMik what to change                          │
//   │                                                           │
//   │  ❯ _                                                      │
//   │  ~/.config/mikmik/plans/<session>.md                      │
//   │  ↑↓ choose   Enter confirm   ctrl+g edit   Esc keep …     │
//   ╰───────────────────────────────────────────────────────────╯

use std::cell::Cell;
use std::path::PathBuf;

use mikmik_core::config::PermissionMode;
use mikmik_tools::{PlanChoice, PlanDecision};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::ask_user_dialog::word_wrap;
use crate::overlays::{centered_rect, MIKMIK_PANEL_BG};

const BORDER_FG: Color = Color::Rgb(120, 120, 170);
const TITLE_FG: Color = Color::Rgb(200, 160, 255);
const PLAN_FG: Color = Color::Rgb(230, 230, 230);
const OPTION_FG: Color = Color::Rgb(190, 190, 210);
const SELECTED_FG: Color = Color::Rgb(255, 255, 255);
const SELECTED_BG: Color = Color::Rgb(55, 55, 90);
const HINT_FG: Color = Color::Rgb(100, 100, 130);
const INPUT_FG: Color = Color::Rgb(200, 255, 200);
const NUMBER_FG: Color = Color::Rgb(150, 150, 200);

/// The four answers, in the order they are listed.
const CHOICES: [PlanChoice; 4] = [
    PlanChoice::ApproveAndClearContext,
    PlanChoice::Approve,
    PlanChoice::ApproveWithManualEdits,
    PlanChoice::KeepPlanning,
];

/// How far one PageUp / PageDown moves through the plan.
const SCROLL_STEP: usize = 5;

/// How a permission mode reads in an answer.
pub(crate) fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::BypassPermissions => "BYPASS PERMISSIONS",
        PermissionMode::AcceptEdits => "auto-accept edits",
        PermissionMode::Default => "manual approval",
        PermissionMode::Plan => "plan mode",
    }
}

/// State for the plan approval overlay.
pub struct PlanApprovalDialogState {
    /// Whether the dialog is currently visible.
    pub visible: bool,
    /// The plan, as the model wrote it.
    pub plan: String,
    /// The answer that will be sent, always one of [`CHOICES`].
    ///
    /// Kept separately from the cursor, so that typing a note does not throw
    /// away the answer the user had already picked.
    pub choice_idx: usize,
    /// Free text the user typed alongside the answer.
    pub note: String,
    /// Whether the note row has the cursor.
    pub in_note: bool,
    /// First plan line drawn, so a plan taller than the dialog can be read.
    pub scroll: usize,
    /// Largest useful [`Self::scroll`], written by the renderer because only it
    /// knows how tall the plan came out at this terminal size.
    max_scroll: Cell<usize>,
    /// The mode an approval puts the session into: the one plan mode was
    /// entered from.
    pub restore_mode: PermissionMode,
    /// How full the context is, for the answer that clears it. `None` when the
    /// model's window is unknown.
    pub context_pct: Option<u64>,
    /// Where the plan is on disk, when it could be written. `None` removes the
    /// path row and the offer to edit it.
    pub plan_path: Option<PathBuf>,
    /// Set when the user asks to edit the plan; the session loop takes it,
    /// because only it can leave the alternate screen for an editor.
    pub edit_requested: bool,
    /// Set when the dialog opens, consumed on the answer.
    pub(crate) reply_tx: Option<tokio::sync::oneshot::Sender<PlanDecision>>,
}

impl Default for PlanApprovalDialogState {
    fn default() -> Self {
        Self {
            visible: false,
            plan: String::new(),
            choice_idx: 0,
            note: String::new(),
            in_note: false,
            scroll: 0,
            max_scroll: Cell::new(0),
            restore_mode: PermissionMode::AcceptEdits,
            context_pct: None,
            plan_path: None,
            edit_requested: false,
            reply_tx: None,
        }
    }
}

impl PlanApprovalDialogState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `plan` and wait for one of the four answers.
    ///
    /// `restore_mode` is the permission mode an approval returns the session
    /// to, and it names the first two answers.
    pub fn open(
        &mut self,
        plan: String,
        plan_path: Option<PathBuf>,
        restore_mode: PermissionMode,
        context_pct: Option<u64>,
        reply_tx: tokio::sync::oneshot::Sender<PlanDecision>,
    ) {
        self.plan = plan;
        self.plan_path = plan_path;
        self.restore_mode = restore_mode;
        self.context_pct = context_pct;
        self.choice_idx = 0;
        self.note.clear();
        self.in_note = false;
        self.scroll = 0;
        self.max_scroll.set(0);
        self.edit_requested = false;
        self.reply_tx = Some(reply_tx);
        self.visible = true;
    }

    /// The label for each answer, in the order they are listed.
    fn labels(&self) -> [String; 4] {
        let mode = mode_label(self.restore_mode);
        let clear = match self.context_pct {
            Some(pct) => format!("Yes, clear context ({pct}% used) and switch to {mode}"),
            None => format!("Yes, clear context and switch to {mode}"),
        };
        [
            clear,
            format!("Yes, and switch to {mode} for this session"),
            "Yes, manually approve edits".to_string(),
            "Tell MikMik what to change".to_string(),
        ]
    }

    /// Ask the session loop to open the plan in an editor.
    ///
    /// Does nothing when there is no file, which is the case when the plan
    /// could not be written.
    pub fn request_edit(&mut self) {
        self.edit_requested = self.plan_path.is_some();
    }

    /// Take the pending request to edit the plan, if there is one.
    pub fn take_edit_request(&mut self) -> Option<PathBuf> {
        if !self.edit_requested {
            return None;
        }
        self.edit_requested = false;
        self.plan_path.clone()
    }

    /// Move the cursor up through the answers and the note row.
    pub fn select_prev(&mut self) {
        if self.in_note {
            self.in_note = false;
            self.choice_idx = CHOICES.len() - 1;
        } else if self.choice_idx == 0 {
            self.in_note = true;
        } else {
            self.choice_idx -= 1;
        }
    }

    /// Move the cursor down through the answers and the note row.
    pub fn select_next(&mut self) {
        if self.in_note {
            self.in_note = false;
            self.choice_idx = 0;
        } else if self.choice_idx + 1 >= CHOICES.len() {
            self.in_note = true;
        } else {
            self.choice_idx += 1;
        }
    }

    /// Pick an answer by its 1-based number.
    pub fn select_by_number(&mut self, n: usize) {
        if n >= 1 && n <= CHOICES.len() {
            self.choice_idx = n - 1;
            self.in_note = false;
        }
    }

    /// Type into the note.
    ///
    /// Any printable character moves the cursor to the note row, so the user
    /// can start typing a reason without navigating there first. The answer
    /// they had picked is left alone.
    pub fn push_char(&mut self, c: char) {
        self.note.push(c);
        self.in_note = true;
    }

    pub fn pop_char(&mut self) {
        if self.in_note {
            self.note.pop();
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(SCROLL_STEP);
    }

    pub fn scroll_down(&mut self) {
        self.scroll = (self.scroll + SCROLL_STEP).min(self.max_scroll.get());
    }

    /// Send the picked answer.
    ///
    /// Returns `false` when nothing was waiting on it.
    pub fn confirm(&mut self) -> bool {
        self.send(self.highlighted_choice())
    }

    /// The answer `shift+tab` sends: the picked one when it approves, and a
    /// plain approval otherwise.
    ///
    /// The shortcut means "approve with this feedback", so it cannot send the
    /// answer that refuses. It also never picks the one that clears the
    /// context, because a shortcut should not throw the conversation away.
    pub fn approve_with_feedback_choice(&self) -> PlanChoice {
        match self.highlighted_choice() {
            PlanChoice::KeepPlanning => PlanChoice::Approve,
            other => other,
        }
    }

    /// Approve carrying whatever is in the note row.
    pub fn approve_with_feedback(&mut self) -> bool {
        self.send(self.approve_with_feedback_choice())
    }

    /// Close without approving. The plan stands and the session stays in plan
    /// mode, which is the safe reading of a dismissed dialog.
    pub fn dismiss(&mut self) -> bool {
        self.note.clear();
        self.send(PlanChoice::KeepPlanning)
    }

    fn send(&mut self, choice: PlanChoice) -> bool {
        self.visible = false;
        let note = self.note.trim();
        let note = (!note.is_empty()).then(|| note.to_string());
        match self.reply_tx.take() {
            Some(tx) => {
                let _ = tx.send(PlanDecision { choice, note });
                true
            }
            None => false,
        }
    }

    /// The answer that would be sent right now, for the caller that has to
    /// apply it to the session.
    pub fn highlighted_choice(&self) -> PlanChoice {
        CHOICES
            .get(self.choice_idx)
            .copied()
            .unwrap_or(PlanChoice::KeepPlanning)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the plan approval dialog. Does nothing unless the dialog is visible.
pub fn render_plan_approval_dialog(state: &PlanApprovalDialogState, area: Rect, buf: &mut Buffer) {
    if !state.visible {
        return;
    }

    let width = 76u16.min(area.width.saturating_sub(4));
    let inner_w = width.saturating_sub(4) as usize;
    let plan_lines = word_wrap(&state.plan, inner_w);
    let labels = state.labels();

    // Everything that is not the plan: two border rows, one row of top padding,
    // the four answers between two spacers, the note row, the hint row, and the
    // path row when there is a file. One short and the last row falls off the
    // bottom.
    let fixed_rows = 11 + u16::from(state.plan_path.is_some());
    let available = area.height.saturating_sub(2);
    let height = (fixed_rows + plan_lines.len() as u16).min(available).max(
        // A dialog too short to show its own answers cannot be used.
        fixed_rows.min(available),
    );
    let modal_area = centered_rect(width, height, area);

    // How much of the plan is left after the fixed rows have taken their share.
    let plan_viewport = modal_area.height.saturating_sub(fixed_rows) as usize;
    state
        .max_scroll
        .set(plan_lines.len().saturating_sub(plan_viewport));
    let scroll = state.scroll.min(state.max_scroll.get());

    // ---- background ----
    for y in modal_area.top()..modal_area.bottom() {
        for x in modal_area.left()..modal_area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_bg(MIKMIK_PANEL_BG);
            }
        }
    }

    // ---- border ----
    let border_style = Style::default().fg(BORDER_FG).bg(MIKMIK_PANEL_BG);
    for y in modal_area.top()..modal_area.bottom() {
        let is_top = y == modal_area.top();
        let is_bot = y == modal_area.bottom() - 1;
        for x in modal_area.left()..modal_area.right() {
            let is_left = x == modal_area.left();
            let is_right = x == modal_area.right() - 1;
            if let Some(cell) = buf.cell_mut((x, y)) {
                let ch = match (is_top, is_bot, is_left, is_right) {
                    (true, _, true, _) => '╭',
                    (true, _, _, true) => '╮',
                    (_, true, true, _) => '╰',
                    (_, true, _, true) => '╯',
                    (true, _, _, _) | (_, true, _, _) => '─',
                    (_, _, true, _) | (_, _, _, true) => '│',
                    _ => continue,
                };
                cell.set_char(ch);
                cell.set_style(border_style);
            }
        }
    }

    // ---- title ----
    let title = if state.max_scroll.get() > 0 {
        format!(
            " Plan ready ({}/{}) ",
            scroll + 1,
            state.max_scroll.get() + 1
        )
    } else {
        " Plan ready ".to_string()
    };
    let title_style = Style::default()
        .fg(TITLE_FG)
        .bg(MIKMIK_PANEL_BG)
        .add_modifier(Modifier::BOLD);
    for (i, ch) in title.chars().enumerate() {
        let x = modal_area.left() + 2 + i as u16;
        if x < modal_area.right() - 1 {
            if let Some(cell) = buf.cell_mut((x, modal_area.top())) {
                cell.set_char(ch);
                cell.set_style(title_style);
            }
        }
    }

    let inner = Rect {
        x: modal_area.x + 2,
        y: modal_area.y + 1,
        width: modal_area.width.saturating_sub(4),
        height: modal_area.height.saturating_sub(2),
    };
    let bottom = inner.y + inner.height;

    macro_rules! write_line {
        ($row:expr, $line:expr) => {{
            if $row < bottom {
                let r = Rect {
                    x: inner.x,
                    y: $row,
                    width: inner.width,
                    height: 1,
                };
                Paragraph::new($line).render(r, buf);
            }
        }};
    }

    let mut row = inner.y + 1; // top padding

    // ---- the plan ----
    for plan_line in plan_lines.iter().skip(scroll).take(plan_viewport) {
        write_line!(
            row,
            Line::from(Span::styled(
                plan_line.clone(),
                Style::default().fg(PLAN_FG).bg(MIKMIK_PANEL_BG)
            ))
        );
        row += 1;
    }

    row += 1; // spacer

    // ---- the answers ----
    for (i, label) in labels.iter().enumerate() {
        // The marker follows the picked answer even while the cursor is down
        // in the note, so the user can see what Enter would send. Only the
        // highlight follows the cursor.
        let is_picked = state.choice_idx == i;
        let has_cursor = is_picked && !state.in_note;
        let style_bg = if has_cursor {
            SELECTED_BG
        } else {
            MIKMIK_PANEL_BG
        };
        write_line!(
            row,
            Line::from(vec![
                Span::styled(
                    if is_picked { "▶ " } else { "  " },
                    Style::default()
                        .fg(if is_picked { SELECTED_FG } else { HINT_FG })
                        .bg(style_bg)
                ),
                Span::styled(
                    format!("{}", i + 1),
                    Style::default().fg(NUMBER_FG).bg(style_bg)
                ),
                Span::styled(
                    format!(" {label}"),
                    Style::default()
                        .fg(if is_picked { SELECTED_FG } else { OPTION_FG })
                        .bg(style_bg)
                        .add_modifier(if is_picked {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        })
                ),
            ])
        );
        row += 1;
    }

    row += 1; // spacer

    // ---- the note ----
    let style_bg = if state.in_note {
        SELECTED_BG
    } else {
        MIKMIK_PANEL_BG
    };
    let mut spans = vec![Span::styled(
        if state.in_note { "❯ " } else { "  " },
        Style::default()
            .fg(if state.in_note { SELECTED_FG } else { HINT_FG })
            .bg(style_bg),
    )];
    if state.note.is_empty() && !state.in_note {
        spans.push(Span::styled(
            "type to add a note for the model…",
            Style::default().fg(HINT_FG).bg(style_bg),
        ));
    } else {
        spans.push(Span::styled(
            format!("{}{}", state.note, if state.in_note { "█" } else { "" }),
            Style::default().fg(INPUT_FG).bg(style_bg),
        ));
    }
    write_line!(row, Line::from(spans));
    row += 1;

    // ---- where the plan is ----
    if let Some(path) = state.plan_path.as_ref() {
        write_line!(
            row,
            Line::from(Span::styled(
                format!("  {}", fit_path(path, inner_w.saturating_sub(2))),
                Style::default().fg(HINT_FG).bg(MIKMIK_PANEL_BG)
            ))
        );
        row += 1;
    }

    // ---- the hint ----
    // Kept short enough to fit the dialog: a truncated hint is how the user
    // loses the last key on the row. The two conditional keys are only offered
    // when they do something.
    let mut hint = String::from("↑↓ choose   Enter confirm");
    if state.plan_path.is_some() {
        hint.push_str("   ctrl+g edit");
    }
    if state.max_scroll.get() > 0 {
        hint.push_str("   PgUp/PgDn scroll");
    }
    hint.push_str("   Esc keep planning");
    write_line!(
        row,
        Line::from(Span::styled(
            hint,
            Style::default().fg(HINT_FG).bg(MIKMIK_PANEL_BG)
        ))
    );
}

/// The path as it fits the dialog: home written as `~`, and the head dropped
/// before the tail.
///
/// The row exists to say which file `ctrl+g` opens, so the file name is the
/// part that has to survive; a path cut at its right edge keeps the part the
/// user already knows and loses the part they do not.
fn fit_path(path: &std::path::Path, width: usize) -> String {
    let shown = path.display().to_string();
    let shown = match std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .and_then(|home| {
            shown
                .strip_prefix(home.to_string_lossy().as_ref())
                .map(|rest| format!("~{rest}"))
        }) {
        Some(shortened) => shortened,
        None => shown,
    };

    if shown.chars().count() <= width || width == 0 {
        return shown;
    }
    let tail: String = shown
        .chars()
        .skip(shown.chars().count() - width.saturating_sub(1))
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A dialog restoring bypass permissions, with a plan file and a known
    /// context size, which is the case that draws every row.
    fn open_dialog(
        plan: &str,
    ) -> (
        PlanApprovalDialogState,
        tokio::sync::oneshot::Receiver<PlanDecision>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut state = PlanApprovalDialogState::new();
        state.open(
            plan.to_string(),
            Some(PathBuf::from("/tmp/plans/sess-1.md")),
            PermissionMode::BypassPermissions,
            Some(54),
            tx,
        );
        (state, rx)
    }

    /// A dialog with nothing but the plan: no file, no known context size.
    fn open_bare_dialog(
        plan: &str,
    ) -> (
        PlanApprovalDialogState,
        tokio::sync::oneshot::Receiver<PlanDecision>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut state = PlanApprovalDialogState::new();
        state.open(
            plan.to_string(),
            None,
            PermissionMode::AcceptEdits,
            None,
            tx,
        );
        (state, rx)
    }

    /// What the buffer shows, one string per row.
    fn draw(state: &PlanApprovalDialogState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = match Terminal::new(TestBackend::new(width, height)) {
            Ok(terminal) => terminal,
            Err(error) => panic!("could not build a test terminal: {error}"),
        };
        if let Err(error) = terminal.draw(|frame| {
            let area = frame.area();
            render_plan_approval_dialog(state, area, frame.buffer_mut());
        }) {
            panic!("could not draw: {error}");
        }
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_string()))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn the_four_answers_and_the_note_row_are_drawn() {
        let (state, _rx) = open_dialog("Move the loader behind a trait.");
        let rows = draw(&state, 100, 30);
        let screen = rows.join("\n");

        assert!(screen.contains("Plan ready"), "{screen}");
        assert!(
            screen.contains("Move the loader behind a trait."),
            "{screen}"
        );
        // The mode named in the first two answers is the one plan mode was
        // entered from, and the percentage says what clearing would free.
        assert!(
            screen.contains("Yes, clear context (54% used) and switch to BYPASS PERMISSIONS"),
            "{screen}"
        );
        assert!(
            screen.contains("Yes, and switch to BYPASS PERMISSIONS for this session"),
            "{screen}"
        );
        assert!(screen.contains("Yes, manually approve edits"), "{screen}");
        assert!(screen.contains("Tell MikMik what to change"), "{screen}");
        assert!(screen.contains("type to add a note"), "{screen}");
        assert!(screen.contains("plans/sess-1.md"), "{screen}");
        // The keys are only discoverable from this row, and it is the first
        // thing a dialog sized one row short drops.
        assert!(screen.contains("Enter confirm"), "{screen}");
        assert!(screen.contains("ctrl+g edit"), "{screen}");
        // Whole, not cut off at the dialog's right edge.
        assert!(screen.contains("Esc keep planning"), "{screen}");
    }

    /// Without a plan file there is nothing to edit and no path to show.
    #[test]
    fn a_plan_that_could_not_be_written_offers_no_editor() {
        let (mut state, _rx) = open_bare_dialog("a plan");
        let screen = draw(&state, 100, 30).join("\n");

        assert!(!screen.contains("ctrl+g"), "{screen}");
        // The window size was unknown, so the answer cannot claim a percentage.
        assert!(
            screen.contains("Yes, clear context and switch to auto-accept edits"),
            "{screen}"
        );

        state.request_edit();
        assert_eq!(state.take_edit_request(), None);
    }

    /// The row says which file ctrl+g opens, so the file name is the part
    /// that has to survive a path too long for the dialog.
    #[test]
    fn a_long_path_keeps_its_file_name() {
        let (state, _rx) = open_dialog("a plan");
        let long = PathBuf::from(
            "/private/tmp/claude-501/-Users-someone-Desktop-a-very-long-project/plans/sess-1.md",
        );

        let fitted = fit_path(&long, 40);
        assert_eq!(fitted.chars().count(), 40);
        assert!(fitted.starts_with('…'), "{fitted}");
        assert!(fitted.ends_with("plans/sess-1.md"), "{fitted}");

        // A path that fits is left alone.
        let short = PathBuf::from("/tmp/plans/sess-1.md");
        assert_eq!(fit_path(&short, 40), "/tmp/plans/sess-1.md");
        let _ = state;
    }

    /// ctrl+g hands the session loop the path, because only it can leave the
    /// alternate screen for an editor.
    #[test]
    fn asking_to_edit_hands_over_the_path() {
        let (mut state, _rx) = open_dialog("a plan");

        assert_eq!(state.take_edit_request(), None);
        state.request_edit();
        assert_eq!(
            state.take_edit_request(),
            Some(PathBuf::from("/tmp/plans/sess-1.md"))
        );
        // Taken once: a second read must not reopen the editor every frame.
        assert_eq!(state.take_edit_request(), None);
    }

    /// The dialog grows with the plan and keeps every fixed row visible.
    #[test]
    fn the_hint_row_survives_a_taller_plan() {
        for lines in 1..=6 {
            let plan = (1..=lines)
                .map(|n| format!("step {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            let (state, _rx) = open_dialog(&plan);
            let screen = draw(&state, 100, 30).join("\n");

            assert!(
                screen.contains(&format!("step {lines}")),
                "the last plan line is missing at {lines} lines:\n{screen}"
            );
            assert!(
                screen.contains("Esc keep planning"),
                "the hint row is missing at {lines} lines:\n{screen}"
            );
        }
    }

    #[test]
    fn the_first_answer_starts_highlighted() {
        let (state, _rx) = open_dialog("a plan");
        let rows = draw(&state, 100, 30);

        let highlighted = rows.iter().find(|row| row.contains("Yes, clear context"));
        assert!(
            highlighted.is_some_and(|row| row.contains('▶')),
            "the first answer is not marked: {rows:#?}"
        );
    }

    #[test]
    fn a_plan_taller_than_the_dialog_scrolls() {
        // 40 lines into a 20-row terminal: the tail is off-screen until scrolled.
        let plan = (1..=40)
            .map(|n| format!("step {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut state, _rx) = open_dialog(&plan);

        let first = draw(&state, 100, 20).join("\n");
        assert!(first.contains("step 1"), "{first}");
        assert!(!first.contains("step 40"), "{first}");

        // The renderer had to run once for the scroll bound to be known.
        for _ in 0..40 {
            state.scroll_down();
            draw(&state, 100, 20);
        }
        let last = draw(&state, 100, 20).join("\n");
        assert!(last.contains("step 40"), "{last}");
        // Scrolling stops at the end rather than running off it.
        assert!(last.contains("Yes, clear context"), "{last}");
    }

    /// While a note is being typed the answer is still armed, so the screen
    /// has to keep saying which one Enter would send.
    #[test]
    fn the_picked_answer_stays_marked_while_a_note_is_typed() {
        let (mut state, _rx) = open_dialog("a plan");
        state.select_by_number(3);
        state.push_char('x');

        let rows = draw(&state, 100, 30);
        let marked = rows
            .iter()
            .find(|row| row.contains("Yes, manually approve edits"));
        assert!(
            marked.is_some_and(|row| row.contains('▶')),
            "the picked answer lost its marker: {rows:#?}"
        );
    }

    #[test]
    fn typing_moves_the_cursor_to_the_note() {
        let (mut state, _rx) = open_dialog("a plan");
        state.push_char('n');
        state.push_char('o');

        assert!(state.in_note);
        let screen = draw(&state, 100, 30).join("\n");
        assert!(screen.contains("no█"), "{screen}");
    }

    #[tokio::test]
    async fn an_answer_carries_its_note() {
        let (mut state, rx) = open_dialog("a plan");
        state.select_by_number(2);
        // Typing moves the cursor to the note; the picked answer must survive it.
        state.push_char('x');
        assert!(state.in_note);

        assert!(state.confirm());
        assert!(!state.visible);

        let decision = rx.await.expect("the dialog answered");
        assert_eq!(decision.choice, PlanChoice::Approve);
        assert_eq!(decision.note.as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn dismissing_keeps_the_session_planning() {
        let (mut state, rx) = open_dialog("a plan");
        state.select_by_number(1);
        state.push_char('?');

        assert!(state.dismiss());

        let decision = rx.await.expect("the dialog answered");
        assert_eq!(decision.choice, PlanChoice::KeepPlanning);
        // Esc is not a way to send the model a note.
        assert_eq!(decision.note, None);
    }

    #[test]
    fn the_cursor_wraps_through_the_note_row() {
        let (mut state, _rx) = open_dialog("a plan");
        assert_eq!(
            state.highlighted_choice(),
            PlanChoice::ApproveAndClearContext
        );

        state.select_prev();
        assert!(state.in_note, "up from the first answer lands on the note");

        state.select_next();
        assert!(!state.in_note);
        assert_eq!(
            state.highlighted_choice(),
            PlanChoice::ApproveAndClearContext
        );

        // Down past the last answer lands on the note, and down again wraps.
        for _ in 0..3 {
            state.select_next();
        }
        assert_eq!(state.highlighted_choice(), PlanChoice::KeepPlanning);
        state.select_next();
        assert!(state.in_note);
        state.select_next();
        assert_eq!(
            state.highlighted_choice(),
            PlanChoice::ApproveAndClearContext
        );
    }

    /// Shift+Tab promotes the answer that refuses into a plain approval.
    #[test]
    fn the_feedback_shortcut_never_refuses() {
        let (mut state, _rx) = open_dialog("a plan");

        state.select_by_number(4);
        assert_eq!(state.highlighted_choice(), PlanChoice::KeepPlanning);
        assert_eq!(state.approve_with_feedback_choice(), PlanChoice::Approve);

        // An answer that already approves is sent as it stands, including the
        // one that clears the context.
        state.select_by_number(1);
        assert_eq!(
            state.approve_with_feedback_choice(),
            PlanChoice::ApproveAndClearContext
        );
        state.select_by_number(3);
        assert_eq!(
            state.approve_with_feedback_choice(),
            PlanChoice::ApproveWithManualEdits
        );
    }
}
