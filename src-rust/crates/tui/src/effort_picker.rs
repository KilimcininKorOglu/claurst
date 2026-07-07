// effort_picker.rs — horizontal, model-adaptive Effort selector for `/effort`.
//
// Replaces the prior 4-row vertical modal with a horizontal "Faster → Smarter"
// track (issue #268). The selectable levels are model-adaptive: they come from
// `claurst_api::supported_efforts(provider, model, registry)`, which returns the
// model's supported ladder (ascending) with `Ultracode` always last. `Ultracode`
// is separated from the native levels by a `│` divider and rendered specially.
//
// Layout (inside a bordered "Effort" panel):
//
//     Faster                                   Smarter
//     ─────────────────────────────────────────────────
//     low   medium   high   xhigh   max   │   ultracode
//                     ▲
//     <description of the selected level>
//
//     ←/→ to adjust · Enter to confirm · Esc to cancel
//
// Selector-only visuals (never the prompt box): the selected label is bold and
// highlighted; `xhigh` is bold purple; `max` is a per-character rainbow; and
// `ultracode` is purple and, when selected, paints an animated translucent-purple
// audio-spectrum background driven by `frame_count`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;

use crate::model_picker::EffortLevel;
use crate::overlays::centered_rect;

// ---------------------------------------------------------------------------
// Palette (selector-only)
// ---------------------------------------------------------------------------

/// Signature ultracode/xhigh purple.
const PURPLE: Color = Color::Rgb(168, 85, 247);
/// Brighter purple for the selected ultracode label / marker.
const PURPLE_BRIGHT: Color = Color::Rgb(196, 138, 255);
/// Dimmer purple for the unselected ultracode label and the "Smarter" end.
const PURPLE_DIM: Color = Color::Rgb(150, 118, 205);
/// Highlight for the selected (non-special) label.
const SELECTED_FG: Color = Color::Rgb(238, 238, 240);
/// Gray for unselected labels.
const DIM_FG: Color = Color::Rgb(120, 120, 130);
/// The horizontal track line + divider.
const TRACK_FG: Color = Color::Rgb(90, 90, 104);
/// The "Faster" end label.
const FASTER_FG: Color = Color::Rgb(120, 160, 200);

/// Controls hint line.
const CONTROLS: &str = "\u{2190}/\u{2192} to adjust \u{b7} Enter to confirm \u{b7} Esc to cancel";
/// Spaces between adjacent labels / around the divider.
const SEP: usize = 3;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Interactive state for the horizontal `/effort` selector.
#[derive(Debug, Default, Clone)]
pub struct EffortPickerState {
    pub visible: bool,
    /// The model-adaptive ordered ladder (ascending, `Ultracode` last).
    pub levels: Vec<EffortLevel>,
    /// Index into `levels` of the currently-highlighted level.
    pub selected: usize,
}

impl EffortPickerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the picker for the `current` effort, using `levels` as the
    /// model-adaptive ladder (as returned by `claurst_api::supported_efforts`).
    ///
    /// If `levels` is empty a sane default ladder is used. The selection is
    /// placed on `current` if present, otherwise on the nearest level at or below
    /// it (so switching from a model that supported `Max` to one that does not
    /// lands on the highest still-available level).
    pub fn open(&mut self, current: EffortLevel, levels: Vec<EffortLevel>) {
        let levels = if levels.is_empty() {
            default_levels()
        } else {
            levels
        };
        self.selected = index_for(&levels, current);
        self.levels = levels;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Move the selection one step toward "Faster" (clamped at the low end).
    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection one step toward "Smarter" (clamped at ultracode).
    pub fn select_next(&mut self) {
        if !self.levels.is_empty() {
            self.selected = (self.selected + 1).min(self.levels.len() - 1);
        }
    }

    /// The currently-selected level (falls back to `Medium` if empty).
    pub fn current(&self) -> EffortLevel {
        self.levels
            .get(self.selected)
            .copied()
            .unwrap_or(EffortLevel::Medium)
    }

    /// Whether the picker is showing its animated ultracode spectrum and so needs
    /// continuous repaints to keep moving. The CLI event loop uses this to keep
    /// ticking while the picker is open on `ultracode`.
    pub fn wants_animation(&self) -> bool {
        self.visible && self.current().is_ultracode()
    }
}

fn default_levels() -> Vec<EffortLevel> {
    vec![
        EffortLevel::Low,
        EffortLevel::Medium,
        EffortLevel::High,
        EffortLevel::Ultracode,
    ]
}

