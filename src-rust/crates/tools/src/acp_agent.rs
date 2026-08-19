//! AcpAgent tool: delegate a task to an external agent over the Agent Client
//! Protocol.
//!
//! The tool is agent-agnostic. Whatever the user lists under `acpAgents` in
//! settings runs as a subprocess and is driven over stdio, so any conforming
//! agent works without code changes here.
//!
//! Two things make this a tool rather than a provider. It runs inside the
//! normal tool loop, so cost tracking, cancellation and transcript rendering
//! all apply unchanged; and it can reach `ToolContext`, which is what lets the
//! sub-agent's permission requests be answered by the same prompt the user
//! already sees for local tools instead of a second, separate gate.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use agent_client_protocol_schema as acp;
use async_trait::async_trait;
use claurst_core::config::AcpAgentConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{debug, warn};

/// How long the whole delegated turn may take before the child is killed.
///
/// A sub-agent that hangs would otherwise pin the tool loop indefinitely, and
/// the caller has no other way to notice.
const TURN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How many trailing stderr lines are quoted when the child dies.
const STDERR_TAIL_LINES: usize = 20;

pub struct AcpAgentTool;

#[derive(Debug, Deserialize)]
struct AcpAgentInput {
    /// Which configured agent to run.
    agent: String,
    /// The task, in the same form a user would type it.
    prompt: String,
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// One inbound JSON-RPC message, narrowed to what a client has to deal with.
enum Inbound {
    /// A response to the request we are waiting on.
    Response {
        id: Value,
        body: Result<Value, String>,
    },
    /// The agent asking us something; it expects a response with this id.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A one-way message, typically `session/update`.
    Notification { method: String, params: Value },
}

/// Classify a decoded JSON-RPC line.
///
/// Returns `None` for a line that is valid JSON but carries neither a method
/// nor a result, which is nothing this client can act on.
fn classify(message: Value) -> Option<Inbound> {
    let id = message.get("id").cloned();
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);

    match (id, method) {
        (Some(id), Some(method)) => Some(Inbound::Request {
            id,
            method,
            params: message.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(method)) => Some(Inbound::Notification {
            method,
            params: message.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => {
            let body = match message.get("error") {
                Some(error) => Err(describe_rpc_error(error)),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Inbound::Response { id, body })
        }
        (None, None) => None,
    }
}

/// Human-readable form of a JSON-RPC `error` object.
fn describe_rpc_error(error: &Value) -> String {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    match error.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{} (code {})", message, code),
        None => message.to_string(),
    }
}

/// The text carried by one `session/update`, if it carries any.
///
/// Only the agent's own message chunks are collected. Thought chunks are the
/// agent's private reasoning and tool-call updates describe work the caller
/// did not ask to see, so folding either into the answer would bury it.
fn update_text(params: &Value) -> Option<String> {
    let notification: acp::SessionNotification = serde_json::from_value(params.clone()).ok()?;
    match notification.update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            acp::ContentBlock::Text(text) => Some(text.text),
            _ => None,
        },
        _ => None,
    }
}

/// Pick the option to answer a permission request with.
///
/// A one-shot option is preferred over a remembered one in both directions:
/// the decision was made for this call, and silently upgrading it to "always"
/// would grant more than the user agreed to.
fn choose_option(options: &[acp::PermissionOption], allow: bool) -> Option<&acp::PermissionOption> {
    let ranked: [acp::PermissionOptionKind; 2] = if allow {
        [
            acp::PermissionOptionKind::AllowOnce,
            acp::PermissionOptionKind::AllowAlways,
        ]
    } else {
        [
            acp::PermissionOptionKind::RejectOnce,
            acp::PermissionOptionKind::RejectAlways,
        ]
    };
    ranked
        .iter()
        .find_map(|kind| options.iter().find(|option| option.kind == *kind))
}

/// What the permission prompt says the sub-agent wants to do.
fn describe_tool_call(request: &acp::RequestPermissionRequest) -> String {
    request
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_else(|| format!("tool call {}", request.tool_call.tool_call_id.0))
}

// ---------------------------------------------------------------------------
// Session driver
// ---------------------------------------------------------------------------

