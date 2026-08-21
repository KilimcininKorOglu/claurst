// plan_approval_dialog.rs — TUI overlay for approving a plan.
//
// Rendered when the model calls `ExitPlanMode`. The dialog shows the plan the
// model wrote, three ways to answer it, and a free-text row whose contents
// reach the model on every answer, so a rejection carries its reason.
//
// Layout:
//   ╭─ Plan ready ────────────────────────────────────╮
//   │                                                 │
//   │  1. Move the loader behind a trait              │
//   │  2. Point the two callers at it                 │
//   │                                                 │
//   │  ▶ 1  Approve and auto-accept edits             │
//   │    2  Approve, ask before each edit             │
//   │    3  Keep planning                             │
//   │                                                 │
//   │  ❯ _                                    (note)  │
//   │                                                 │
//   │  ↑↓: choose  Enter: confirm  PgUp/PgDn: scroll  │
//   ╰─────────────────────────────────────────────────╯

use std::cell::Cell;

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

/// The three answers, in the order they are listed.
const CHOICES: [(PlanChoice, &str); 3] = [
    (PlanChoice::AutoAcceptEdits, "Approve and auto-accept edits"),
    (PlanChoice::ManualApproval, "Approve, ask before each edit"),
    (PlanChoice::KeepPlanning, "Keep planning"),
];

/// How far one PageUp / PageDown moves through the plan.
const SCROLL_STEP: usize = 5;

/// State for the plan approval overlay.
#[derive(Default)]
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
    /// Set when the dialog opens, consumed on the answer.
    pub(crate) reply_tx: Option<tokio::sync::oneshot::Sender<PlanDecision>>,
}

impl PlanApprovalDialogState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show `plan` and wait for one of the three answers.
    pub fn open(&mut self, plan: String, reply_tx: tokio::sync::oneshot::Sender<PlanDecision>) {
        self.plan = plan;
        self.choice_idx = 0;
        self.note.clear();
        self.in_note = false;
        self.scroll = 0;
        self.max_scroll.set(0);
        self.reply_tx = Some(reply_tx);
        self.visible = true;
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
            .map(|(choice, _)| *choice)
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

    let width = 72u16.min(area.width.saturating_sub(4));
    let inner_w = width.saturating_sub(4) as usize;
    let plan_lines = word_wrap(&state.plan, inner_w);

    // Everything that is not the plan: padding, the three answers with their
    // spacer, the note row, and the hint row.
    const FIXED_ROWS: u16 = 9;
    let available = area.height.saturating_sub(2);
    let height = (FIXED_ROWS + plan_lines.len() as u16).min(available).max(
        // A dialog too short to show its own answers cannot be used.
        FIXED_ROWS.min(available),
    );
    let modal_area = centered_rect(width, height, area);

    // How much of the plan is left after the fixed rows have taken their share.
    let plan_viewport = modal_area.height.saturating_sub(FIXED_ROWS) as usize;
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
    for (i, (_, label)) in CHOICES.iter().enumerate() {
        let is_sel = !state.in_note && state.choice_idx == i;
        let style_bg = if is_sel { SELECTED_BG } else { MIKMIK_PANEL_BG };
        write_line!(
            row,
            Line::from(vec![
                Span::styled(
                    if is_sel { "▶ " } else { "  " },
                    Style::default()
                        .fg(if is_sel { SELECTED_FG } else { HINT_FG })
                        .bg(style_bg)
                ),
                Span::styled(
                    format!("{}", i + 1),
                    Style::default().fg(NUMBER_FG).bg(style_bg)
                ),
                Span::styled(
                    format!(" {label}"),
                    Style::default()
                        .fg(if is_sel { SELECTED_FG } else { OPTION_FG })
                        .bg(style_bg)
                        .add_modifier(if is_sel {
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

    // ---- the hint ----
    write_line!(
        row,
        Line::from(Span::styled(
            "↑↓: choose   1-3: pick   Enter: confirm   PgUp/PgDn: scroll   Esc: keep planning",
            Style::default().fg(HINT_FG).bg(MIKMIK_PANEL_BG)
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn open_dialog(
        plan: &str,
    ) -> (
        PlanApprovalDialogState,
        tokio::sync::oneshot::Receiver<PlanDecision>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut state = PlanApprovalDialogState::new();
        state.open(plan.to_string(), tx);
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
    fn the_three_answers_and_the_note_row_are_drawn() {
        let (state, _rx) = open_dialog("Move the loader behind a trait.");
        let rows = draw(&state, 100, 30);
        let screen = rows.join("\n");

        assert!(screen.contains("Plan ready"), "{screen}");
        assert!(
            screen.contains("Move the loader behind a trait."),
            "{screen}"
        );
        assert!(screen.contains("Approve and auto-accept edits"), "{screen}");
        assert!(screen.contains("Approve, ask before each edit"), "{screen}");
        assert!(screen.contains("Keep planning"), "{screen}");
        assert!(screen.contains("type to add a note"), "{screen}");
    }

    #[test]
    fn the_first_answer_starts_highlighted() {
        let (state, _rx) = open_dialog("a plan");
        let rows = draw(&state, 100, 30);

        let highlighted = rows
            .iter()
            .find(|row| row.contains("Approve and auto-accept edits"));
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
        assert!(last.contains("Approve and auto-accept edits"), "{last}");
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
        assert_eq!(decision.choice, PlanChoice::ManualApproval);
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
        assert_eq!(state.highlighted_choice(), PlanChoice::AutoAcceptEdits);

        state.select_prev();
        assert!(state.in_note, "up from the first answer lands on the note");

        state.select_next();
        assert!(!state.in_note);
        assert_eq!(state.highlighted_choice(), PlanChoice::AutoAcceptEdits);

        // Down past the last answer lands on the note, and down again wraps.
        state.select_next();
        state.select_next();
        assert_eq!(state.highlighted_choice(), PlanChoice::KeepPlanning);
        state.select_next();
        assert!(state.in_note);
        state.select_next();
        assert_eq!(state.highlighted_choice(), PlanChoice::AutoAcceptEdits);
    }
}
