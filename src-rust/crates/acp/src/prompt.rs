//! `session/prompt` handler — drives the Claurst query loop and forwards
//! every meaningful event back to the ACP client as a `session/update`
//! notification.

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol_schema as acp;
use claurst_api::streaming::{AnthropicStreamEvent, ContentDelta};
use claurst_core::types::Message;
use claurst_query::{QueryEvent, QueryOutcome};
use claurst_tools::ToolContext;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::connection::Connection;
use crate::permission::AcpPermissionHandler;
use crate::runtime::AgentRuntime;
use crate::sessions::SessionState;

/// Handle one `session/prompt` JSON-RPC call.
///
/// Drives the full Claurst query loop with the runtime's tools, MCP servers,
/// and provider registry, while streaming every text delta, thinking delta,
/// and tool invocation back as `session/update` notifications. Returns the
/// final `PromptResponse` with the appropriate `StopReason`.
pub async fn handle(
    runtime: Arc<AgentRuntime>,
    connection: Arc<Connection>,
    session: Arc<SessionState>,
    params: acp::PromptRequest,
) -> Result<acp::PromptResponse, acp::Error> {
    // Convert prompt content blocks → a single user message in Claurst's
    // internal format.
    let user_text = render_prompt_blocks(&params.prompt);
    if user_text.trim().is_empty() {
        return Err(acp::Error::invalid_params());
    }

    // A prompt naming a slash command is the command layer's to answer, and
    // only what it hands back becomes a turn.
    let turn_text = match crate::commands::split_command(&user_text) {
        Some((name, arguments)) => {
            match run_command(&runtime, &connection, &session, &name, &arguments).await {
                CommandTurn::Prompt(text) => text,
                CommandTurn::Answered(stop) => return Ok(acp::PromptResponse::new(stop)),
            }
        }
        None => user_text,
    };

    // Append the user turn to the session transcript.
    let mut messages: Vec<Message> = {
        let guard = session.messages.lock();
        guard.clone()
    };
    messages.push(Message::user(turn_text));

    // Reset the session's cancellation token for this new turn.
    let cancel = session.cancel_token.clone();

    // What the client has changed for this session sits on top of the
    // runtime's configuration, and only for the turns of this session.
    let overrides = session.settings.lock().clone();
    let mut config = runtime.config.clone();
    crate::session_config::apply_overrides(&mut config, &overrides);

    // Build per-session ToolContext.
    let permission_handler: Arc<dyn claurst_core::PermissionHandler> =
        Arc::new(AcpPermissionHandler);
    let tool_ctx = ToolContext {
        working_dir: session.cwd.lock().clone(),
        permission_mode: config.permission_mode.clone(),
        permission_handler,
        cost_tracker: runtime.cost_tracker.clone(),
        session_id: session.session_id.0.to_string(),
        file_history: session.file_history.clone(),
        current_turn: session.current_turn.clone(),
        non_interactive: false, // ACP routes permissions via the bridge
        mcp_manager: runtime.mcp_manager.clone(),
        managed_agent_config: config.managed_agents.clone(),
        config: config.clone(),
        completion_notifier: None,
        pending_permissions: Some(session.pending_permissions.clone()),
        permission_manager: Some(runtime.permission_manager.clone()),
        user_question_tx: None,
        // Bind to this turn's cancel token so the parallel tool executor and any
        // sub-agents observe cancellation (issue #218). `run_query_loop` also
        // rebinds this to the token it is driven by.
        cancel_token: cancel.clone(),
    };

    // Spawn the permission drainer for this turn.
    let drainer_cancel = CancellationToken::new();
    let drainer = crate::permission::spawn_drainer(
        connection.clone(),
        session.session_id.clone(),
        session.pending_permissions.clone(),
        drainer_cancel.clone(),
    );

    // Event channel + forwarder.
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<QueryEvent>();
    let forwarder = tokio::spawn(forward_events(
        connection.clone(),
        session.session_id.clone(),
        session.file_history.clone(),
        ev_rx,
    ));

    // The session names its own directory in `session/new`, which need not be
    // the one the runtime was started in, so the prompt has to describe the
    // session's directory rather than the runtime's.
    let mut query_config = runtime.query_config.clone();
    if let Some(model) = &overrides.model {
        query_config.model = model.clone();
    }
    if let Some(effort) = overrides.effort {
        query_config.effort_level = Some(effort);
    }
    let session_cwd = session.cwd.lock().clone();
    query_config.working_directory = Some(session_cwd.display().to_string());
    query_config.workspace_roots = claurst_core::workspace::generate_root_names(
        &session_cwd,
        &config.additional_dirs,
        &config.workspace_paths,
    )
    .into_iter()
    .map(|(name, path)| (name, path.display().to_string()))
    .collect();

    // Run the query loop.
    let outcome = claurst_query::run_query_loop(
        runtime.api_client.as_ref(),
        &mut messages,
        runtime.tools.as_slice(),
        &tool_ctx,
        &query_config,
        runtime.cost_tracker.clone(),
        Some(ev_tx),
        cancel,
        None,
    )
    .await;

    // Tear down forwarder + drainer.
    drainer_cancel.cancel();
    let _ = drainer.await;
    // Forwarder finishes when ev_tx is dropped at end of run_query_loop.
    let _ = forwarder.await;

    // Persist the updated transcript.
    {
        let mut guard = session.messages.lock();
        *guard = messages;
    }
    // A session nobody has named takes its name from what was asked of it, and
    // the client is told so it can label the conversation it is showing.
    let derived = crate::persist::derive_title(&session.messages.lock());
    let named = {
        let mut title = session.title.lock();
        match (&*title, derived) {
            (None, Some(derived)) => {
                *title = Some(derived.clone());
                Some(derived)
            }
            _ => None,
        }
    };
    if let Some(title) = named {
        send_session_update(
            &connection,
            &session.session_id,
            acp::SessionUpdate::SessionInfoUpdate(
                acp::SessionInfoUpdate::new()
                    .title(title)
                    .updated_at(chrono::Utc::now().to_rfc3339()),
            ),
        )
        .await;
    }

    // And to disk, so the session outlives this connection.
    crate::persist::save(&session, &query_config.model).await;

    let stop_reason = match outcome {
        QueryOutcome::EndTurn { .. } => acp::StopReason::EndTurn,
        QueryOutcome::MaxTokens { .. } => acp::StopReason::MaxTokens,
        QueryOutcome::Cancelled => acp::StopReason::Cancelled,
        QueryOutcome::BudgetExceeded { .. } => acp::StopReason::MaxTurnRequests,
        QueryOutcome::Error(e) => {
            error!(error = ?e, "ACP: query loop errored");
            acp::StopReason::Refusal
        }
    };

    Ok(acp::PromptResponse::new(stop_reason))
}