/// Drives one delegated turn over an already-spawned child process.
///
/// Requests go out one at a time and the reader loop runs until that one
/// request is answered, handling anything the agent sends in the meantime.
/// That is enough for a client that never has two calls in flight, and it
/// avoids a pending-request map that would have nothing to hold.
struct Session<W, R> {
    writer: W,
    reader: BufReader<R>,
    next_id: i64,
    /// The id `session/new` returned, once it has. Needed to cancel the turn:
    /// `session/cancel` names the session, so before there is one there is
    /// nothing to cancel.
    session_id: Option<String>,
    /// The agent's message text, in arrival order.
    transcript: String,
    /// Permission requests answered, for the tool's own summary line.
    approvals: usize,
    denials: usize,
}

impl<W: AsyncWrite + Unpin, R: AsyncRead + Unpin> Session<W, R> {
    fn new(writer: W, reader: R) -> Self {
        Self {
            writer,
            reader: BufReader::new(reader),
            next_id: 1,
            session_id: None,
            transcript: String::new(),
            approvals: 0,
            denials: 0,
        }
    }

    async fn write_line(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(message)
            .map_err(|e| format!("could not encode a message for the agent: {e}"))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|e| format!("could not write to the agent: {e}"))?;
        self.writer
            .flush()
            .await
            .map_err(|e| format!("could not flush to the agent: {e}"))
    }

    /// Send a request and pump the reader until its response arrives.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        ctx: &ToolContext,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;

        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("could not read from the agent: {e}"))?;
            if read == 0 {
                return Err(format!(
                    "the agent closed its output while waiting for {method}"
                ));
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(message) = serde_json::from_str::<Value>(line) else {
                warn!(line, "AcpAgent: agent sent a line that is not JSON");
                continue;
            };
            let Some(inbound) = classify(message) else {
                continue;
            };
            match inbound {
                Inbound::Response { id: got, body } => {
                    if got.as_i64() == Some(id) {
                        return body;
                    }
                    debug!(?got, want = id, "AcpAgent: response for another request");
                }
                Inbound::Notification { method, params } => {
                    if method == "session/update" {
                        if let Some(text) = update_text(&params) {
                            self.transcript.push_str(&text);
                        }
                    }
                }
                Inbound::Request { id, method, params } => {
                    self.answer_request(id, &method, params, ctx).await?;
                }
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    /// Answer a request the agent made of us.
    ///
    /// Only `session/request_permission` is served. Everything else is refused
    /// with "method not found" rather than ignored: an unanswered request would
    /// leave the agent waiting forever, and answering it with a lie about a
    /// capability we do not have is worse than saying so.
    async fn answer_request(
        &mut self,
        id: Value,
        method: &str,
        params: Value,
        ctx: &ToolContext,
    ) -> Result<(), String> {
        if method != "session/request_permission" {
            return self
                .write_line(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("{method} is not supported") },
                }))
                .await;
        }

        let request: acp::RequestPermissionRequest = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(e) => {
                return self
                    .write_line(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32602,
                            "message": format!("malformed permission request: {e}"),
                        },
                    }))
                    .await;
            }
        };

        let outcome = self.decide(&request, &params, ctx);
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "outcome": outcome },
        }))
        .await
    }

    /// Run one sub-agent permission request through this session's own
    /// permission system and translate the answer back into an ACP outcome.
    fn decide(
        &mut self,
        request: &acp::RequestPermissionRequest,
        raw: &Value,
        ctx: &ToolContext,
    ) -> Value {
        let title = describe_tool_call(request);
        let allowed = ctx
            .check_permission_with_details(
                "AcpAgent",
                &format!("external agent wants to run: {title}"),
                &format!(
                    "The delegated ACP agent is asking to perform \"{title}\". \
                     Approving runs it inside the agent, not here."
                ),
                false,
            )
            .is_ok();

        match choose_option(&request.options, allowed) {
            Some(option) => {
                if allowed {
                    self.approvals += 1;
                } else {
                    self.denials += 1;
                }
                json!({ "outcome": "selected", "optionId": option.option_id })
            }
            None => {
                // The agent offered nothing that matches the decision. Cancelling
                // is the only honest answer: picking an unrelated option would
                // apply a choice the user never made.
                warn!(
                    allowed,
                    raw = %raw,
                    "AcpAgent: no permission option matches the decision, cancelling"
                );
                self.denials += 1;
                json!({ "outcome": "cancelled" })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

/// Read the last few lines of the child's stderr, for an error message that
/// says something more useful than "the agent exited".
async fn stderr_tail(stderr: Option<tokio::process::ChildStderr>) -> String {
    let Some(stderr) = stderr else {
        return String::new();
    };
    let mut lines = Vec::new();
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        lines.push(line);
        if lines.len() > STDERR_TAIL_LINES {
            lines.remove(0);
        }
    }
    lines.join("\n")
}

#[async_trait]
impl Tool for AcpAgentTool {
    fn name(&self) -> &str {
        "AcpAgent"
    }

    fn description(&self) -> &str {
        "Delegate a task to an external agent that speaks the Agent Client \
         Protocol. Names come from the `acpAgents` block in settings. The \
         sub-agent runs in this session's working directory and every action it \
         wants to take is approved through the same permission prompt as a \
         local tool."
    }

    /// The sub-agent runs an arbitrary executable and can do anything that
    /// executable can, so this sits at the same level as running a command.
    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Execute
    }

    /// The tool prompts per sub-agent action inside `execute`. A second gate on
    /// the call itself would ask twice for the same work.
    fn self_gates(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Name of a configured ACP agent, as it appears under `acpAgents` in settings."
                },
                "prompt": {
                    "type": "string",
                    "description": "The task to delegate, written the way you would write it to a person."
                }
            },
            "required": ["agent", "prompt"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: AcpAgentInput = match serde_json::from_value(input) {
            Ok(params) => params,
            Err(e) => return ToolResult::error(format!("Invalid input: {e}")),
        };

        let Some(config) = ctx.config.acp_agents.get(&params.agent) else {
            let mut names: Vec<&str> = ctx.config.acp_agents.keys().map(String::as_str).collect();
            names.sort_unstable();
            return ToolResult::error(if names.is_empty() {
                "No ACP agents are configured. Add one under `acpAgents` in settings.json."
                    .to_string()
            } else {
                format!(
                    "Unknown ACP agent {:?}. Configured agents: {}.",
                    params.agent,
                    names.join(", ")
                )
            });
        };

        match run_agent(config, &params.prompt, ctx).await {
            Ok(output) => ToolResult::success(output),
            Err(e) => ToolResult::error(e),
        }
    }
}

