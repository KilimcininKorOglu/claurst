//! MikMik mascot rendering for ratatui.
//!
//! A 3-row ASCII cat. Call `mikmik_lines()` to get 4 `Line` values (3 body
//! rows + 1 blank spacing row) ready for embedding in a Paragraph.
//!
//! Structure (top to bottom):
//!   Row 1 — ears
//!   Row 2 — face: the eyes vary with the pose, the rest is fixed
//!   Row 3 — whiskers and muzzle
//!   Row 4 — blank spacing
//!
//! Every row is exactly [`MIKMIK_WIDTH`] characters wide, because the welcome
//! screen centres the mascot on that width rather than measuring it.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Width of every mascot row, in characters.
///
/// `render.rs` centres the mascot by subtracting this from the column width,
/// so a row that does not match it renders off-centre.
pub const MIKMIK_WIDTH: u16 = 7;

/// The mascot's name, shown under it on the welcome screen.
pub const MIKMIK_NAME: &str = "MikMik";

/// The pose / expression of the MikMik mascot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MikMikPose {
    Default,
    LookLeft,
    LookRight,
    LookDown,
    /// Eyes closed. Brief and frequent, which is what makes it read as a
    /// blink rather than as a nap.
    Blink,
    /// Loading / error spinner — `frame` drives the animation.
    Loading {
        frame: u64,
    },
}

/// Body-part style: bold pink foreground (#e91e63).
fn body_style() -> Style {
    Style::default()
        .fg(Color::Rgb(233, 30, 99))
        .add_modifier(Modifier::BOLD)
}

/// Face style: pink text on black background.
fn face_style() -> Style {
    Style::default()
        .fg(Color::Rgb(233, 30, 99))
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Eyeball highlight style: white on black.
fn eyeball_style() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Characters that count as an eye and therefore get the white highlight.
///
/// The muzzle `.` and the brackets stay pink, so the eyes are what the reader
/// looks at first.
fn is_eyeball(ch: char) -> bool {
    matches!(ch, 'o' | '-' | 'v' | '|' | '/' | '\\')
}

/// Build spans for the face row, giving the eye characters white foreground
/// and everything else pink-on-black.
fn face_spans(s: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut buf_is_eyeball = false;

    for ch in s.chars() {
        let eyeball = is_eyeball(ch);

        if eyeball != buf_is_eyeball && !buf.is_empty() {
            let style = if buf_is_eyeball {
                eyeball_style()
            } else {
                face_style()
            };
            spans.push(Span::styled(std::mem::take(&mut buf), style));
        }

        buf_is_eyeball = eyeball;
        buf.push(ch);
    }

    if !buf.is_empty() {
        let style = if buf_is_eyeball {
            eyeball_style()
        } else {
            face_style()
        };
        spans.push(Span::styled(buf, style));
    }

    spans
}

/// The face row for a pose, always [`MIKMIK_WIDTH`] characters.
///
/// `Loading` cycles a stroke through both eyes so the cat looks like it is
/// tracking something the user cannot see.
fn face_row(pose: &MikMikPose) -> &'static str {
    match pose {
        MikMikPose::Default => "( o.o )",
        MikMikPose::Blink => "( -.- )",
        // The pair slides inside the brackets rather than changing shape, so
        // the row width never moves.
        MikMikPose::LookLeft => "(o.o  )",
        MikMikPose::LookRight => "(  o.o)",
        MikMikPose::LookDown => "( v.v )",
        MikMikPose::Loading { frame } => {
            // One step every 5 frames, matching the old spinner's cadence.
            const PHASES: [&str; 4] = ["( |.| )", "( /./ )", "( -.- )", "( \\.\\ )"];
            PHASES[(frame / 5) as usize % PHASES.len()]
        }
    }
}

