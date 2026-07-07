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
/// Very dark purple wash behind the ultracode spectrum (translucent look).
const SPECTRUM_BG: Color = Color::Rgb(24, 16, 40);

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
    frame_count: u64,
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

    // When ultracode is the selected level, paint an animated translucent-purple
    // audio-spectrum behind everything; the labels/track/text are drawn on top.
    if sel_level.is_ultracode() {
        paint_spectrum(buf, inner, frame_count);
    }

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
        EffortLevel::Max => rainbow_spans(text),
        _ => vec![Span::styled(
            text.to_string(),
            Style::default().fg(SELECTED_FG).add_modifier(Modifier::BOLD),
        )],
    }
}

/// One bold span per character, each with a distinct hue cycled across the word,
/// producing a rainbow gradient (selector-only visual for `max`).
fn rainbow_spans(text: &str) -> Vec<Span<'static>> {
    let n = text.chars().count().max(1);
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let hue = 360.0 * i as f32 / n as f32;
            let (r, g, b) = hsv_to_rgb(hue, 0.9, 1.0);
            Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(Color::Rgb(r, g, b))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

/// Convert HSV (`h` in degrees, `s`/`v` in `[0, 1]`) to an 8-bit RGB triple.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
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

// ---------------------------------------------------------------------------
// Ultracode spectrum background
// ---------------------------------------------------------------------------

/// Paint an animated, translucent-purple audio-spectrum into `inner`.
///
/// Every column gets a vertical bar rising from the bottom whose height and
/// brightness vary per column and SHIFT each frame — `frame_count` is the phase,
/// so successive frames animate. All shades are low-value purples (a dark wash +
/// dim bars) so foreground text drawn on top stays readable.
fn paint_spectrum(buf: &mut Buffer, inner: Rect, frame_count: u64) {
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Translucent purple wash across the whole panel.
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_bg(SPECTRUM_BG);
            }
        }
    }

    // Bars rising from the bottom.
    let height = inner.height as f32;
    for gx in 0..inner.width {
        let amp = spectrum_amp(gx, frame_count).clamp(0.0, 1.0);
        let filled = amp * height;
        let bars = filled.floor() as u16;
        let frac = filled - bars as f32;
        let x = inner.left() + gx;
        for r in 0..inner.height {
            let (ch, lit) = if r < bars {
                ('\u{2588}', 0.65 + 0.35 * amp)
            } else if r == bars && frac > 0.08 {
                (partial_block(frac), 0.35 + 0.4 * frac)
            } else {
                continue;
            };
            let y = inner.bottom() - 1 - r;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(Style::default().fg(purple_shade(lit)).bg(SPECTRUM_BG));
            }
        }
    }
}

/// Per-column spectrum amplitude in `[0, 1]` for a given column and frame. Two
/// out-of-phase sines make it read like a shifting equalizer; `frame` moves the
/// phase so the bars animate over time.
fn spectrum_amp(gx: u16, frame: u64) -> f32 {
    let fx = gx as f32;
    let t = frame as f32;
    let a = 0.55 * (fx * 0.55 + t * 0.20).sin() + 0.45 * (fx * 0.27 - t * 0.13 + 1.7).sin();
    0.5 + 0.5 * a
}

/// The partial block glyph (`▁`..`█`) for a fractional bar height in `[0, 1]`.
fn partial_block(frac: f32) -> char {
    const BLOCKS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let idx = ((frac * 8.0) as usize).min(BLOCKS.len() - 1);
    BLOCKS[idx]
}

