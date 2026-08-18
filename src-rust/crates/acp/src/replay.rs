//! Turning a stored transcript back into the updates a client already knows.
//!
//! `session/load` asks the agent to hand the whole conversation over again as
//! `session/update` notifications, in the order it happened, so the editor can
//! draw a session it never saw being written. Nothing here talks to the
//! provider or the connection: it maps messages to updates, which is what
//! makes it testable.

use agent_client_protocol_schema as acp;
use claurst_core::types::{ContentBlock, Message, MessageContent, Role, ToolResultContent};

/// Every update needed to redraw `messages`, in the order they happened.
pub fn updates_for(messages: &[Message]) -> Vec<acp::SessionUpdate> {
    let mut updates = Vec::new();
    for message in messages {
        match &message.content {
            // A message stored as plain text has no blocks to walk; it is the
            // shape every prompt arrives in, so it cannot be skipped.
            MessageContent::Text(text) => updates.push(text_chunk(&message.role, text)),
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    updates.extend(update_for(&message.role, block));
                }
            }
        }
    }
    finish_unanswered_calls(messages, &mut updates);
    updates
}

/// The content blocks of a message. A message stored as plain text carries
/// none, and no tool call is ever recorded in that shape.
fn blocks_of(message: &Message) -> &[ContentBlock] {
    match &message.content {
        MessageContent::Text(_) => &[],
        MessageContent::Blocks(blocks) => blocks.as_slice(),
    }
}

/// One block as the updates that describe it, if it has any.
fn update_for(role: &Role, block: &ContentBlock) -> Vec<acp::SessionUpdate> {
    match block {
        ContentBlock::Text { text } => vec![text_chunk(role, text)],
        ContentBlock::Thinking { thinking, .. } => vec![acp::SessionUpdate::AgentThoughtChunk(
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                thinking.clone(),
            ))),
        )],
        ContentBlock::ToolUse {
            id, name, input, ..
        } => {
            let kind = crate::prompt::classify_tool_kind(name);
            let title = crate::prompt::tool_title(name, Some(input));
            vec![acp::SessionUpdate::ToolCall(
                acp::ToolCall::new(acp::ToolCallId::new(id.as_str()), title)
                    .kind(kind)
                    .status(acp::ToolCallStatus::InProgress)
                    .raw_input(Some(input.clone())),
            )]
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let status = if is_error.unwrap_or(false) {
                acp::ToolCallStatus::Failed
            } else {
                acp::ToolCallStatus::Completed
            };
            let text = tool_result_text(content);
            vec![acp::SessionUpdate::ToolCallUpdate(
                acp::ToolCallUpdate::new(
                    acp::ToolCallId::new(tool_use_id.as_str()),
                    acp::ToolCallUpdateFields::new()
                        .status(status)
                        .content(vec![acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::Text(acp::TextContent::new(text)),
                        ))]),
                ),
            )]
        }
        // An image, a document or a local shell block has no place in the
        // replay: the protocol carries them per prompt, not per transcript.
        _ => Vec::new(),
    }
}

/// A message chunk attributed to whoever said it.
fn text_chunk(role: &Role, text: &str) -> acp::SessionUpdate {
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
        text.to_string(),
    )));
    match role {
        Role::User => acp::SessionUpdate::UserMessageChunk(chunk),
        _ => acp::SessionUpdate::AgentMessageChunk(chunk),
    }
}

