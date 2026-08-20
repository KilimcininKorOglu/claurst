//! Bridge between MikMik's synchronous `PermissionHandler` trait and the
//! asynchronous `session/request_permission` JSON-RPC round-trip used by ACP.
//!
//! The handler itself simply returns `Ask { reason }` for every permission
//! check. That causes `ToolContext::request_permission_inner` to enqueue the
//! request onto a shared `PendingPermissionStore` and block on a oneshot.
//! A background task — spawned by `prompt::handle_prompt` — drains the queue,
//! converts each pending request into a `session/request_permission` call to
//! the connected client, and forwards the client's decision back through the
//! oneshot to unblock the tool.

use std::sync::Arc;

use agent_client_protocol_schema as acp;
use mikmik_core::permissions::{PermissionDecision, PermissionRequest};
use mikmik_core::PermissionHandler;
use mikmik_tools::{PendingPermissionRequest, PendingPermissionStore};
use tracing::{debug, warn};

use crate::connection::Connection;

/// Permission handler that defers every decision to the ACP client.
pub struct AcpPermissionHandler;

impl PermissionHandler for AcpPermissionHandler {
    fn check_permission(&self, _request: &PermissionRequest) -> PermissionDecision {
        // Defer everything to interactive resolution.
        PermissionDecision::Ask {
            reason: String::new(),
        }
    }

    fn request_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        let mut reason = format!("Tool '{}' requires approval", request.tool_name);
        if let Some(detail) = &request.details {
            reason.push_str(": ");
            reason.push_str(detail);
        }
        PermissionDecision::Ask { reason }
    }
}

/// Drain a single pending permission request, route it through the
/// connection as `session/request_permission`, and fire the oneshot with
/// the resulting decision.
pub async fn forward_pending(
    connection: Arc<Connection>,
    session_id: acp::SessionId,
    pending: PendingPermissionRequest,
) {
    let PendingPermissionRequest {
        tool_use_id,
        request,
        reason,
        decision_tx,
    } = pending;

    let Some(decision_tx) = decision_tx else {
        warn!(
            tool_use_id,
            "ACP permission: pending request had no decision_tx"
        );
        return;
    };

    let title = if reason.is_empty() {
        format!("Approve {}", request.tool_name)
    } else {
        reason.clone()
    };

    let mut fields = acp::ToolCallUpdateFields::new()
        .kind(Some(infer_tool_kind(&request)))
        .status(Some(acp::ToolCallStatus::Pending))
        .title(Some(title))
        .content(Some(preview(&request).await));
    let locations = locations_of(&request);
    if !locations.is_empty() {
        fields = fields.locations(Some(locations));
    }
    if let Some(input) = &request.input {
        fields = fields.raw_input(Some(input.clone()));
    }
    let tool_call = acp::ToolCallUpdate::new(acp::ToolCallId::new(tool_use_id.as_str()), fields);

    let options = vec![
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("allow_once"),
            "Allow once",
            acp::PermissionOptionKind::AllowOnce,
        ),
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("allow_always"),
            "Allow always",
            acp::PermissionOptionKind::AllowAlways,
        ),
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("reject_once"),
            "Reject",
            acp::PermissionOptionKind::RejectOnce,
        ),
        acp::PermissionOption::new(
            acp::PermissionOptionId::new("reject_always"),
            "Reject always",
            acp::PermissionOptionKind::RejectAlways,
        ),
    ];

    let request_params = acp::RequestPermissionRequest::new(session_id, tool_call, options);

    debug!(tool = %request.tool_name, "ACP permission: requesting from client");
    let result = connection
        .send_request::<_, acp::RequestPermissionResponse>(
            "session/request_permission",
            request_params,
        )
        .await;

    let decision = match result {
        Ok(Ok(response)) => match response.outcome {
            acp::RequestPermissionOutcome::Selected(sel) => decision_for(sel.option_id.0.as_ref()),
            acp::RequestPermissionOutcome::Cancelled => PermissionDecision::Deny,
            _ => PermissionDecision::Deny,
        },
        Ok(Err(err)) => {
            warn!(?err, "ACP permission: client returned error, denying");
            PermissionDecision::Deny
        }
        Err(err) => {
            warn!(?err, "ACP permission: send_request failed, denying");
            PermissionDecision::Deny
        }
    };

    let _ = decision_tx.send(decision);
}