/// A dim/translucent purple whose brightness scales with `lit` in `[0, 1]`, kept
/// in a low value range so foreground text stays dominant.
fn purple_shade(lit: f32) -> Color {
    let k = 0.18 + 0.30 * lit.clamp(0.0, 1.0);
    Color::Rgb(
        (168.0 * k) as u8,
        (85.0 * k) as u8,
        (247.0 * k) as u8,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn full_ladder() -> Vec<EffortLevel> {
        vec![
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::XHigh,
            EffortLevel::Max,
            EffortLevel::Ultracode,
        ]
    }

    fn state_with(levels: Vec<EffortLevel>, selected: usize) -> EffortPickerState {
        EffortPickerState {
            visible: true,
            levels,
            selected,
        }
    }

    fn render_to_buffer(state: &EffortPickerState, frame_count: u64) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_effort_picker(f, state, f.area(), frame_count))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Each buffer row as a `String` of cell glyphs (all glyphs here are 1 cell
    /// wide, so a char index equals its column).
    fn buffer_rows(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect()
    }

    /// Char-column index of `needle` in `row` (converts the byte offset from
    /// `str::find` to a char/column index, since labels can share a row with
    /// multi-byte glyphs like the border/divider `│`).
    fn char_col_of(row: &str, needle: &str) -> Option<usize> {
        let byte_idx = row.find(needle)?;
        Some(row[..byte_idx].chars().count())
    }

    #[test]
    fn open_selects_current_and_clamps_navigation() {
        let mut s = EffortPickerState::new();
        s.open(EffortLevel::High, full_ladder());
        assert!(s.visible);
        assert_eq!(s.current(), EffortLevel::High);

        // ← past the start clamps at Low.
        for _ in 0..10 {
            s.select_prev();
        }
        assert_eq!(s.current(), EffortLevel::Low);
        // → past the end clamps at Ultracode.
        for _ in 0..20 {
            s.select_next();
        }
        assert_eq!(s.current(), EffortLevel::Ultracode);
        assert!(s.wants_animation());
    }

    #[test]
    fn open_falls_back_to_nearest_available_level() {
        // Model without Max/XHigh; opening on Max lands on the highest native.
        let levels = vec![
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Ultracode,
        ];
        let mut s = EffortPickerState::new();
        s.open(EffortLevel::Max, levels);
        assert_eq!(s.current(), EffortLevel::High);
    }

    #[test]
    fn renders_model_levels_and_ultracode_after_divider() {
        // Max selected → no spectrum, so label gaps read as plain spaces.
        let state = state_with(full_ladder(), 4);
        let rows = buffer_rows(&render_to_buffer(&state, 0));
        let label_row = rows
            .iter()
            .find(|r| r.contains("ultracode"))
            .expect("label row present");

        for lbl in ["low", "medium", "high", "xhigh", "max"] {
            assert!(label_row.contains(lbl), "labels row missing {lbl}: {label_row:?}");
        }
        // A divider must sit between `max` and `ultracode`.
        let max_end = label_row.find("max").unwrap() + "max".len();
        let uc = label_row.find("ultracode").unwrap();
        let gap = &label_row[max_end..uc];
        assert!(
            gap.contains('\u{2502}'),
            "expected `│` divider between max and ultracode, gap={gap:?}"
        );
    }

    #[test]
    fn marker_sits_under_selected_level() {
        // Select `medium` (unique, not a substring of another label).
        let state = state_with(full_ladder(), 1);
        let rows = buffer_rows(&render_to_buffer(&state, 0));

        let (marker_y, marker_row) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains('\u{25b2}'))
            .map(|(i, r)| (i, r.clone()))
            .expect("marker row present");
        let marker_col = marker_row.chars().position(|c| c == '\u{25b2}').unwrap();

        let label_row = &rows[marker_y - 1];
        let start = char_col_of(label_row, "medium").expect("medium in labels row");
        let end = start + "medium".chars().count();
        assert!(
            marker_col >= start && marker_col < end,
            "marker col {marker_col} not within medium [{start}, {end})"
        );
    }

    #[test]
    fn max_uses_distinct_per_char_rainbow_colors() {
        let state = state_with(full_ladder(), 4); // max selected
        let buf = render_to_buffer(&state, 0);
        let rows = buffer_rows(&buf);
        let label_y = rows
            .iter()
            .position(|r| r.contains("ultracode"))
            .expect("label row present");
        let label_row = &rows[label_y];
        let start = char_col_of(label_row, "max").expect("max in labels row");

        let y = label_y as u16;
        let colors: Vec<Color> = (0..3u16)
            .map(|dx| buf.cell((start as u16 + dx, y)).expect("max cell").fg)
            .collect();
        assert_ne!(colors[0], colors[1], "rainbow chars must differ: {colors:?}");
        assert_ne!(colors[1], colors[2], "rainbow chars must differ: {colors:?}");
        assert_ne!(colors[0], colors[2], "rainbow chars must differ: {colors:?}");
    }

    #[test]
    fn ultracode_spectrum_animates_but_others_are_static() {
        let levels = vec![
            EffortLevel::Low,
            EffortLevel::Medium,
            EffortLevel::High,
            EffortLevel::Ultracode,
        ];

        // Ultracode selected → background differs between two frame_count values.
        let ultra = state_with(levels.clone(), levels.len() - 1);
        let a = render_to_buffer(&ultra, 0);
        let b = render_to_buffer(&ultra, 30);
        assert_ne!(
            a.content(),
            b.content(),
            "ultracode spectrum should animate between frames"
        );

        // Non-ultracode selection → no spectrum, identical across frames.
        let high = state_with(levels, 2);
        let c = render_to_buffer(&high, 0);
        let d = render_to_buffer(&high, 30);
        assert_eq!(
            c.content(),
            d.content(),
            "non-ultracode picker must not animate"
        );
    }
}