/// The text of a tool result, whichever shape it was stored in.
fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Close out a call whose result was never recorded.
///
/// A turn cancelled mid-tool leaves the call open in the transcript. Replaying
/// it as-is leaves the client showing a tool that runs forever, so each one is
/// closed as failed and says why.
fn finish_unanswered_calls(messages: &[Message], updates: &mut Vec<acp::SessionUpdate>) {
    let mut started: Vec<&str> = Vec::new();
    let mut answered: Vec<&str> = Vec::new();
    for block in messages.iter().flat_map(blocks_of) {
        match block {
            ContentBlock::ToolUse { id, .. } => started.push(id.as_str()),
            ContentBlock::ToolResult { tool_use_id, .. } => answered.push(tool_use_id.as_str()),
            _ => {}
        }
    }
    for id in started.into_iter().filter(|id| !answered.contains(id)) {
        updates.push(acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new(id),
                acp::ToolCallUpdateFields::new()
                    .status(acp::ToolCallStatus::Failed)
                    .content(vec![acp::ToolCallContent::Content(acp::Content::new(
                        acp::ContentBlock::Text(acp::TextContent::new(
                            "The session ended before this tool reported a result.".to_string(),
                        )),
                    ))]),
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_of(update: &acp::SessionUpdate) -> Option<&str> {
        let chunk = match update {
            acp::SessionUpdate::UserMessageChunk(c)
            | acp::SessionUpdate::AgentMessageChunk(c)
            | acp::SessionUpdate::AgentThoughtChunk(c) => c,
            _ => return None,
        };
        match &chunk.content {
            acp::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        }
    }

    fn user_blocks(blocks: Vec<ContentBlock>) -> Message {
        Message {
            role: Role::User,
            content: MessageContent::Blocks(blocks),
            uuid: None,
            cost: None,
            snapshot_patch: None,
            timestamp: None,
        }
    }

    #[test]
    fn a_conversation_replays_in_the_order_it_happened() {
        let messages = vec![
            user_blocks(vec![ContentBlock::Text {
                text: "count the crates".to_string(),
            }]),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "twelve".to_string(),
            }]),
        ];

        let updates = updates_for(&messages);

        assert_eq!(updates.len(), 2);
        assert!(matches!(
            updates[0],
            acp::SessionUpdate::UserMessageChunk(_)
        ));
        assert_eq!(text_of(&updates[0]), Some("count the crates"));
        assert!(matches!(
            updates[1],
            acp::SessionUpdate::AgentMessageChunk(_)
        ));
        assert_eq!(text_of(&updates[1]), Some("twelve"));
    }

    #[test]
    fn a_tool_call_replays_as_the_call_and_its_result() {
        let messages = vec![
            Message::assistant_blocks(vec![ContentBlock::ToolUse {
                id: "call-1".to_string(),
                name: "Read".to_string(),
                input: json!({ "file_path": "/tmp/a.rs" }),
                thought_signature: None,
            }]),
            user_blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".to_string(),
                content: ToolResultContent::Text("fn main() {}".to_string()),
                is_error: None,
            }]),
        ];

        let updates = updates_for(&messages);

        assert_eq!(updates.len(), 2);
        let acp::SessionUpdate::ToolCall(call) = &updates[0] else {
            panic!("expected a tool call, got {:?}", updates[0]);
        };
        assert_eq!(call.tool_call_id.0.as_ref(), "call-1");
        assert_eq!(call.kind, acp::ToolKind::Read);
        let acp::SessionUpdate::ToolCallUpdate(update) = &updates[1] else {
            panic!("expected a tool call update, got {:?}", updates[1]);
        };
        assert_eq!(update.tool_call_id.0.as_ref(), "call-1");
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    }

    #[test]
    fn a_failed_tool_replays_as_failed() {
        let messages = vec![user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: "call-9".to_string(),
            content: ToolResultContent::Text("no such file".to_string()),
            is_error: Some(true),
        }])];

        let updates = updates_for(&messages);

        let acp::SessionUpdate::ToolCallUpdate(update) = &updates[0] else {
            panic!("expected a tool call update, got {:?}", updates[0]);
        };
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
    }

    #[test]
    fn a_tool_that_never_reported_back_is_closed_rather_than_left_running() {
        // Otherwise the client draws a spinner that nothing will ever stop.
        let messages = vec![Message::assistant_blocks(vec![ContentBlock::ToolUse {
            id: "call-2".to_string(),
            name: "Bash".to_string(),
            input: json!({ "command": "sleep 100" }),
            thought_signature: None,
        }])];

        let updates = updates_for(&messages);

        assert_eq!(updates.len(), 2);
        let acp::SessionUpdate::ToolCallUpdate(update) = &updates[1] else {
            panic!("expected a closing update, got {:?}", updates[1]);
        };
        assert_eq!(update.tool_call_id.0.as_ref(), "call-2");
        assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
    }

    #[test]
    fn thinking_replays_as_thought_rather_than_as_an_answer() {
        let messages = vec![Message::assistant_blocks(vec![ContentBlock::Thinking {
            thinking: "the parser runs first".to_string(),
            signature: String::new(),
        }])];

        let updates = updates_for(&messages);

        assert!(matches!(
            updates[0],
            acp::SessionUpdate::AgentThoughtChunk(_)
        ));
        assert_eq!(text_of(&updates[0]), Some("the parser runs first"));
    }

    #[test]
    fn an_empty_transcript_replays_as_nothing() {
        assert!(updates_for(&[]).is_empty());
    }
}