/// What the option the user picked means here.
///
/// An id this does not know denies: an approval must be something the client
/// was actually offered, never a default.
fn decision_for(option_id: &str) -> PermissionDecision {
    match option_id {
        "allow_once" => PermissionDecision::Allow,
        "allow_always" => PermissionDecision::AllowPermanently,
        "reject_always" => PermissionDecision::DenyPermanently,
        _ => PermissionDecision::Deny,
    }
}

/// The file this request is about, when it names one.
///
/// A client draws its "jump to file" affordance from this, so a request that
/// names no path gets an empty list rather than an invented one.
fn locations_of(request: &PermissionRequest) -> Vec<acp::ToolCallLocation> {
    match &request.path {
        Some(path) => vec![acp::ToolCallLocation::new(std::path::PathBuf::from(path))],
        None => Vec::new(),
    }
}

/// What the client should show alongside "approve this?".
///
/// A prompt naming only the tool asks the user to approve something they
/// cannot see. A `Write` is shown as a real diff, because both sides of it are
/// known: the file on disk and the text about to replace it. Everything else
/// is described in words built from the call's own arguments — an `Edit`'s new
/// file contents do not exist yet, and computing them here would be a second
/// implementation of the edit that could disagree with the tool's own.
async fn preview(request: &PermissionRequest) -> Vec<acp::ToolCallContent> {
    if let Some(diff) = write_diff(request).await {
        return vec![acp::ToolCallContent::Diff(diff)];
    }
    let described = describe(request);
    if described.is_empty() {
        return Vec::new();
    }
    vec![acp::ToolCallContent::Content(acp::Content::new(
        acp::ContentBlock::Text(acp::TextContent::new(described)),
    ))]
}

/// A whole-file diff for a call that replaces a file outright.
///
/// `None` for every other tool, and for a write whose target cannot be read as
/// text: the protocol's diff carries text, and there is nothing truthful to
/// put in it otherwise. A file that does not exist yet diffs against nothing,
/// which is what creating it is.
async fn write_diff(request: &PermissionRequest) -> Option<acp::Diff> {
    if !matches!(request.tool_name.as_str(), "Write" | "FileWrite") {
        return None;
    }
    let input = request.input.as_ref()?;
    let new_text = input.get("content")?.as_str()?.to_string();
    let path = request.path.clone()?;
    let old_text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Unreadable is not the same as absent, and claiming the file was
        // empty would show the user a diff that adds a file it replaces.
        Err(_) => return None,
    };
    let mut diff = acp::Diff::new(std::path::PathBuf::from(path), new_text);
    if let Some(old) = old_text {
        diff = diff.old_text(Some(old));
    }
    Some(diff)
}

/// Say in words what the call would do, from its own arguments.
///
/// Falls back to whatever the tool already explained (`context_description`,
/// then `details`) when the arguments are not in a shape this knows.
fn describe(request: &PermissionRequest) -> String {
    let by_input = request
        .input
        .as_ref()
        .and_then(|input| match request.tool_name.as_str() {
            "Edit" | "FileEdit" => {
                let old = input.get("old_string")?.as_str()?;
                let new = input.get("new_string")?.as_str()?;
                let all = input
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let scope = if all { "every occurrence of" } else { "" };
                Some(format!(
                    "Replace {scope}\n\n```\n{}\n```\n\nwith\n\n```\n{}\n```",
                    truncate(old),
                    truncate(new)
                ))
            }
            "Bash" | "PtyBash" => Some(format!(
                "Run\n\n```\n{}\n```",
                truncate(input.get("command")?.as_str()?)
            )),
            "WebFetch" => Some(format!("Fetch {}", input.get("url")?.as_str()?)),
            _ => None,
        });

    by_input
        .or_else(|| request.context_description.clone())
        .or_else(|| request.details.clone())
        .unwrap_or_default()
}

/// Keep a preview readable. A permission prompt is a dialog, not a file
/// viewer, and a client that wants the whole thing has `raw_input`.
fn truncate(text: &str) -> String {
    const LIMIT: usize = 2000;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let kept: String = text.chars().take(LIMIT).collect();
    format!("{kept}\n… (truncated)")
}

/// Classify a permission request into an ACP `ToolKind` for client UI hints.
/// A caller that marks the call read-only outranks the name, so a tool used
/// in a read-only mode is not announced as an edit.
fn infer_tool_kind(request: &PermissionRequest) -> acp::ToolKind {
    if request.is_read_only {
        return acp::ToolKind::Read;
    }
    crate::prompt::classify_tool_kind(&request.tool_name)
}