/// Choose the selected index for `current` within `levels`: an exact match if
/// present, otherwise the nearest level at or below it by rank, else the first.
fn index_for(levels: &[EffortLevel], current: EffortLevel) -> usize {
    if let Some(i) = levels.iter().position(|l| *l == current) {
        return i;
    }
    let want = rank(current);
    let mut best = 0usize;
    let mut best_rank = 0u8;
    for (i, l) in levels.iter().enumerate() {
        let r = rank(*l);
        if r <= want && r >= best_rank {
            best = i;
            best_rank = r;
        }
    }
    best
}

/// Ascending ordering rank used for nearest-level selection.
fn rank(level: EffortLevel) -> u8 {
    match level {
        EffortLevel::Low => 0,
        EffortLevel::Medium => 1,
        EffortLevel::High => 2,
        EffortLevel::XHigh => 3,
        EffortLevel::Max => 4,
        EffortLevel::Ultracode => 5,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the horizontal `/effort` selector. `frame_count` drives the animated
/// ultracode spectrum background (see [`EffortPickerState::wants_animation`]).
pub fn render_effort_picker(
    frame: &mut Frame,
    state: &EffortPickerState,
    area: Rect,
    _frame_count: u64,
) {
    if !state.visible || state.levels.is_empty() {
        return;
    }
    let selected = state.selected.min(state.levels.len() - 1);
    let sel_level = state.levels[selected];

    // Lay out the label row: styled spans, per-level center columns, total width.
    let (label_spans, centers, content_w) = layout_labels(&state.levels, selected);

    let controls_w = CONTROLS.chars().count();
    let body_w = content_w.max(controls_w);

    // 10 inner rows (see the row map below) + 2 border rows; 1 pad on each side.
    let want_w = body_w as u16 + 4;
    let width = want_w.min(area.width.saturating_sub(2)).max(10);
    let height = 12u16.min(area.height.saturating_sub(2)).max(6);
    let dlg = centered_rect(width, height, area);

    frame.render_widget(Clear, dlg);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PURPLE))
        .title(Span::styled(
            " Effort ",
            Style::default().fg(PURPLE).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(dlg);
    frame.render_widget(block, dlg);

    let buf = frame.buffer_mut();

    // Content is laid out from a 1-cell left pad inside the border.
    let x0 = inner.x + 1;
    let cw = content_w as u16;

    // Row map (relative to inner.y):
    //   0 blank | 1 Faster..Smarter | 2 track | 3 labels | 4 marker
    //   5 blank | 6 desc0 | 7 desc1 | 8 blank | 9 controls
    let row = |i: u16| inner.y + i;

    // Faster / Smarter ends of the track.
    blit_str(buf, x0, row(1), "Faster", Style::default().fg(FASTER_FG), inner);
    let smarter = "Smarter";
    let sm_x = x0 + cw.saturating_sub(smarter.chars().count() as u16);
    blit_str(
        buf,
        sm_x,
        row(1),
        smarter,
        Style::default().fg(PURPLE_DIM),
        inner,
    );

    // Track line.
    for dx in 0..cw {
        set_cell(buf, x0 + dx, row(2), '\u{2500}', Style::default().fg(TRACK_FG), inner);
    }

    // Level labels.
    for (col, span) in &label_spans {
        blit_span(buf, x0 + *col as u16, row(3), span, inner);
    }

    // Triangle marker directly under the selected level.
    let marker_x = x0 + centers[selected] as u16;
    set_cell(
        buf,
        marker_x,
        row(4),
        '\u{25b2}',
        Style::default()
            .fg(accent_for(sel_level))
            .add_modifier(Modifier::BOLD),
        inner,
    );

    // Description of the selected level (word-wrapped, up to two rows).
    let desc = level_description(sel_level, &state.levels);
    for (i, line) in word_wrap(&desc, body_w).into_iter().take(2).enumerate() {
        blit_str(
            buf,
            x0,
            row(6 + i as u16),
            &line,
            Style::default().fg(DIM_FG),
            inner,
        );
    }

    // Controls hint.
    blit_str(buf, x0, row(9), CONTROLS, Style::default().fg(DIM_FG), inner);
}

/// The accent color for a level's marker (matches its label styling).
fn accent_for(level: EffortLevel) -> Color {
    match level {
        EffortLevel::XHigh => PURPLE,
        EffortLevel::Max => Color::Rgb(255, 170, 60),
        EffortLevel::Ultracode => PURPLE_BRIGHT,
        _ => SELECTED_FG,
    }
}

/// Build the label row: placed styled spans (`(col_offset, span)`), the center
/// column of each level (for marker alignment), and the total content width.
fn layout_labels(
    levels: &[EffortLevel],
    selected: usize,
) -> (Vec<(usize, Span<'static>)>, Vec<usize>, usize) {
    let mut placed: Vec<(usize, Span<'static>)> = Vec::new();
    let mut centers = vec![0usize; levels.len()];
    let mut col = 0usize;
    let mut first = true;
    for (i, lvl) in levels.iter().enumerate() {
        // Ultracode is fenced off from the native ladder by a divider.
        if lvl.is_ultracode() {
            if !first {
                col += SEP;
            }
            placed.push((col, Span::styled("\u{2502}".to_string(), Style::default().fg(TRACK_FG))));
            col += 1;
            first = false;
        }
        if !first {
            col += SEP;
        }
        first = false;

        let start = col;
        let width = lvl.label().chars().count();
        centers[i] = start + width / 2;
        for span in styled_label(*lvl, i == selected) {
            let w = span.content.chars().count();
            placed.push((col, span));
            col += w;
        }
    }
    (placed, centers, col)
}

/// Style a single level label. Non-selected labels are dim gray; the selected one
/// is highlighted, with `xhigh` bold purple and `ultracode` purple. (`max` gets a
/// per-character rainbow, added in a later step.)
fn styled_label(level: EffortLevel, selected: bool) -> Vec<Span<'static>> {
    let text = level.label();
    if level.is_ultracode() {
        let fg = if selected { PURPLE_BRIGHT } else { PURPLE_DIM };
        let mut st = Style::default().fg(fg);
        if selected {
            st = st.add_modifier(Modifier::BOLD);
        }
        return vec![Span::styled(text.to_string(), st)];
    }
    if !selected {
        return vec![Span::styled(text.to_string(), Style::default().fg(DIM_FG))];
    }
    match level {
        EffortLevel::XHigh => vec![Span::styled(
            text.to_string(),
            Style::default().fg(PURPLE).add_modifier(Modifier::BOLD),
        )],
        _ => vec![Span::styled(
            text.to_string(),
            Style::default().fg(SELECTED_FG).add_modifier(Modifier::BOLD),
        )],
    }
}

/// The description shown for the selected level. Ultracode's description is
/// derived from the model's top native effort: "<top> + workflows".
fn level_description(level: EffortLevel, levels: &[EffortLevel]) -> String {
    match level {
        EffortLevel::Low => {
            "Fastest, most direct responses. Best for simple edits and quick questions.".to_string()
        }
        EffortLevel::Medium => {
            "Balanced reasoning and speed \u{2014} a solid default for everyday work.".to_string()
        }
        EffortLevel::High => {
            "Deeper, more careful reasoning for trickier, multi-step problems.".to_string()
        }
        EffortLevel::XHigh => {
            "Extended thinking budget for hard problems that need more deliberation.".to_string()
        }
        EffortLevel::Max => "May use excessive tokens resulting in long response times or \
             overthinking. Use sparingly for the hardest tasks."
            .to_string(),
        EffortLevel::Ultracode => {
            let top = top_native_label(levels);
            format!("{top} + workflows: bounded delegation across native primitives with verification.")
        }
    }
}

/// The label of the highest non-ultracode level in `levels` (the model's top
/// native effort), used to describe ultracode as "<top> + workflows".
fn top_native_label(levels: &[EffortLevel]) -> &'static str {
    levels
        .iter()
        .rev()
        .find(|l| !l.is_ultracode())
        .map(|l| l.label())
        .unwrap_or("max")
}

// ---------------------------------------------------------------------------
// Buffer helpers
// ---------------------------------------------------------------------------

/// Set a single cell's glyph + style, clipped to `inner`.
fn set_cell(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style, inner: Rect) {
    if !(inner.left()..inner.right()).contains(&x) || !(inner.top()..inner.bottom()).contains(&y) {
        return;
    }
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

/// Write a string starting at `(x, y)`, one cell per char, clipped to `inner`.
fn blit_str(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, inner: Rect) {
    let mut cx = x;
    for ch in s.chars() {
        set_cell(buf, cx, y, ch, style, inner);
        cx = cx.saturating_add(1);
    }
}

/// Write a styled span starting at `(x, y)`.
fn blit_span(buf: &mut Buffer, x: u16, y: u16, span: &Span, inner: Rect) {
    blit_str(buf, x, y, span.content.as_ref(), span.style, inner);
}

/// Minimal greedy word-wrap to `width` columns.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}