/// What a slash command left for the turn to do.
enum CommandTurn {
    /// The command answered by itself; the turn is over.
    Answered(acp::StopReason),
    /// The command asked for this text to go to the model instead.
    Prompt(String),
}

/// Run a slash command and tell the client what it did.
///
/// Anything the command reports while it is still working reaches the client
/// as it happens, because a browser URL that arrives with the final answer is
/// too late to open.
async fn run_command(
    runtime: &Arc<AgentRuntime>,
    connection: &Arc<Connection>,
    session: &Arc<SessionState>,
    name: &str,
    arguments: &str,
) -> CommandTurn {
    let (notes_tx, mut notes_rx) = mpsc::unbounded_channel::<String>();
    let relay = tokio::spawn({
        let connection = connection.clone();
        let session_id = session.session_id.clone();
        async move {
            while let Some(note) = notes_rx.recv().await {
                send_text_chunk(&connection, &session_id, &note, false).await;
            }
        }
    });

    let outcome = crate::commands::run(runtime, session, &notes_tx, name, arguments).await;
    drop(notes_tx);
    // The relay outlives this call whenever a command left work running in the
    // background; it ends when that work drops its own sender.
    if outcome.prompt.is_none() {
        // Nothing more will be said by this turn, so waiting costs nothing and
        // keeps the notes ahead of the answer.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), relay).await;
    }

    for update in outcome.updates {
        send_session_update(connection, &session.session_id, update).await;
    }
    if let Some(reply) = &outcome.reply {
        send_text_chunk(connection, &session.session_id, reply, false).await;
    }
    // A command can change the transcript, the name, or the directory, and
    // none of that survives the process without this.
    crate::persist::save(session, &runtime.query_config.model).await;

    match outcome.prompt {
        Some(text) => CommandTurn::Prompt(text),
        None if outcome.failed => CommandTurn::Answered(acp::StopReason::Refusal),
        None => CommandTurn::Answered(acp::StopReason::EndTurn),
    }
}

