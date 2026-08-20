// Find-in-transcript and go-to-message: a one-line bar that docks above the
// prompt, matching the `/effort` selector's docking pattern.
//
// The bar owns only the query and which match is current. The rows a query
// matches are worked out by the renderer, which is the only place the wrapped
// transcript exists, and left on `App` the same way `message_row_map` is.

/// What the docked bar is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindMode {
    /// Ctrl+F: substring search over the rendered transcript.
    #[default]
    Search,
    /// Ctrl+G: a message number to scroll to.
    GoToMessage,
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptFindState {
    /// Whether the bar is docked above the prompt.
    pub visible: bool,
    pub mode: FindMode,
    /// What the user has typed into the bar.
    pub query: String,
    /// Index into the match list of the match currently scrolled to.
    ///
    /// Held rather than derived from the scroll offset so repeated `findNext`
    /// presses walk the list instead of sticking on whichever match happens to
    /// be nearest the viewport.
    pub current: Option<usize>,
}

impl TranscriptFindState {
    pub fn open(&mut self, mode: FindMode) {
        self.visible = true;
        self.mode = mode;
        self.query.clear();
        self.current = None;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
        self.current = None;
    }

    /// Whether a search query is live, so the renderer should highlight it.
    ///
    /// A go-to-message number is not a transcript search, so it highlights
    /// nothing.
    pub fn is_searching(&self) -> bool {
        self.mode == FindMode::Search && !self.query.is_empty()
    }

    pub fn push_char(&mut self, c: char) {
        if self.mode == FindMode::GoToMessage && !c.is_ascii_digit() {
            return;
        }
        self.query.push(c);
        // A changed query invalidates the position in the old match list.
        self.current = None;
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.current = None;
    }

    /// The message number typed into a go-to bar, if it is one.
    pub fn target_message(&self) -> Option<usize> {
        if self.mode != FindMode::GoToMessage {
            return None;
        }
        self.query.parse::<usize>().ok()
    }

    /// Advance to the next match, wrapping at the end.
    ///
    /// `total` is the match count the last render reported. Returns the new
    /// index, or `None` when there is nothing to step through.
    pub fn step(&mut self, total: usize, forward: bool) -> Option<usize> {
        if total == 0 {
            self.current = None;
            return None;
        }
        let next = match self.current {
            None if forward => 0,
            None => total - 1,
            Some(i) if forward => (i + 1) % total,
            Some(i) => (i + total - 1) % total,
        };
        self.current = Some(next);
        Some(next)
    }

    /// One-line label for the bar.
    ///
    /// `match_count` comes from the caller because only the render pass knows
    /// it; keeping a copy here would be a second answer that can go stale.
    pub fn label(&self, match_count: usize) -> String {
        match self.mode {
            FindMode::Search => {
                let position = match (self.current, match_count) {
                    (_, 0) if self.query.is_empty() => String::new(),
                    (_, 0) => "  no matches".to_string(),
                    (Some(i), total) => format!("  {}/{}", i + 1, total),
                    (None, total) => format!("  {total} matches"),
                };
                format!("Find: {}{}", self.query, position)
            }
            FindMode::GoToMessage => format!("Go to message #{}", self.query),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_in_both_directions() {
        let mut s = TranscriptFindState::default();

        assert_eq!(s.step(3, true), Some(0));
        assert_eq!(s.step(3, true), Some(1));
        assert_eq!(s.step(3, true), Some(2));
        assert_eq!(s.step(3, true), Some(0), "forward did not wrap");

        assert_eq!(s.step(3, false), Some(2), "backward did not wrap");
        assert_eq!(s.step(3, false), Some(1));
    }

    #[test]
    fn a_first_backward_step_lands_on_the_last_match() {
        let mut s = TranscriptFindState::default();
        assert_eq!(s.step(4, false), Some(3));
    }

    #[test]
    fn stepping_an_empty_match_list_yields_nothing() {
        let mut s = TranscriptFindState {
            current: Some(2),
            ..Default::default()
        };
        assert_eq!(s.step(0, true), None);
        assert_eq!(s.current, None);
    }

    #[test]
    fn editing_the_query_forgets_the_position() {
        let mut s = TranscriptFindState::default();
        s.step(3, true);
        s.push_char('a');
        assert_eq!(s.current, None);

        s.step(3, true);
        s.pop_char();
        assert_eq!(s.current, None);
    }

    #[test]
    fn a_go_to_bar_takes_digits_only() {
        let mut s = TranscriptFindState::default();
        s.open(FindMode::GoToMessage);
        s.push_char('1');
        s.push_char('x');
        s.push_char('2');
        assert_eq!(s.query, "12");
        assert_eq!(s.target_message(), Some(12));
    }

    #[test]
    fn a_search_bar_takes_any_character_and_names_no_target() {
        let mut s = TranscriptFindState::default();
        s.open(FindMode::Search);
        s.push_char('1');
        s.push_char('x');
        assert_eq!(s.query, "1x");
        assert_eq!(s.target_message(), None);
    }

    #[test]
    fn only_a_non_empty_search_asks_for_highlighting() {
        let mut s = TranscriptFindState::default();
        s.open(FindMode::Search);
        assert!(!s.is_searching());
        s.push_char('a');
        assert!(s.is_searching());

        s.open(FindMode::GoToMessage);
        s.push_char('3');
        assert!(!s.is_searching(), "a message number is not a search");
    }

    #[test]
    fn the_label_reports_the_position_and_the_empty_case() {
        let mut s = TranscriptFindState::default();
        s.open(FindMode::Search);
        assert_eq!(s.label(0), "Find: ");

        s.push_char('a');
        assert_eq!(s.label(0), "Find: a  no matches");

        assert_eq!(s.label(3), "Find: a  3 matches");
        s.step(3, true);
        assert_eq!(s.label(3), "Find: a  1/3");
    }
}
