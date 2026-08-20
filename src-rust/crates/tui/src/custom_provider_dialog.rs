// custom_provider_dialog.rs — Modal dialog for adding an account by hand.
//
// Collects the three things an account needs that cannot be inferred: the name
// it will be stored and addressed under, the endpoint, and the credential. The
// protocol comes from the entry the user picked in /connect, so the same
// dialog serves an Anthropic-format gateway and an OpenAI-compatible one.

use ratatui::layout::Rect;
use ratatui::prelude::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::overlays::{centered_rect, render_dark_overlay, render_dialog_bg, MIKMIK_PANEL_BG};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomProviderField {
    Account,
    Url,
    ApiKey,
}

pub struct CustomProviderDialogState {
    pub visible: bool,
    /// Wire format the endpoint speaks, taken from the connect picker.
    pub provider_id: String,
    pub provider_name: String,
    /// Name this endpoint is stored and addressed under.
    ///
    /// Separate from `provider_id` so two endpoints speaking the same protocol
    /// can coexist, and so `"<account>/<model>"` can name one of them.
    pub account_input: String,
    pub url_input: String,
    pub api_key_input: String,
    pub active_field: CustomProviderField,
}

impl Default for CustomProviderDialogState {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomProviderDialogState {
    pub fn new() -> Self {
        Self {
            visible: false,
            provider_id: String::new(),
            provider_name: String::new(),
            account_input: String::new(),
            url_input: String::new(),
            api_key_input: String::new(),
            active_field: CustomProviderField::Account,
        }
    }

    pub fn open(
        &mut self,
        provider_id: String,
        provider_name: String,
        current_url: Option<String>,
    ) {
        self.visible = true;
        // Default the account to the protocol's own name, which is what the
        // entry was keyed by before accounts could be named.
        self.account_input = provider_id.clone();
        self.provider_id = provider_id;
        self.provider_name = provider_name;
        self.url_input = current_url.unwrap_or_default();
        self.api_key_input.clear();
        self.active_field = CustomProviderField::Account;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.account_input.clear();
        self.url_input.clear();
        self.api_key_input.clear();
        self.active_field = CustomProviderField::Account;
    }

    pub fn move_next_field(&mut self) {
        self.active_field = match self.active_field {
            CustomProviderField::Account => CustomProviderField::Url,
            CustomProviderField::Url => CustomProviderField::ApiKey,
            CustomProviderField::ApiKey => CustomProviderField::Account,
        };
    }

    pub fn move_prev_field(&mut self) {
        self.active_field = match self.active_field {
            CustomProviderField::Account => CustomProviderField::ApiKey,
            CustomProviderField::Url => CustomProviderField::Account,
            CustomProviderField::ApiKey => CustomProviderField::Url,
        };
    }

    pub fn insert_char(&mut self, c: char) {
        match self.active_field {
            CustomProviderField::Account => self.account_input.push(c),
            CustomProviderField::Url => self.url_input.push(c),
            CustomProviderField::ApiKey => self.api_key_input.push(c),
        }
    }

    pub fn backspace(&mut self) {
        match self.active_field {
            CustomProviderField::Account => {
                self.account_input.pop();
            }
            CustomProviderField::Url => {
                self.url_input.pop();
            }
            CustomProviderField::ApiKey => {
                self.api_key_input.pop();
            }
        }
    }

    /// Whether the typed account name can be stored and addressed.
    pub fn account_name_is_valid(&self) -> bool {
        mikmik_core::config::account_name_is_valid(&self.account_input)
    }

    pub fn can_submit(&self) -> bool {
        !self.url_input.trim().is_empty() && self.account_name_is_valid()
    }