/// Spawn the agent, run one turn, and tear the process down on every exit path.
async fn run_agent(
    config: &AcpAgentConfig,
    prompt: &str,
    ctx: &ToolContext,
) -> Result<String, String> {
    // Launching the agent is itself arbitrary code execution, and the
    // sub-agent may never ask for anything afterwards. Without this gate a
    // `self_gates` tool could run a binary with no prompt at all.
    let command_line = std::iter::once(config.command.as_str())
        .chain(config.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    ctx.check_permission_with_details(
        "AcpAgent",
        &format!("run external agent: {command_line}"),
        &format!(
            "Starts `{command_line}` in {} and hands it this task. \
             It will ask again before each action it takes.",
            ctx.working_dir.display()
        ),
        false,
    )
    .map_err(|e| e.to_string())?;

    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .current_dir(&ctx.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in &config.env {
        command.env(key, value);
    }
    claurst_core::process_tree::spawn_in_own_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", config.command))?;

    // `kill_on_drop` reaps the agent itself; whatever the agent started is
    // reachable only through its process tree.
    let _tree_guard = claurst_core::process_tree::ProcessTreeKillGuard::new(child.id());

    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill().await;
        return Err(format!(
            "{:?} did not provide the stdio pipes the protocol needs",
            config.command
        ));
    };
    let stderr = child.stderr.take();

    let mut session = Session::new(stdin, stdout);
    let turn = drive_turn(&mut session, prompt, ctx);

    let outcome = tokio::select! {
        biased;
        () = ctx.cancel_token.cancelled() => Err("cancelled".to_string()),
        result = tokio::time::timeout(TURN_TIMEOUT, turn) => match result {
            Ok(result) => result,
            Err(_) => Err(format!(
                "the agent did not finish within {}s",
                TURN_TIMEOUT.as_secs()
            )),
        },
    };

    // Every exit path lands here, so a cancelled or timed-out agent cannot
    // outlive the call that started it.
    if outcome.is_err() {
        // A cancel that names no session is not a valid notification, so it is
        // only worth sending once `session/new` has answered.
        if let Some(session_id) = session.session_id.clone() {
            let _ = session
                .notify("session/cancel", json!({ "sessionId": session_id }))
                .await;
        }
    }
    let _ = child.kill().await;

    match outcome {
        Ok(summary) => Ok(summary),
        Err(reason) => {
            let tail = stderr_tail(stderr).await;
            Err(if tail.is_empty() {
                format!("ACP agent failed: {reason}")
            } else {
                format!("ACP agent failed: {reason}\n\nAgent stderr:\n{tail}")
            })
        }
    }
}

/// initialize -> session/new -> session/prompt, then report what came back.
async fn drive_turn<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    session: &mut Session<W, R>,
    prompt: &str,
    ctx: &ToolContext,
) -> Result<String, String> {
    session
        .call(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": { "name": "claurst", "version": env!("CARGO_PKG_VERSION") },
            }),
            ctx,
        )
        .await?;

    let new_session = session
        .call(
            "session/new",
            json!({ "cwd": ctx.working_dir, "mcpServers": [] }),
            ctx,
        )
        .await?;
    let session_id = new_session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "the agent's session/new response carried no sessionId".to_string())?
        .to_string();
    session.session_id = Some(session_id.clone());

    let response = session
        .call(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }],
            }),
            ctx,
        )
        .await?;

    let stop_reason = response
        .get("stopReason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let mut out = String::new();
    if session.transcript.trim().is_empty() {
        out.push_str("The agent produced no message text.");
    } else {
        out.push_str(session.transcript.trim());
    }
    out.push_str(&format!("\n\n(stop reason: {stop_reason}"));
    if session.approvals > 0 || session.denials > 0 {
        out.push_str(&format!(
            "; {} permission request(s) approved, {} denied",
            session.approvals, session.denials
        ));
    }
    out.push(')');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str, kind: acp::PermissionOptionKind) -> acp::PermissionOption {
        acp::PermissionOption::new(acp::PermissionOptionId::new(id), id.to_string(), kind)
    }

    // --- Message classification ---------------------------------------------

    #[test]
    fn a_message_with_id_and_method_is_a_request() {
        let inbound = classify(json!({"id": 7, "method": "session/request_permission"}))
            .expect("a request is actionable");
        match inbound {
            Inbound::Request { id, method, .. } => {
                assert_eq!(id, json!(7));
                assert_eq!(method, "session/request_permission");
            }
            _ => panic!("expected a request"),
        }
    }

    #[test]
    fn a_method_without_an_id_is_a_notification() {
        let inbound =
            classify(json!({"method": "session/update", "params": {}})).expect("actionable");
        assert!(matches!(inbound, Inbound::Notification { .. }));
    }

    #[test]
    fn an_error_response_carries_its_message_rather_than_reading_as_success() {
        let inbound = classify(json!({
            "id": 1,
            "error": { "code": -32601, "message": "no such method" }
        }))
        .expect("actionable");
        match inbound {
            Inbound::Response { body, .. } => {
                let err = body.expect_err("an error object is not a result");
                assert!(err.contains("no such method"), "{err:?}");
                assert!(err.contains("-32601"), "{err:?}");
            }
            _ => panic!("expected a response"),
        }
    }

    #[test]
    fn a_line_with_neither_a_method_nor_an_id_is_not_actionable() {
        assert!(classify(json!({"jsonrpc": "2.0"})).is_none());
    }

    // --- Update text ---------------------------------------------------------

    #[test]
    fn agent_message_chunks_are_collected_and_other_updates_are_not() {
        let text = update_text(&json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "hello" }
            }
        }));
        assert_eq!(text.as_deref(), Some("hello"));

        // Private reasoning must not be folded into the answer.
        let thought = update_text(&json!({
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": "thinking" }
            }
        }));
        assert_eq!(thought, None);
    }

    #[test]
    fn an_unparseable_update_yields_no_text_rather_than_panicking() {
        assert_eq!(update_text(&json!({"nonsense": true})), None);
    }

    // --- Permission mapping --------------------------------------------------

    #[test]
    fn a_one_shot_option_is_preferred_over_a_remembered_one() {
        let options = vec![
            option("allow_always", acp::PermissionOptionKind::AllowAlways),
            option("allow_once", acp::PermissionOptionKind::AllowOnce),
            option("reject_always", acp::PermissionOptionKind::RejectAlways),
            option("reject_once", acp::PermissionOptionKind::RejectOnce),
        ];
        assert_eq!(
            choose_option(&options, true).map(|o| o.option_id.0.as_ref()),
            Some("allow_once")
        );
        assert_eq!(
            choose_option(&options, false).map(|o| o.option_id.0.as_ref()),
            Some("reject_once")
        );
    }

    #[test]
    fn a_remembered_option_is_used_when_it_is_the_only_one() {
        let options = vec![
            option("allow_always", acp::PermissionOptionKind::AllowAlways),
            option("reject_always", acp::PermissionOptionKind::RejectAlways),
        ];
        assert_eq!(
            choose_option(&options, true).map(|o| o.option_id.0.as_ref()),
            Some("allow_always")
        );
        assert_eq!(
            choose_option(&options, false).map(|o| o.option_id.0.as_ref()),
            Some("reject_always")
        );
    }

    #[test]
    fn a_decision_with_no_matching_option_picks_nothing() {
        // Only allow options offered, but the user denied: there is no honest
        // option to select, so the caller must cancel instead.
        let options = vec![option("allow_once", acp::PermissionOptionKind::AllowOnce)];
        assert!(choose_option(&options, false).is_none());
        assert!(choose_option(&[], true).is_none());
    }

    #[test]
    fn a_permission_request_without_a_title_still_names_the_call() {
        let request = acp::RequestPermissionRequest::new(
            acp::SessionId::new("s1"),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new("call-9"),
                acp::ToolCallUpdateFields::new(),
            ),
            vec![],
        );
        assert!(describe_tool_call(&request).contains("call-9"));
    }

    #[test]
    fn a_titled_permission_request_uses_the_title() {
        let request = acp::RequestPermissionRequest::new(
            acp::SessionId::new("s1"),
            acp::ToolCallUpdate::new(
                acp::ToolCallId::new("call-9"),
                acp::ToolCallUpdateFields::new().title("Write src/main.rs".to_string()),
            ),
            vec![],
        );
        assert_eq!(describe_tool_call(&request), "Write src/main.rs");
    }
}

