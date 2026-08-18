//! Bridge between Claurst's synchronous `PermissionHandler` trait and the
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
use claurst_core::permissions::{PermissionDecision, PermissionRequest};
use claurst_core::PermissionHandler;
use claurst_tools::{PendingPermissionRequest, PendingPermissionStore};
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

    let fields = acp::ToolCallUpdateFields::new()
        .kind(Some(infer_tool_kind(&request)))
        .status(Some(acp::ToolCallStatus::Pending))
        .title(Some(title));
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
            acp::RequestPermissionOutcome::Selected(sel) => match sel.option_id.0.as_ref() {
                "allow_once" => PermissionDecision::Allow,
                "allow_always" => PermissionDecision::AllowPermanently,
                "reject_always" => PermissionDecision::DenyPermanently,
                _ => PermissionDecision::Deny,
            },
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
    fn a_request_without_details_still_names_the_tool() {
        match AcpPermissionHandler.request_permission(&request("WebFetch", false)) {
            PermissionDecision::Ask { reason } => assert!(reason.contains("WebFetch"), "{reason}"),
            other => panic!("expected Ask, got {other:?}"),
        }
    }
}