    /// Returns `(account, protocol, url, api_key)`.
    pub fn take_values(&mut self) -> (String, String, String, String) {
        let account = self.account_input.trim().to_string();
        let protocol = self.provider_id.clone();
        let url = self.url_input.trim().to_string();
        let api_key = self.api_key_input.clone();
        self.close();
        (account, protocol, url, api_key)
    }
}

pub fn render_custom_provider_dialog(
    frame: &mut Frame,
    state: &CustomProviderDialogState,
    area: Rect,
) {
    if !state.visible {
        return;
    }

    let pink = Color::Rgb(233, 30, 99);
    let dim = Color::Rgb(90, 90, 90);
    let muted = Color::Rgb(180, 180, 180);
    let dialog_bg = MIKMIK_PANEL_BG;

    render_dark_overlay(frame, area);

    let width = 76u16.min(area.width.saturating_sub(4));
    // Three fields of two rows each, plus the title, the spacers and the
    // footer. Grew by three rows when the account field was added.
    let height = 16u16;
    let dialog_area = centered_rect(width, height, area);
    render_dialog_bg(frame, dialog_area);

    let inner = Rect {
        x: dialog_area.x + 1,
        y: dialog_area.y + 1,
        width: dialog_area.width.saturating_sub(2),
        height: dialog_area.height.saturating_sub(2),
    };

    let title_text = format!("Connect {}", state.provider_name);
    let title_pad = inner.width.saturating_sub(title_text.len() as u16 + 5) as usize;

    let account_style = if state.active_field == CustomProviderField::Account {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let url_style = if state.active_field == CustomProviderField::Url {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let key_style = if state.active_field == CustomProviderField::ApiKey {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let url_text = if state.url_input.is_empty() {
        // The same dialog serves both custom protocols, so the hint follows
        // the one that was picked instead of always naming OpenAI's.
        if state.provider_id == "custom-anthropic" {
            "https://your-anthropic-compatible-endpoint".to_string()
        } else {
            "https://your-openai-compatible-endpoint/v1".to_string()
        }
    } else {
        state.url_input.clone()
    };

    let masked_key = if state.api_key_input.is_empty() {
        "paste your API key here...".to_string()
    } else {
        let chars: Vec<char> = state.api_key_input.chars().collect();
        if chars.len() <= 4 {
            state.api_key_input.clone()
        } else {
            let visible: String = chars[chars.len() - 4..].iter().collect();
            format!("{}{}", "•".repeat(chars.len() - 4), visible)
        }
    };

    let account_text = if state.account_input.is_empty() {
        "name this account...".to_string()
    } else {
        state.account_input.clone()
    };

    let confirm_hint = if state.can_submit() {
        " enter confirm"
    } else if !state.account_name_is_valid() {
        " name: no spaces or /"
    } else {
        " fill URL field"
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {}", title_text),
            Style::default().fg(pink).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>width$}", "esc ", width = title_pad),
            Style::default().fg(dim),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" Account name:", Style::default().fg(muted)),
        Span::styled(
            format!("  (speaks {})", state.provider_id),
            Style::default().fg(dim),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!(" {}", account_text), account_style),
        Span::styled(
            if state.active_field == CustomProviderField::Account {
                "_"
            } else {
                ""
            },
            Style::default().fg(pink),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " URL:",
        Style::default().fg(muted),
    )]));
    lines.push(Line::from(vec![
        Span::styled(format!(" {}", url_text), url_style),
        Span::styled(
            if state.active_field == CustomProviderField::Url {
                "_"
            } else {
                ""
            },
            Style::default().fg(pink),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        " API Key:",
        Style::default().fg(muted),
    )]));
    lines.push(Line::from(vec![
        Span::styled(format!(" {}", masked_key), key_style),
        Span::styled(
            if state.active_field == CustomProviderField::ApiKey {
                "_"
            } else {
                ""
            },
            Style::default().fg(pink),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" tab", Style::default().fg(dim)),
        Span::styled(" switch field  ", Style::default().fg(dim)),
        Span::styled(confirm_hint, Style::default().fg(dim)),
    ]));

    let para = Paragraph::new(lines).bg(dialog_bg);
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn opened() -> CustomProviderDialogState {
        let mut state = CustomProviderDialogState::new();
        state.open(
            "custom-anthropic".to_string(),
            "Custom Anthropic".to_string(),
            None,
        );
        state
    }

