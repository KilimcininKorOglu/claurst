//! Runner-facing API: the endpoints `claurst-bridge` already calls.
//!
//! This surface is fixed by the CLI, not by us. `crates/bridge/src/lib.rs`
//! hardcodes the paths, the bare-array poll response and the `{"events": []}`
//! upload body, so they are reproduced exactly.
//!
//! The bridge speaks two protocols concurrently. `/api/claude_code/sessions`
//! is the primary one and carries prompts, permissions and cancellation.
//! `/api/bridge/sessions` is described in the CLI as a "best-effort
//! supplementary delivery path" and carries a subset of the same events; it is
//! accepted here and discarded, so the CLI's background loop gets clean
//! responses instead of hammering 404s.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tracing::{debug, info};

use crate::protocol::{BridgeMessage, EventsBody, RegisterBody};
use crate::state::Relay;

/// How long a poll is held open before returning an empty batch.
///
/// The CLI's HTTP timeout is 35 s (`bridge/src/lib.rs`), so this leaves a wide
/// margin for the response to travel.
const POLL_HOLD: Duration = Duration::from_secs(25);

pub fn routes() -> Router<Arc<Relay>> {
    Router::new()
        // Primary protocol.
        .route("/api/claude_code/sessions", post(register))
        .route(
            "/api/claude_code/sessions/{session_id}",
            axum::routing::delete(deregister),
        )
        .route("/api/claude_code/sessions/{session_id}/poll", get(poll))
        .route(
            "/api/claude_code/sessions/{session_id}/events",
            post(upload_events),
        )
        // Supplementary protocol: accepted so the CLI's secondary loop stays
        // quiet. Its payloads duplicate the primary path, so nothing is lost.
        .route("/api/bridge/sessions", post(register_supplementary))
        .route(
            "/api/bridge/sessions/{session_id}/messages",
            get(supplementary_messages),
        )
        .route(
            "/api/bridge/sessions/{session_id}/events",
            post(supplementary_events),
        )
}

async fn register(State(relay): State<Arc<Relay>>, Json(body): Json<RegisterBody>) -> StatusCode {
    info!(
        session_id = %body.session_id,
        label = ?body.label,
        "runner registered"
    );
    relay.register(&body).await;
    StatusCode::CREATED
}

async fn deregister(State(relay): State<Arc<Relay>>, Path(session_id): Path<String>) -> StatusCode {
    info!(session_id = %session_id, "runner deregistered");
    relay.deregister(&session_id).await;
    StatusCode::NO_CONTENT
}

/// Long-poll for queued messages.
///
/// Returns a bare JSON array, which is what the CLI parses. Holding the
/// request open keeps latency low without the CLI polling in a tight loop; an
/// empty array after the hold is the normal idle result.
async fn poll(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
) -> Json<Vec<BridgeMessage>> {
    let ready = relay.take_inbound(&session_id).await;
    if !ready.is_empty() {
        return Json(ready);
    }

    if let Some(notify) = relay.notifier(&session_id).await {
        // `notified()` is created before the wait so a message queued between
        // the drain above and here is not missed.
        let waiter = notify.notified();
        tokio::select! {
            _ = waiter => {}
            _ = tokio::time::sleep(POLL_HOLD) => {}
        }
    }

    Json(relay.take_inbound(&session_id).await)
}

async fn upload_events(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
    Json(body): Json<EventsBody>,
) -> StatusCode {
    debug!(session_id = %session_id, count = body.events.len(), "events uploaded");
    relay.push_events(&session_id, body.events).await;
    StatusCode::ACCEPTED
}

// ---------------------------------------------------------------------------
// Supplementary protocol
// ---------------------------------------------------------------------------

async fn register_supplementary() -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// Always empty: prompts are delivered through the primary poll endpoint, and
/// answering here too would deliver every prompt twice.
async fn supplementary_messages() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

async fn supplementary_events() -> StatusCode {
    StatusCode::ACCEPTED
}