/// Concatenate text from prompt content blocks.
///
/// A resource the client embedded (an `@file` mention in an editor) arrives
/// with its own uri, which is named alongside the contents: the model cannot
/// answer about a file whose path it was never told. Image and audio blocks
/// are dropped, and `initialize` says so.
fn render_prompt_blocks(blocks: &[acp::ContentBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            acp::ContentBlock::Text(t) => parts.push(t.text.clone()),
            acp::ContentBlock::ResourceLink(link) => {
                parts.push(format!("[resource link: {}]", link.uri));
            }
            acp::ContentBlock::Resource(res) => {
                let json = serde_json::to_value(res).unwrap_or_default();
                let resource = json.get("resource");
                let text = resource
                    .and_then(|r| r.get("text"))
                    .and_then(|t| t.as_str());
                let uri = resource.and_then(|r| r.get("uri")).and_then(|u| u.as_str());
                match (uri, text) {
                    (Some(uri), Some(text)) => parts.push(format!("[{uri}]\n{text}")),
                    (None, Some(text)) => parts.push(text.to_string()),
                    // A resource with no text is a binary blob or a reference
                    // the client expected us to fetch; naming it is better
                    // than dropping it silently.
                    (Some(uri), None) => parts.push(format!("[resource: {uri}]")),
                    (None, None) => warn!("ACP: ignoring a resource block with no uri or text"),
                }
            }
            acp::ContentBlock::Image(_) | acp::ContentBlock::Audio(_) => {
                warn!("ACP: ignoring multimedia content block (capability not advertised)");
            }
            _ => {
                warn!("ACP: ignoring unknown content block variant");
            }
        }
    }
    parts.join("\n\n")
}

/// Pump QueryEvents → `session/update` SessionNotifications.
///
/// `file_history` is the session's own recorder. Every editing tool writes the
/// before and after text of each file it touches there, which is what lets a
/// finished tool call carry a diff instead of a paragraph about one.
async fn forward_events(
    connection: Arc<Connection>,
    session_id: acp::SessionId,
    file_history: Arc<parking_lot::Mutex<claurst_core::file_history::FileHistory>>,
    mut rx: mpsc::UnboundedReceiver<QueryEvent>,
) {
    // Track tool calls so ToolEnd updates carry the right title and kind.
    let mut active_tools: HashMap<String, ToolMeta> = HashMap::new();

    while let Some(event) = rx.recv().await {
        match event {
            QueryEvent::Stream(AnthropicStreamEvent::ContentBlockDelta { delta, .. }) => {
                match delta {
                    ContentDelta::TextDelta { text } => {
                        send_text_chunk(&connection, &session_id, &text, false).await;
                    }
                    ContentDelta::ThinkingDelta { thinking } => {
                        send_text_chunk(&connection, &session_id, &thinking, true).await;
                    }
                    _ => {}
                }
            }
            QueryEvent::ToolStart {
                tool_name,
                tool_id,
                input_json,
            } => {
                let kind = classify_tool_kind(&tool_name);
                let raw_input = serde_json::from_str::<serde_json::Value>(&input_json).ok();
                let title = tool_title(&tool_name, raw_input.as_ref());
                active_tools.insert(
                    tool_id.clone(),
                    ToolMeta {
                        title: title.clone(),
                        kind,
                        // Where the file recorder stood before this tool ran.
                        // Everything appended past this point belongs to it.
                        history_len: file_history.lock().entries().len(),
                        // The todo list the model is about to store. Held until
                        // the call succeeds, so a rejected write does not leave
                        // a plan on screen that nothing is following.
                        plan: raw_input.as_ref().and_then(plan_from_todos),
                    },
                );
                let mut tool_call =
                    acp::ToolCall::new(acp::ToolCallId::new(tool_id.as_str()), title)
                        .kind(kind)
                        .status(acp::ToolCallStatus::InProgress);
                if let Some(input) = raw_input {
                    tool_call = tool_call.raw_input(Some(input));
                }
                send_session_update(
                    &connection,
                    &session_id,
                    acp::SessionUpdate::ToolCall(tool_call),
                )
                .await;
            }
            QueryEvent::ToolEnd {
                tool_name: _,
                tool_id,
                result,
                is_error,
            } => {
                let status = if is_error {
                    acp::ToolCallStatus::Failed
                } else {
                    acp::ToolCallStatus::Completed
                };
                let mut content = vec![acp::ToolCallContent::Content(acp::Content::new(
                    acp::ContentBlock::Text(acp::TextContent::new(result.clone())),
                ))];
                // Whatever this tool wrote to disk, said in the protocol's own
                // terms. The tool's name is not consulted: a recorded change is
                // a change, so a new editing tool needs nothing added here.
                if let Some(meta) = active_tools.get(&tool_id) {
                    content.extend(diffs_since(&file_history, meta.history_len));
                }
                let raw_output = serde_json::from_str::<serde_json::Value>(&result)
                    .ok()
                    .or_else(|| Some(serde_json::Value::String(result.clone())));
                let mut fields = acp::ToolCallUpdateFields::new()
                    .status(status)
                    .content(content);
                if let Some(out) = raw_output {
                    fields = fields.raw_output(Some(out));
                }
                let update =
                    acp::ToolCallUpdate::new(acp::ToolCallId::new(tool_id.as_str()), fields);
                send_session_update(
                    &connection,
                    &session_id,
                    acp::SessionUpdate::ToolCallUpdate(update),
                )
                .await;
                // A stored todo list is the agent's plan, and the protocol has
                // a place for it that a client renders as a checklist.
                if let Some(plan) = active_tools
                    .get_mut(&tool_id)
                    .and_then(|meta| meta.plan.take())
                    .filter(|_| !is_error)
                {
                    send_session_update(&connection, &session_id, acp::SessionUpdate::Plan(plan))
                        .await;
                }
                active_tools.remove(&tool_id);
            }
            QueryEvent::Error(msg) => {
                send_text_chunk(
                    &connection,
                    &session_id,
                    &format!("\n[error: {}]", msg),
                    false,
                )
                .await;
            }
            _ => {}
        }
    }
}