#[cfg(test)]
mod session_tests {
    //! End-to-end coverage against a scripted ACP peer over in-memory pipes.
    //! No agent is launched: the point is the protocol exchange and the
    //! permission bridge, both of which are ours.
    use super::*;
    use crate::test_support::allow_all_context;
    use claurst_core::permissions::{PermissionDecision, PermissionHandler, PermissionRequest};
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct DenyAllHandler;

    impl PermissionHandler for DenyAllHandler {
        fn check_permission(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Deny
        }
        fn request_permission(&self, _request: &PermissionRequest) -> PermissionDecision {
            PermissionDecision::Deny
        }
    }

    fn context(allow: bool) -> ToolContext {
        let mut ctx = allow_all_context(std::env::temp_dir());
        if !allow {
            ctx.permission_handler = Arc::new(DenyAllHandler);
        }
        ctx
    }

    /// Run `drive_turn` against a peer that answers each request with the next
    /// scripted reply, optionally sending extra lines first.
    ///
    /// Returns the turn's result plus every line the client sent, so a test can
    /// assert on the exchange rather than only on the final string.
    async fn exchange(
        script: Vec<(Vec<Value>, Value)>,
        ctx: &ToolContext,
    ) -> (Result<String, String>, Vec<Value>) {
        let (client_side, peer_side) = tokio::io::duplex(64 * 1024);
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_side);
        let (client_reader, client_writer) = tokio::io::split(client_side);