/// Spawn a task that watches the shared `PendingPermissionStore` and
/// forwards each enqueued request through the ACP connection. The task
/// exits when `cancel` is fired or the connection drops.
pub fn spawn_drainer(
    connection: Arc<Connection>,
    session_id: acp::SessionId,
    store: Arc<parking_lot::Mutex<PendingPermissionStore>>,
    cancel: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(50));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {}
            }
            let popped: Vec<PendingPermissionRequest> = {
                let mut guard = store.lock();
                guard.queue.drain(..).collect()
            };
            for pending in popped {
                let conn = connection.clone();
                let sid = session_id.clone();
                tokio::spawn(async move {
                    forward_pending(conn, sid, pending).await;
                });
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tool_name: &str, is_read_only: bool) -> PermissionRequest {
        PermissionRequest {
            tool_name: tool_name.to_string(),
            description: String::new(),
            details: None,
            is_read_only,
            path: None,
            working_dir: None,
            allowed_roots: Vec::new(),
            context_description: None,
            input: None,
        }
    }

    #[test]
    fn a_read_only_call_reads_as_read_whatever_the_tool_is_called() {
        // The editor draws its icon from this kind, so a read-only Bash call
        // must not arrive looking like it is about to run something.
        assert_eq!(infer_tool_kind(&request("Bash", true)), acp::ToolKind::Read);
        assert_eq!(infer_tool_kind(&request("Rm", true)), acp::ToolKind::Read);
    }

    #[test]
    fn a_writing_tool_reads_as_edit() {
        for name in [
            "Edit",
            "FileEdit",
            "Write",
            "FileWrite",
            "BatchEdit",
            "ApplyPatch",
        ] {
            assert_eq!(
                infer_tool_kind(&request(name, false)),
                acp::ToolKind::Edit,
                "tool: {name}"
            );
        }
    }

    #[test]
    fn a_destructive_tool_is_not_lumped_in_with_an_edit() {
        assert_eq!(
            infer_tool_kind(&request("Rm", false)),
            acp::ToolKind::Delete
        );
        assert_eq!(
            infer_tool_kind(&request("Bash", false)),
            acp::ToolKind::Execute
        );
    }

    #[test]
    fn a_reading_tool_reads_as_read_even_when_the_flag_is_unset() {
        // `is_read_only` is set by the tool that raised the request, and a
        // reading tool that leaves it false still reads a file. The session
        // update path already classifies it as Read; both must agree.
        assert_eq!(
            infer_tool_kind(&request("Read", false)),
            acp::ToolKind::Read
        );
        assert_eq!(
            infer_tool_kind(&request("FileRead", false)),
            acp::ToolKind::Read
        );
    }

    #[test]
    fn an_mcp_tool_falls_back_to_other() {
        assert_eq!(
            infer_tool_kind(&request("SomeCustomMcpTool", false)),
            acp::ToolKind::Other
        );
    }

    #[test]
    fn a_check_defers_without_a_reason_of_its_own() {
        // `check_permission` never decides; the queue and the client do, and a
        // reason here would surface before the client had been asked anything.
        let decision = AcpPermissionHandler.check_permission(&request("Bash", false));
        assert!(matches!(decision, PermissionDecision::Ask { reason } if reason.is_empty()));
    }

    #[test]
    fn a_request_names_the_tool_and_what_it_would_do() {
        let mut pending = request("Bash", false);
        pending.details = Some("rm -rf /tmp/x".to_string());

        match AcpPermissionHandler.request_permission(&pending) {
            PermissionDecision::Ask { reason } => {
                assert!(reason.contains("Bash"), "{reason}");
                assert!(reason.contains("rm -rf /tmp/x"), "{reason}");
            }
            other => panic!("expected Ask, got {other:?}"),
        }
    }

    #[test]
    fn every_offered_option_maps_to_a_decision_of_its_own() {
        assert!(matches!(
            decision_for("allow_once"),
            PermissionDecision::Allow
        ));
        assert!(matches!(
            decision_for("allow_always"),
            PermissionDecision::AllowPermanently
        ));
        assert!(matches!(
            decision_for("reject_once"),
            PermissionDecision::Deny
        ));
        assert!(matches!(
            decision_for("reject_always"),
            PermissionDecision::DenyPermanently
        ));
    }

    #[test]
    fn an_option_that_was_never_offered_denies() {
        assert!(matches!(
            decision_for("allow_everything_forever"),
            PermissionDecision::Deny
        ));
    }

    #[test]
    fn a_request_naming_a_file_says_which_one() {
        let mut req = request("Edit", false);
        req.path = Some("/src/main.rs".to_string());

        let locations = locations_of(&req);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, std::path::PathBuf::from("/src/main.rs"));
    }

    #[test]
    fn a_request_naming_no_file_invents_none() {
        assert!(locations_of(&request("Bash", false)).is_empty());
    }

    #[test]
    fn an_edit_shows_both_sides_of_the_change() {
        let mut req = request("Edit", false);
        req.input = Some(serde_json::json!({
            "file_path": "/src/main.rs",
            "old_string": "fn old()",
            "new_string": "fn new()",
        }));

        let described = describe(&req);
        assert!(described.contains("fn old()"), "{described}");
        assert!(described.contains("fn new()"), "{described}");
    }

    #[test]
    fn replacing_everywhere_is_said_out_loud() {
        let mut req = request("Edit", false);
        req.input = Some(serde_json::json!({
            "old_string": "a",
            "new_string": "b",
            "replace_all": true,
        }));

        assert!(
            describe(&req).contains("every occurrence"),
            "{}",
            describe(&req)
        );
    }

    #[test]
    fn a_command_is_shown_before_it_runs() {
        let mut req = request("Bash", false);
        req.input = Some(serde_json::json!({ "command": "rm -rf /tmp/x" }));

        assert!(describe(&req).contains("rm -rf /tmp/x"));
    }

    #[test]
    fn a_tool_this_knows_nothing_about_falls_back_to_what_it_explained() {
        let mut req = request("SomeMcpTool", false);
        req.input = Some(serde_json::json!({ "anything": 1 }));
        req.context_description = Some("call the deploy endpoint".to_string());

        assert_eq!(describe(&req), "call the deploy endpoint");
    }

    #[test]
    fn a_request_that_explains_nothing_shows_nothing() {
        assert!(describe(&request("SomeMcpTool", false)).is_empty());
    }

    #[test]
    fn a_long_preview_is_cut_rather_than_sent_whole() {
        let long = "x".repeat(5000);
        let cut = truncate(&long);

        assert!(cut.chars().count() < long.chars().count());
        assert!(cut.ends_with("… (truncated)"), "{}", &cut[cut.len() - 40..]);
    }

    #[tokio::test]
    async fn a_write_is_shown_as_a_diff_against_what_is_on_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("notes.txt");
        tokio::fs::write(&path, "before\n").await.expect("seed");

        let mut req = request("Write", false);
        req.path = Some(path.display().to_string());
        req.input = Some(serde_json::json!({ "content": "after\n" }));

        let diff = write_diff(&req).await.expect("a write diffs");
        assert_eq!(diff.new_text, "after\n");
        assert_eq!(diff.old_text.as_deref(), Some("before\n"));
    }

    #[tokio::test]
    async fn creating_a_file_diffs_against_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut req = request("Write", false);
        req.path = Some(dir.path().join("new.txt").display().to_string());
        req.input = Some(serde_json::json!({ "content": "hello\n" }));

        let diff = write_diff(&req).await.expect("a write diffs");
        assert_eq!(diff.old_text, None, "a new file had no previous contents");
    }

    #[tokio::test]
    async fn an_edit_is_not_passed_off_as_a_whole_file_diff() {
        // The new contents do not exist yet, and a Diff carrying only the
        // changed fragment would render as though it were the whole file.
        let mut req = request("Edit", false);
        req.path = Some("/src/main.rs".to_string());
        req.input = Some(serde_json::json!({
            "old_string": "a",
            "new_string": "b",
        }));

        assert!(write_diff(&req).await.is_none());
        assert!(matches!(
            preview(&req).await.as_slice(),
            [acp::ToolCallContent::Content(_)]
        ));
    }

    #[test]
    fn a_request_without_details_still_names_the_tool() {
        match AcpPermissionHandler.request_permission(&request("WebFetch", false)) {
            PermissionDecision::Ask { reason } => assert!(reason.contains("WebFetch"), "{reason}"),
            other => panic!("expected Ask, got {other:?}"),
        }
    }
}