struct ToolMeta {
    #[allow(dead_code)]
    title: String,
    #[allow(dead_code)]
    kind: acp::ToolKind,
    /// Length of the file recorder when this tool started.
    history_len: usize,
    /// The plan this call would publish once it succeeds.
    plan: Option<acp::Plan>,
}

/// Read a `TodoWrite` input as the protocol's plan.
///
/// Returns `None` for any other tool: the shape is the contract, so a tool
/// that happens to carry a `todos` array of the same shape is a plan too.
fn plan_from_todos(input: &serde_json::Value) -> Option<acp::Plan> {
    let todos = input.get("todos")?.as_array()?;
    let entries: Vec<acp::PlanEntry> = todos
        .iter()
        .filter_map(|todo| {
            let content = todo.get("content")?.as_str()?;
            let status = match todo.get("status").and_then(|s| s.as_str()) {
                Some("in_progress") => acp::PlanEntryStatus::InProgress,
                Some("completed") => acp::PlanEntryStatus::Completed,
                // The tool rejects any other name, so anything reaching here
                // is a todo yet to be started.
                _ => acp::PlanEntryStatus::Pending,
            };
            let priority = match todo.get("priority").and_then(|p| p.as_str()) {
                Some("high") => acp::PlanEntryPriority::High,
                Some("low") => acp::PlanEntryPriority::Low,
                _ => acp::PlanEntryPriority::Medium,
            };
            Some(acp::PlanEntry::new(content, priority, status))
        })
        .collect();
    Some(acp::Plan::new(entries))
}

/// Every file change recorded after `from`, as protocol diffs.
///
/// A binary change is skipped: the protocol's diff carries text, and there is
/// nothing truthful to put in it.
fn diffs_since(
    file_history: &parking_lot::Mutex<claurst_core::file_history::FileHistory>,
    from: usize,
) -> Vec<acp::ToolCallContent> {
    let history = file_history.lock();
    history
        .entries()
        .iter()
        .skip(from)
        .filter(|entry| !entry.binary)
        .filter_map(|entry| {
            let after = entry.after_text.clone()?;
            Some(acp::ToolCallContent::Diff(
                acp::Diff::new(entry.path.clone(), after).old_text(entry.before_text.clone()),
            ))
        })
        .collect()
}

async fn send_text_chunk(
    connection: &Arc<Connection>,
    session_id: &acp::SessionId,
    text: &str,
    is_thought: bool,
) {
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(text)));
    let update = if is_thought {
        acp::SessionUpdate::AgentThoughtChunk(chunk)
    } else {
        acp::SessionUpdate::AgentMessageChunk(chunk)
    };
    send_session_update(connection, session_id, update).await;
}