        let peer = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut lines = BufReader::new(peer_reader).lines();
            let mut script = script.into_iter();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message): Result<Value, _> = serde_json::from_str(&line) else {
                    continue;
                };
                seen.push(message.clone());
                // Notifications and our own responses need no reply.
                let Some(id) = message.get("id") else {
                    continue;
                };
                if message.get("method").is_none() {
                    continue;
                }
                let Some((extra, result)) = script.next() else {
                    break;
                };
                for line in extra {
                    let mut bytes = serde_json::to_vec(&line).expect("encode");
                    bytes.push(b'\n');
                    if peer_writer.write_all(&bytes).await.is_err() {
                        return seen;
                    }
                }
                let mut bytes = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }))
                .expect("encode");
                bytes.push(b'\n');
                if peer_writer.write_all(&bytes).await.is_err() {
                    return seen;
                }
            }
            seen
        });

        let mut session = Session::new(client_writer, client_reader);
        let result = drive_turn(&mut session, "do the thing", ctx).await;
        drop(session);
        let seen = peer.await.expect("peer task");
        (result, seen)
    }

    fn happy_script() -> Vec<(Vec<Value>, Value)> {
        vec![
            (vec![], json!({ "protocolVersion": 1 })),
            (vec![], json!({ "sessionId": "sess-1" })),
            (vec![], json!({ "stopReason": "end_turn" })),
        ]
    }

    #[tokio::test]
    async fn the_handshake_runs_in_order_and_carries_the_working_directory() {
        let ctx = context(true);
        let (result, seen) = exchange(happy_script(), &ctx).await;
        result.expect("the turn completes");

        let methods: Vec<&str> = seen
            .iter()
            .filter_map(|m| m.get("method").and_then(Value::as_str))
            .collect();
        assert_eq!(methods, vec!["initialize", "session/new", "session/prompt"]);

        let new_session = &seen[1];
        assert_eq!(
            new_session["params"]["cwd"],
            json!(ctx.working_dir.to_string_lossy())
        );
        // The prompt must reach the agent, and the session id must be echoed
        // back from session/new rather than invented.
        assert_eq!(seen[2]["params"]["sessionId"], json!("sess-1"));
        assert_eq!(
            seen[2]["params"]["prompt"][0]["text"],
            json!("do the thing")
        );
    }

    #[tokio::test]
    async fn agent_message_chunks_are_collected_into_the_answer() {
        let chunk = |text: &str| {
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-1",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": text }
                    }
                }
            })
        };
        let mut script = happy_script();
        script[2].0 = vec![
            chunk("Refactored "),
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-1",
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": { "type": "text", "text": "hmm" }
                    }
                }
            }),
            chunk("the parser."),
        ];

        let (result, _) = exchange(script, &context(true)).await;
        let output = result.expect("the turn completes");
        assert!(output.starts_with("Refactored the parser."), "{output:?}");
        assert!(!output.contains("hmm"), "thoughts stay private: {output:?}");
        assert!(output.contains("stop reason: end_turn"), "{output:?}");
    }

    fn permission_request() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 900,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess-1",
                "toolCall": { "toolCallId": "call-1", "title": "Write src/main.rs" },
                "options": [
                    { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                ]
            }
        })
    }

    /// The client's answer to the peer's permission request.
    fn permission_answer(seen: &[Value]) -> Value {
        seen.iter()
            .find(|m| m.get("id") == Some(&json!(900)))
            .cloned()
            .expect("the client answered the permission request")
    }

    #[tokio::test]
    async fn an_approved_request_selects_the_allow_option() {
        let mut script = happy_script();
        script[2].0 = vec![permission_request()];
        let (result, seen) = exchange(script, &context(true)).await;
        let output = result.expect("the turn completes");

        assert_eq!(
            permission_answer(&seen)["result"]["outcome"],
            json!({ "outcome": "selected", "optionId": "yes" })
        );
        assert!(
            output.contains("1 permission request(s) approved, 0 denied"),
            "{output:?}"
        );
    }

    #[tokio::test]
    async fn a_denied_request_selects_the_reject_option() {
        let mut script = happy_script();
        script[2].0 = vec![permission_request()];
        let (result, seen) = exchange(script, &context(false)).await;
        let output = result.expect("the turn completes");

        assert_eq!(
            permission_answer(&seen)["result"]["outcome"],
            json!({ "outcome": "selected", "optionId": "no" })
        );
        assert!(
            output.contains("0 permission request(s) approved, 1 denied"),
            "{output:?}"
        );
    }

    #[tokio::test]
    async fn a_request_with_no_usable_option_is_cancelled_rather_than_guessed() {
        let mut request = permission_request();
        // Only an allow option offered, and this session denies.
        request["params"]["options"] =
            json!([{ "optionId": "yes", "name": "Allow", "kind": "allow_once" }]);
        let mut script = happy_script();
        script[2].0 = vec![request];

        let (result, seen) = exchange(script, &context(false)).await;
        result.expect("the turn completes");
        assert_eq!(
            permission_answer(&seen)["result"]["outcome"],
            json!({ "outcome": "cancelled" })
        );
    }

    #[tokio::test]
    async fn an_unsupported_request_is_refused_rather_than_left_hanging() {
        let mut script = happy_script();
        script[2].0 = vec![json!({
            "jsonrpc": "2.0",
            "id": 901,
            "method": "fs/read_text_file",
            "params": { "path": "/etc/passwd" }
        })];

        let (result, seen) = exchange(script, &context(true)).await;
        result.expect("an unsupported request does not fail the turn");

        let answer = seen
            .iter()
            .find(|m| m.get("id") == Some(&json!(901)))
            .expect("the client answered");
        assert_eq!(answer["error"]["code"], json!(-32601));
    }

    #[tokio::test]
    async fn a_session_new_response_without_an_id_fails_the_turn() {
        let script = vec![
            (vec![], json!({ "protocolVersion": 1 })),
            (vec![], json!({ "modes": null })),
        ];
        let (result, _) = exchange(script, &context(true)).await;
        let err = result.expect_err("no sessionId means no session");
        assert!(err.contains("sessionId"), "{err:?}");
    }

    #[tokio::test]
    async fn launching_the_agent_is_gated_even_when_it_never_asks_for_anything() {
        // `self_gates` turns off the central backstop, so this prompt is the
        // only thing standing between the model and an arbitrary binary.
        let config = AcpAgentConfig {
            command: "definitely-not-a-real-agent-binary".to_string(),
            args: vec!["--force".to_string(), "acp".to_string()],
            env: Default::default(),
        };
        let err = run_agent(&config, "do the thing", &context(false))
            .await
            .expect_err("a denied launch must not spawn anything");
        assert!(
            err.to_lowercase().contains("permission"),
            "the failure must be the gate, not a missing binary: {err:?}"
        );
        assert!(
            !err.contains("could not start"),
            "the process must not have been spawned: {err:?}"
        );
    }

    #[tokio::test]
    async fn an_agent_that_cannot_be_started_says_so() {
        let config = AcpAgentConfig {
            command: "definitely-not-a-real-agent-binary".to_string(),
            args: vec![],
            env: Default::default(),
        };
        let err = run_agent(&config, "do the thing", &context(true))
            .await
            .expect_err("the binary does not exist");
        assert!(err.contains("could not start"), "{err:?}");
    }

    #[tokio::test]
    async fn the_session_id_is_remembered_so_the_turn_can_be_cancelled() {
        // `session/cancel` names the session; without this the teardown path
        // would send a notification the agent must reject.
        let (client_side, peer_side) = tokio::io::duplex(64 * 1024);
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_side);
        let (client_reader, client_writer) = tokio::io::split(client_side);

        tokio::spawn(async move {
            let mut lines = BufReader::new(peer_reader).lines();
            let mut replies = happy_script().into_iter();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message): Result<Value, _> = serde_json::from_str(&line) else {
                    continue;
                };
                let Some(id) = message.get("id") else {
                    continue;
                };
                let Some((_, result)) = replies.next() else {
                    break;
                };
                let mut bytes =
                    serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
                        .expect("encode");
                bytes.push(b'\n');
                if peer_writer.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });

        let mut session = Session::new(client_writer, client_reader);
        drive_turn(&mut session, "do the thing", &context(true))
            .await
            .expect("the turn completes");
        assert_eq!(session.session_id.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn a_closed_pipe_reports_which_call_was_waiting() {
        // An empty script makes the peer break after the first request.
        let (result, _) = exchange(vec![], &context(true)).await;
        let err = result.expect_err("the peer went away");
        assert!(err.contains("initialize"), "{err:?}");
    }
}