/// Returns 4 Lines representing the MikMik mascot:
///   [0] — ears
///   [1] — face (eyes vary with the pose)
///   [2] — whiskers and muzzle
///   [3] — blank spacing line
pub fn mikmik_lines(pose: &MikMikPose) -> [Line<'static>; 4] {
    // Row 1: ears.
    let row1 = Line::from(vec![Span::styled(" /\\_/\\ ".to_string(), body_style())]);

    // Row 2: face. The eye characters are highlighted white inside it.
    let row2 = Line::from(face_spans(face_row(pose)));

    // Row 3: whiskers and muzzle. Fixed across every pose; only the eyes move.
    let row3 = Line::from(vec![Span::styled(" > ^ < ".to_string(), body_style())]);

    // Row 4: blank spacing.
    let row4 = Line::from("");

    [row1, row2, row3, row4]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    fn all_poses() -> Vec<MikMikPose> {
        vec![
            MikMikPose::Default,
            MikMikPose::LookLeft,
            MikMikPose::LookRight,
            MikMikPose::LookDown,
            MikMikPose::Blink,
            MikMikPose::Loading { frame: 0 },
            MikMikPose::Loading { frame: 7 },
        ]
    }

    #[test]
    fn every_pose_is_three_rows_of_the_declared_width() {
        // The welcome screen centres on MIKMIK_WIDTH instead of measuring, so
        // a row of another width would render off-centre.
        for pose in all_poses() {
            let lines = mikmik_lines(&pose);
            for (i, line) in lines.iter().take(3).enumerate() {
                assert_eq!(
                    line_text(line).chars().count(),
                    MIKMIK_WIDTH as usize,
                    "row {i} of {pose:?} is the wrong width"
                );
            }
            assert_eq!(line_text(&lines[3]), "", "row 3 is the spacing row");
        }
    }

    #[test]
    fn blinking_closes_both_eyes() {
        let open = line_text(&mikmik_lines(&MikMikPose::Default)[1]);
        let shut = line_text(&mikmik_lines(&MikMikPose::Blink)[1]);
        assert_eq!(open, "( o.o )");
        assert_eq!(shut, "( -.- )");
        assert!(!shut.contains('o'), "an open eye survived the blink");
    }

    #[test]
    fn the_glances_move_the_eyes_to_opposite_sides() {
        let left = line_text(&mikmik_lines(&MikMikPose::LookLeft)[1]);
        let right = line_text(&mikmik_lines(&MikMikPose::LookRight)[1]);
        let ahead = line_text(&mikmik_lines(&MikMikPose::Default)[1]);

        assert_eq!(left, "(o.o  )");
        assert_eq!(right, "(  o.o)");
        assert_ne!(left, ahead);
        assert_ne!(right, ahead);
        // Mirror images: the eyes sit as far left as they sit right.
        assert_eq!(left.find('o'), right.rfind('o').map(|at| 6 - at));
    }

    #[test]
    fn looking_down_lowers_the_eyes_without_closing_them() {
        let down = line_text(&mikmik_lines(&MikMikPose::LookDown)[1]);
        assert_eq!(down, "( v.v )");
        assert_ne!(down, line_text(&mikmik_lines(&MikMikPose::Blink)[1]));
    }

    #[test]
    fn only_the_face_row_changes_between_poses() {
        // Ears and whiskers are fixed, so the eyes are the only thing the
        // reader can see moving.
        for pose in all_poses() {
            let lines = mikmik_lines(&pose);
            assert_eq!(line_text(&lines[0]), " /\\_/\\ ", "ears moved on {pose:?}");
            assert_eq!(
                line_text(&lines[2]),
                " > ^ < ",
                "whiskers moved on {pose:?}"
            );
        }
    }

    #[test]
    fn the_loading_spinner_cycles_through_four_distinct_faces() {
        let seen: std::collections::HashSet<String> = (0..4)
            .map(|step| line_text(&mikmik_lines(&MikMikPose::Loading { frame: step * 5 })[1]))
            .collect();
        assert_eq!(seen.len(), 4, "the spinner repeats within one rotation");

        // It rotates rather than drifting: frame 20 is back to frame 0.
        assert_eq!(
            line_text(&mikmik_lines(&MikMikPose::Loading { frame: 0 })[1]),
            line_text(&mikmik_lines(&MikMikPose::Loading { frame: 20 })[1]),
        );
    }

    #[test]
    fn only_the_eyes_are_highlighted_white() {
        // The muzzle and brackets must stay pink, or the face reads as a
        // solid white blob at a glance.
        let spans = face_spans("( o.o )");
        let white: String = spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::White))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(white, "oo");
    }
}