async fn send_session_update(
    connection: &Arc<Connection>,
    session_id: &acp::SessionId,
    update: acp::SessionUpdate,
) {
    let notif = acp::SessionNotification::new(session_id.clone(), update);
    if let Err(e) = connection.send_notification("session/update", notif).await {
        warn!(?e, "ACP: failed to send session/update");
    } else {
        debug!("ACP: sent session/update");
    }
}

/// Classify a Claurst tool name into the ACP `ToolKind` the client uses to
/// pick an icon and a verb. The permission path classifies the same names, so
/// this is the single table both sides read.
pub(crate) fn classify_tool_kind(tool_name: &str) -> acp::ToolKind {
    match tool_name {
        "Read" | "FileRead" => acp::ToolKind::Read,
        "Edit" | "FileEdit" | "Write" | "FileWrite" | "BatchEdit" | "ApplyPatch" => {
            acp::ToolKind::Edit
        }
        "Bash" | "Shell" | "Execute" => acp::ToolKind::Execute,
        "WebFetch" | "WebSearch" => acp::ToolKind::Fetch,
        "Glob" | "Grep" | "GlobTool" => acp::ToolKind::Search,
        "Delete" | "Rm" => acp::ToolKind::Delete,
        "Move" | "Rename" => acp::ToolKind::Move,
        "Think" | "Sequential" => acp::ToolKind::Think,
        _ => acp::ToolKind::Other,
    }
}