    fn type_into(state: &mut CustomProviderDialogState, text: &str) {
        for c in text.chars() {
            state.insert_char(c);
        }
    }

    #[test]
    fn the_account_defaults_to_the_protocol_name() {
        // Confirming straight through reproduces the pre-accounts behaviour,
        // where the entry was keyed by the protocol's own name.
        let state = opened();
        assert_eq!(state.account_input, "custom-anthropic");
        assert_eq!(state.active_field, CustomProviderField::Account);
    }

    #[test]
    fn tab_visits_every_field_and_returns() {
        let mut state = opened();
        let mut seen = vec![state.active_field];
        for _ in 0..3 {
            state.move_next_field();
            seen.push(state.active_field);
        }
        assert_eq!(
            seen,
            vec![
                CustomProviderField::Account,
                CustomProviderField::Url,
                CustomProviderField::ApiKey,
                CustomProviderField::Account,
            ]
        );
    }

    #[test]
    fn shift_tab_walks_back_the_same_way() {
        let mut state = opened();
        state.move_next_field();
        state.move_prev_field();
        assert_eq!(state.active_field, CustomProviderField::Account);
    }

    #[test]
    fn a_slash_in_the_account_name_is_refused() {
        // A slash is the "<account>/<model>" separator, so an account carrying
        // one could never be addressed.
        let mut state = opened();
        state.account_input.clear();
        type_into(&mut state, "my/gateway");
        state.move_next_field();
        type_into(&mut state, "http://127.0.0.1:8789");
        assert!(!state.account_name_is_valid());
        assert!(!state.can_submit());
    }

    #[test]
    fn whitespace_in_the_account_name_is_refused() {
        let mut state = opened();
        state.account_input.clear();
        type_into(&mut state, "my gateway");
        assert!(!state.account_name_is_valid());
    }

    #[test]
    fn an_empty_account_name_is_refused() {
        let mut state = opened();
        state.account_input.clear();
        state.move_next_field();
        type_into(&mut state, "http://127.0.0.1:8789");
        assert!(!state.can_submit(), "an unnamed account cannot be keyed");
    }

    #[test]
    fn submitting_returns_the_account_and_its_protocol_separately() {
        let mut state = opened();
        state.account_input.clear();
        type_into(&mut state, "is_gateway");
        state.move_next_field();
        type_into(&mut state, "  http://127.0.0.1:8789  ");
        state.move_next_field();
        type_into(&mut state, "sk-test");

        assert!(state.can_submit());
        let (account, protocol, url, key) = state.take_values();
        assert_eq!(account, "is_gateway");
        assert_eq!(protocol, "custom-anthropic", "the wire format is separate");
        assert_eq!(url, "http://127.0.0.1:8789", "the url is trimmed");
        assert_eq!(key, "sk-test");
        assert!(!state.visible, "submitting closes the dialog");
    }

    #[test]
    fn the_dialog_shows_all_three_fields_without_overflowing() {
        // The box grew by three rows for the account field; a row that falls
        // outside it is invisible rather than an error, so assert on cells.
        let mut state = opened();
        state.account_input.clear();
        type_into(&mut state, "is_gateway");

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        terminal
            .draw(|frame| render_custom_provider_dialog(frame, &state, frame.area()))
            .expect("draw");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Account name:"), "account field missing");
        assert!(rendered.contains("is_gateway"), "typed name missing");
        assert!(rendered.contains("URL:"), "url field missing");
        assert!(rendered.contains("API Key:"), "key field missing");
        assert!(
            rendered.contains("custom-anthropic"),
            "the protocol is not shown, so the user cannot tell what it speaks"
        );
    }

    #[test]
    fn a_hidden_dialog_draws_nothing() {
        let state = CustomProviderDialogState::new();
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("terminal");
        terminal
            .draw(|frame| render_custom_provider_dialog(frame, &state, frame.area()))
            .expect("draw");
        assert!(!terminal.backend().to_string().contains("Account name:"));
    }
}
