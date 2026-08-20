// Runner submodule: give a turn that produced nothing a visible answer.
//
// Both dispatch arms of `run_query_loop` need this. A provider that ends a
// turn with no text, no thinking and no tool calls leaves the user looking at
// an unchanged screen with no way to tell whether the agent is still working,
// crashed, or simply finished — the "agent randomly stops" report.

use claurst_core::types::{ContentBlock, Message, MessageContent};
use tokio::sync::mpsc;

use crate::QueryEvent;

/// Whether `msg` carries nothing a user could read or act on.
fn is_silent(msg: &Message) -> bool {
    msg.get_all_text().trim().is_empty()
        && msg.get_thinking_blocks().is_empty()
        && msg.get_tool_use_blocks().is_empty()
}

/// Append a placeholder to a turn that ended without producing any output, so
/// the transcript always says the turn is over.
///
/// Streams the placeholder to `event_tx` as a text delta before appending it,
/// so a front end that renders the stream and a front end that renders the
/// stored message show the same thing. `messages` is expected to end with
/// `assistant_msg`; its last entry is refreshed from the amended copy.
///
/// Returns whether a placeholder was inserted.
pub(crate) fn ensure_turn_has_output(
    assistant_msg: &mut Message,
    messages: &mut [Message],
    event_tx: Option<&mpsc::UnboundedSender<QueryEvent>>,
    stop_reason: &str,
) -> bool {
    if !is_silent(assistant_msg) {
        return false;
    }

    let placeholder =
        format!("(no response — model ended the turn with stop_reason \"{stop_reason}\")");

    if let Some(tx) = event_tx {
        let _ = tx.send(QueryEvent::Stream(
            claurst_api::streaming::AnthropicStreamEvent::ContentBlockDelta {
                index: 0,
                delta: claurst_api::streaming::ContentDelta::TextDelta {
                    text: placeholder.clone(),
                },
            },
        ));
    }

    match assistant_msg.content {
        MessageContent::Blocks(ref mut blocks) => blocks.push(ContentBlock::Text {
            text: placeholder.clone(),
        }),
        // A silent turn can also arrive as an empty `Text` payload; replacing it
        // keeps the shape the provider chose instead of forcing blocks on it.
        MessageContent::Text(ref mut text) => *text = placeholder.clone(),
    }

    if let Some(last) = messages.last_mut() {
        *last = assistant_msg.clone();
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_silent_turn_gets_a_placeholder_in_the_message_and_the_list() {
        let mut msg = Message::assistant_blocks(vec![]);
        let mut messages = vec![msg.clone()];

        assert!(ensure_turn_has_output(
            &mut msg,
            &mut messages,
            None,
            "end_turn"
        ));

        assert!(msg.get_all_text().contains("no response"));
        assert!(msg.get_all_text().contains("end_turn"));
        // The stored copy has to carry it too, or the transcript and the
        // in-flight message disagree about what the turn produced.
        assert_eq!(messages[0].get_all_text(), msg.get_all_text());
    }

    #[test]
    fn an_empty_text_payload_counts_as_silent() {
        let mut msg = Message::assistant("   ");
        let mut messages = vec![msg.clone()];

        assert!(ensure_turn_has_output(
            &mut msg,
            &mut messages,
            None,
            "stop"
        ));
        assert!(msg.get_all_text().contains("no response"));
    }

    #[test]
    fn a_turn_with_text_is_left_alone() {
        let mut msg = Message::assistant_blocks(vec![ContentBlock::Text {
            text: "done".to_string(),
        }]);
        let mut messages = vec![msg.clone()];

        assert!(!ensure_turn_has_output(
            &mut msg,
            &mut messages,
            None,
            "end_turn"
        ));
        assert_eq!(msg.get_all_text(), "done");
    }

    /// A tool round is not silent: the tool call is the output, and adding a
    /// placeholder beside it would put "no response" next to a real one.
    #[test]
    fn a_turn_with_only_a_tool_call_is_left_alone() {
        let mut msg = Message::assistant_blocks(vec![ContentBlock::ToolUse {
            id: "t1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
            thought_signature: None,
        }]);
        let mut messages = vec![msg.clone()];

        assert!(!ensure_turn_has_output(
            &mut msg,
            &mut messages,
            None,
            "tool_use"
        ));
        assert!(msg.get_all_text().trim().is_empty());
    }

    /// Thinking with no text is still an answer the user can read.
    #[test]
    fn a_turn_with_only_thinking_is_left_alone() {
        let mut msg = Message::assistant_blocks(vec![ContentBlock::Thinking {
            thinking: "weighing it up".to_string(),
            signature: String::new(),
        }]);
        let mut messages = vec![msg.clone()];

        assert!(!ensure_turn_has_output(
            &mut msg,
            &mut messages,
            None,
            "end_turn"
        ));
    }
}
