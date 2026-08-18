//! Turn ANSI-coloured text into styled ratatui lines.
//!
//! The external status line lets a user's script colour its own output, so the
//! escape sequences it prints have to survive as styling rather than be shown
//! or dropped. SGR sequences become [`Style`]s; every other escape (OSC links,
//! cursor movement, screen clearing) is discarded, and so are control
//! characters that would corrupt the layout.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Columns between tab stops, matching the terminal default.
const TAB_WIDTH: usize = 8;

/// Parse `text` into one [`Line`] per output line.
pub fn ansi_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut pending = String::new();
    let mut style = Style::default();
    let mut column = 0usize;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => {
                if let Some(next) = consume_escape(&mut chars) {
                    // Only SGR changes styling; it starts a new span.
                    if !pending.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut pending), style));
                    }
                    style = apply_sgr(style, &next);
                }
            }
            '\n' => {
                if !pending.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut pending), style));
                }
                lines.push(Line::from(std::mem::take(&mut spans)));
                column = 0;
            }
            '\r' => {}
            '\t' => {
                let stop = (column / TAB_WIDTH + 1) * TAB_WIDTH;
                for _ in column..stop {
                    pending.push(' ');
                }
                column = stop;
            }
            ch if ch.is_control() => {}
            ch => {
                pending.push(ch);
                column += 1;
            }
        }
    }

    if !pending.is_empty() {
        spans.push(Span::styled(pending, style));
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// Consume one escape sequence after the leading `ESC`.
///
/// Returns the SGR parameter string when the sequence was `CSI … m`, and
/// `None` for everything else, which is swallowed.
fn consume_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    match chars.next() {
        Some('[') => {
            let mut params = String::new();
            for ch in chars.by_ref() {
                // A CSI sequence ends at its final byte, 0x40..=0x7E.
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    return (ch == 'm').then_some(params);
                }
                params.push(ch);
            }
            None
        }
        Some(']') => {
            // OSC runs until BEL or ST (ESC \).
            let mut previous = '\0';
            for ch in chars.by_ref() {
                if ch == '\u{7}' || (previous == '\u{1b}' && ch == '\\') {
                    break;
                }
                previous = ch;
            }
            None
        }
        // Charset designators take one more character; every other two-byte
        // escape is already consumed by the `next()` above.
        Some('(') | Some(')') | Some('*') | Some('+') => {
            chars.next();
            None
        }
        _ => None,
    }
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    // An empty parameter list means SGR 0.
    if params.is_empty() {
        return Style::default();
    }
    let codes: Vec<u8> = params
        .split(';')
        .map(|part| part.parse::<u8>().unwrap_or(0))
        .collect();

    let mut index = 0;
    while index < codes.len() {
        let code = codes[index];
        match code {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            8 => style = style.add_modifier(Modifier::HIDDEN),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            21 | 22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            25 => style = style.remove_modifier(Modifier::SLOW_BLINK),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            28 => style = style.remove_modifier(Modifier::HIDDEN),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(basic_color(code - 30, false)),
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(basic_color(code - 40, false)),
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(basic_color(code - 90, true)),
            100..=107 => style = style.bg(basic_color(code - 100, true)),
            38 | 48 => match extended_color(&codes, &mut index) {
                Some(color) if code == 38 => style = style.fg(color),
                Some(color) => style = style.bg(color),
                None => break,
            },
            _ => {}
        }
        index += 1;
    }
    style
}

/// Read a `5;N` (indexed) or `2;R;G;B` (truecolor) argument, advancing `index`
/// past the parameters it consumed.
fn extended_color(codes: &[u8], index: &mut usize) -> Option<Color> {
    match codes.get(*index + 1)? {
        5 => {
            let value = *codes.get(*index + 2)?;
            *index += 2;
            Some(Color::Indexed(value))
        }
        2 => {
            let red = *codes.get(*index + 2)?;
            let green = *codes.get(*index + 3)?;
            let blue = *codes.get(*index + 4)?;
            *index += 4;
            Some(Color::Rgb(red, green, blue))
        }
        _ => None,
    }
}

fn basic_color(offset: u8, bright: bool) -> Color {
    match (offset, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        (_, true) => Color::White,
        (_, false) => Color::Gray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn plain_text_survives_unchanged() {
        let lines = ansi_to_lines("model: opus");
        assert_eq!(text_of(&lines), "model: opus");
    }

    #[test]
    fn non_ascii_text_survives() {
        // The previous status line filtered on `is_ascii_graphic`, which turned
        // this into "alyor".
        let lines = ansi_to_lines("çalışıyor 📁 ▁▂▃");
        assert_eq!(text_of(&lines), "çalışıyor 📁 ▁▂▃");
    }

    #[test]
    fn a_colour_becomes_a_style_not_text() {
        let lines = ansi_to_lines("\u{1b}[32mok\u{1b}[0m done");
        assert_eq!(text_of(&lines), "ok done");

        let spans = &lines[0].spans;
        assert_eq!(spans[0].content.as_ref(), "ok");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content.as_ref(), " done");
        assert_eq!(spans[1].style.fg, None);
    }

    #[test]
    fn modifiers_accumulate_and_clear() {
        let lines = ansi_to_lines("\u{1b}[1;4mboth\u{1b}[24monly bold");
        let spans = &lines[0].spans;
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(spans[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(!spans[1].style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn indexed_and_truecolor_parse() {
        let indexed = ansi_to_lines("\u{1b}[38;5;208mx");
        assert_eq!(indexed[0].spans[0].style.fg, Some(Color::Indexed(208)));

        let truecolor = ansi_to_lines("\u{1b}[48;2;10;20;30mx");
        assert_eq!(truecolor[0].spans[0].style.bg, Some(Color::Rgb(10, 20, 30)));
    }

    #[test]
    fn a_bare_reset_clears_everything() {
        let lines = ansi_to_lines("\u{1b}[31;1mred\u{1b}[mplain");
        let spans = &lines[0].spans;
        assert_eq!(spans[1].content.as_ref(), "plain");
        assert_eq!(spans[1].style.fg, None);
        assert!(spans[1].style.add_modifier.is_empty());
    }

    #[test]
    fn an_osc_link_leaves_only_its_label() {
        let lines = ansi_to_lines("\u{1b}]8;;https://example.com\u{7}label\u{1b}]8;;\u{7} after");
        assert_eq!(text_of(&lines), "label after");
    }

    #[test]
    fn cursor_movement_is_dropped() {
        let lines = ansi_to_lines("a\u{1b}[2Kb\u{1b}[1;1Hc");
        assert_eq!(text_of(&lines), "abc");
    }

    #[test]
    fn each_output_line_becomes_its_own_line() {
        let lines = ansi_to_lines("first\nsecond\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines), "first\nsecond");
    }

    #[test]
    fn a_tab_advances_to_the_next_stop() {
        let lines = ansi_to_lines("ab\tc");
        assert_eq!(text_of(&lines), "ab      c");
    }

    #[test]
    fn control_characters_are_dropped() {
        let lines = ansi_to_lines("a\u{7}b\u{0}c\r");
        assert_eq!(text_of(&lines), "abc");
    }

    #[test]
    fn empty_input_yields_one_empty_line() {
        let lines = ansi_to_lines("");
        assert_eq!(lines.len(), 1);
        assert_eq!(text_of(&lines), "");
    }
}