/// Compose a short, human-readable title for a tool call. Falls back to the
/// tool's bare name if no descriptive field is present.
pub(crate) fn tool_title(tool_name: &str, raw_input: Option<&serde_json::Value>) -> String {
    if let Some(input) = raw_input {
        // Prefer path-like fields for file tools.
        for key in &["file_path", "path", "filename", "url", "pattern", "command"] {
            if let Some(v) = input.get(*key).and_then(|x| x.as_str()) {
                return format!("{}: {}", tool_name, v);
            }
        }
    }
    tool_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::file_history::FileHistory;

    #[test]
    fn two_text_blocks_are_separated_by_a_blank_line() {
        let blocks = vec![
            acp::ContentBlock::Text(acp::TextContent::new("first")),
            acp::ContentBlock::Text(acp::TextContent::new("second")),
        ];
        assert_eq!(render_prompt_blocks(&blocks), "first\n\nsecond");
    }

    #[test]
    fn a_resource_link_reaches_the_model_as_its_uri() {
        // The model cannot open the link, so the uri is the only thing that
        // tells it which file the user meant.
        let blocks = vec![acp::ContentBlock::ResourceLink(acp::ResourceLink::new(
            "notes.md",
            "file:///tmp/notes.md",
        ))];
        assert_eq!(
            render_prompt_blocks(&blocks),
            "[resource link: file:///tmp/notes.md]"
        );
    }

    #[test]
    fn an_image_is_dropped_and_the_text_around_it_survives() {
        // The server does not advertise the image capability, so an editor that
        // sends one anyway must not cost the user the rest of the prompt.
        let blocks = vec![
            acp::ContentBlock::Text(acp::TextContent::new("caption")),
            acp::ContentBlock::Image(acp::ImageContent::new("base64data", "image/png")),
        ];
        assert_eq!(render_prompt_blocks(&blocks), "caption");
    }

    fn history_with(changes: &[(&str, &[u8], &[u8])]) -> parking_lot::Mutex<FileHistory> {
        let mut history = FileHistory::new();
        for (path, before, after) in changes {
            history.record_modification(std::path::PathBuf::from(path), before, after, 0, "Edit");
        }
        parking_lot::Mutex::new(history)
    }

    fn diff_of(content: &acp::ToolCallContent) -> &acp::Diff {
        match content {
            acp::ToolCallContent::Diff(diff) => diff,
            other => panic!("expected a diff, got {other:?}"),
        }
    }

    #[test]
    fn each_file_a_tool_touched_becomes_its_own_diff() {
        let history = history_with(&[
            ("/repo/a.rs", b"one", b"ONE"),
            ("/repo/b.rs", b"two", b"TWO"),
        ]);

        let diffs = diffs_since(&history, 0);
        assert_eq!(diffs.len(), 2);
        assert_eq!(
            diff_of(&diffs[0]).path,
            std::path::PathBuf::from("/repo/a.rs")
        );
        assert_eq!(diff_of(&diffs[0]).old_text.as_deref(), Some("one"));
        assert_eq!(diff_of(&diffs[0]).new_text, "ONE");
        assert_eq!(diff_of(&diffs[1]).new_text, "TWO");
    }

    #[test]
    fn only_the_changes_this_tool_made_are_reported() {
        // The recorder is per session, so a tool that starts after two earlier
        // edits must not claim them.
        let history = history_with(&[
            ("/repo/old.rs", b"before", b"after"),
            ("/repo/new.rs", b"x", b"y"),
        ]);

        let diffs = diffs_since(&history, 1);
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            diff_of(&diffs[0]).path,
            std::path::PathBuf::from("/repo/new.rs")
        );
    }

    #[test]
    fn a_todo_list_becomes_a_plan_with_its_statuses_intact() {
        let input = serde_json::json!({
            "todos": [
                { "id": "1", "content": "read the code", "status": "completed", "priority": "high" },
                { "id": "2", "content": "write the test", "status": "in_progress" },
                { "id": "3", "content": "run it", "status": "pending", "priority": "low" },
            ]
        });

        let plan = plan_from_todos(&input).expect("a todos array is a plan");
        assert_eq!(plan.entries.len(), 3);
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::Completed);
        assert_eq!(plan.entries[0].priority, acp::PlanEntryPriority::High);
        assert_eq!(plan.entries[1].status, acp::PlanEntryStatus::InProgress);
        // No priority given: the middle rung, not a guess at either end.
        assert_eq!(plan.entries[1].priority, acp::PlanEntryPriority::Medium);
        assert_eq!(plan.entries[2].content, "run it");
    }

    #[test]
    fn a_tool_input_without_todos_is_not_a_plan() {
        let input = serde_json::json!({ "file_path": "/repo/a.rs" });
        assert!(plan_from_todos(&input).is_none());
    }

    #[test]
    fn a_binary_change_is_left_out_rather_than_shown_as_text() {
        let history = history_with(&[("/repo/logo.png", &[0xff, 0xfe], &[0x00, 0x01])]);
        assert!(diffs_since(&history, 0).is_empty());
    }

    #[test]
    fn an_embedded_file_reaches_the_model_with_its_path() {
        // This is what an `@file` mention arrives as. Without the uri the model
        // is handed a wall of code and no way to say which file it edits.
        let resource =
            acp::EmbeddedResource::new(acp::EmbeddedResourceResource::TextResourceContents(
                acp::TextResourceContents::new("fn main() {}", "file:///repo/src/main.rs"),
            ));
        let blocks = vec![
            acp::ContentBlock::Text(acp::TextContent::new("explain this")),
            acp::ContentBlock::Resource(resource),
        ];

        let rendered = render_prompt_blocks(&blocks);
        assert!(rendered.contains("file:///repo/src/main.rs"), "{rendered}");
        assert!(rendered.contains("fn main() {}"), "{rendered}");
        assert!(rendered.starts_with("explain this"), "{rendered}");
    }

    #[test]
    fn no_blocks_render_as_no_text() {
        assert_eq!(render_prompt_blocks(&[]), "");
    }

    #[test]
    fn a_reading_tool_reads_as_read() {
        assert_eq!(classify_tool_kind("Read"), acp::ToolKind::Read);
        assert_eq!(classify_tool_kind("FileRead"), acp::ToolKind::Read);
    }

    #[test]
    fn an_unknown_tool_falls_back_to_other() {
        assert_eq!(classify_tool_kind("SomeMcpTool"), acp::ToolKind::Other);
    }

    #[test]
    fn a_title_without_input_is_the_bare_tool_name() {
        assert_eq!(tool_title("Bash", None), "Bash");
    }

    #[test]
    fn a_title_prefers_the_most_specific_path_field() {
        // Both keys appear on a batch edit; `file_path` names the file the call
        // acts on, while `path` may name the directory it sits in.
        let input = serde_json::json!({ "path": "/a", "file_path": "/b" });
        assert_eq!(tool_title("Edit", Some(&input)), "Edit: /b");
    }

    #[test]
    fn a_shell_call_is_titled_by_its_command() {
        let input = serde_json::json!({ "command": "ls -la" });
        assert_eq!(tool_title("Bash", Some(&input)), "Bash: ls -la");
    }

    #[test]
    fn an_input_with_no_known_field_leaves_the_name_alone() {
        let input = serde_json::json!({ "unrelated": 42 });
        assert_eq!(tool_title("CustomTool", Some(&input)), "CustomTool");
    }
}
