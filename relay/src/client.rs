//! Client-facing API: what the web page and, later, a native app talk to.
//!
//! Deliberately separate from `runner.rs`. That surface is fixed by what
//! `claurst-bridge` already calls and cannot change; this one is ours, so it
//! can grow without breaking the CLI.
//!
//! Events go out over SSE rather than long-polling: a browser `EventSource`
//! and a native client can both consume it, and `?since=<seq>` lets a client
//! that dropped the connection resume instead of restarting the transcript.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::protocol::{BridgeMessage, PermissionDecision};
use crate::state::{Relay, SeqEvent, SessionSummary};

/// How long the stream waits for new events before looping.
///
/// Short enough that a dropped session is noticed promptly, long enough that
/// an idle stream is not spinning.
const STREAM_TICK: Duration = Duration::from_secs(20);

/// Comment sent periodically so proxies do not close an idle stream.
const KEEPALIVE: Duration = Duration::from_secs(15);

pub fn routes() -> Router<Arc<Relay>> {
    Router::new()
        .route("/api/client/auth", post(auth_check))
        .route("/api/client/sessions", get(list_sessions))
        .route("/api/client/sessions/{session_id}/stream", get(stream))
        .route("/api/client/sessions/{session_id}/prompt", post(prompt))
        .route(
            "/api/client/sessions/{session_id}/permission",
            post(permission),
        )
        .route("/api/client/sessions/{session_id}/cancel", post(cancel))
}

/// Confirm the token is good and hand back a cookie.
///
/// A browser `EventSource` cannot set an `Authorization` header, so the cookie
/// is the only way the stream endpoint can be reached from a web page.
///
/// The request has already cleared the auth layer to get here, so whatever it
/// presented is valid and is echoed straight into the cookie. Reaching for the
/// configured token instead would mean handing it to a second place that could
/// leak it.
async fn auth_check(headers: axum::http::HeaderMap) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::auth::bearer_from_header)
        .or_else(|| {
            headers
                .get(axum::http::header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(crate::auth::token_from_cookies)
        });

    let Some(token) = presented else {
        // Unreachable through the auth layer, but returning a cookie-less OK
        // would leave the page thinking it is authenticated.
        return StatusCode::UNAUTHORIZED.into_response();
    };

    (
        [(
            axum::http::header::SET_COOKIE,
            crate::auth::session_cookie(token),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

async fn list_sessions(State(relay): State<Arc<Relay>>) -> Json<Vec<SessionSummary>> {
    Json(relay.summaries().await)
}

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// Last sequence number the client already has. `0` replays the buffer.
    #[serde(default)]
    since: u64,
}

/// Stream a session's events, resuming from `since`.
async fn stream(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !relay.exists(&session_id).await {
        return Err(StatusCode::NOT_FOUND);
    }

    struct StreamState {
        relay: Arc<Relay>,
        session_id: String,
        last_seq: u64,
        pending: VecDeque<SeqEvent>,
    }

    let stream = futures::stream::unfold(
        StreamState {
            relay,
            session_id,
            last_seq: query.since,
            pending: VecDeque::new(),
        },
        |mut state| async move {
            loop {
                if let Some(entry) = state.pending.pop_front() {
                    state.last_seq = entry.seq;
                    // Serialisation of a `serde_json::Value` cannot fail, and
                    // the event is echoed verbatim so the client sees exactly
                    // what the runner sent.
                    let event = Event::default()
                        .id(entry.seq.to_string())
                        .json_data(&entry.event)
                        .unwrap_or_else(|_| Event::default().comment("unserialisable event"));
                    return Some((Ok(event), state));
                }

                // Register interest before reading, so an event arriving
                // between the read and the wait is not missed.
                let notified = state.relay.notifier(&state.session_id).await;

                match state
                    .relay
                    .events_since(&state.session_id, state.last_seq)
                    .await
                {
                    Some((events, _)) if !events.is_empty() => {
                        state.pending.extend(events);
                        continue;
                    }
                    // Session gone: end the stream so the client stops waiting.
                    None => return None,
                    Some(_) => {}
                }

                let notify = notified?;
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(STREAM_TICK) => {}
                }
            }
        },
    );

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}

#[derive(Debug, Deserialize)]
pub struct PromptBody {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Accepted {
    pub ok: bool,
}

async fn prompt(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
    Json(body): Json<PromptBody>,
) -> Result<Json<Accepted>, StatusCode> {
    if body.content.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    info!(session_id = %session_id, "client sent a prompt");
    enqueue(
        &relay,
        &session_id,
        BridgeMessage::UserMessage {
            content: body.content,
            session_id: session_id.clone(),
            message_id: uuid::Uuid::new_v4().to_string(),
            attachments: Vec::new(),
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct PermissionBody {
    pub request_id: String,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    pub decision: PermissionDecision,
}

async fn permission(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
    Json(body): Json<PermissionBody>,
) -> Result<Json<Accepted>, StatusCode> {
    info!(
        session_id = %session_id,
        decision = ?body.decision,
        "client answered a permission request"
    );
    enqueue(
        &relay,
        &session_id,
        BridgeMessage::PermissionResponse {
            request_id: body.request_id,
            tool_use_id: body.tool_use_id,
            decision: body.decision,
        },
    )
    .await
}

#[derive(Debug, Deserialize, Default)]
pub struct CancelBody {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Cancel the in-progress turn.
///
/// The body is read leniently rather than through the `Json` extractor: cancel
/// is the one call a client makes when things are already going wrong, and
/// failing it because the body was empty would be the worst moment to be
/// strict. `reason` is only a diagnostic, so an unparseable body drops it and
/// still cancels.
async fn cancel(
    State(relay): State<Arc<Relay>>,
    Path(session_id): Path<String>,
    body: String,
) -> Result<Json<Accepted>, StatusCode> {
    let reason = serde_json::from_str::<CancelBody>(&body)
        .ok()
        .and_then(|parsed| parsed.reason);
    info!(session_id = %session_id, "client cancelled the turn");
    enqueue(
        &relay,
        &session_id,
        BridgeMessage::Cancel {
            session_id: session_id.clone(),
            reason,
        },
    )
    .await
}

/// Queue a message, reporting a dead session rather than dropping it.
async fn enqueue(
    relay: &Relay,
    session_id: &str,
    message: BridgeMessage,
) -> Result<Json<Accepted>, StatusCode> {
    if relay.push_inbound(session_id, message).await {
        Ok(Json(Accepted { ok: true }))
    } else {
        warn!(
            session_id,
            "client addressed a session that is not connected"
        );
        Err(StatusCode::NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Limits;
    use axum::body::Body;
    use axum::http::{header, Request as HttpRequest};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn relay() -> Arc<Relay> {
        Arc::new(Relay::new(Limits::default()))
    }

    fn app(relay: Arc<Relay>) -> Router {
        crate::app(relay, Arc::new(TOKEN.to_string()))
    }

    fn authed(method: &str, uri: &str) -> axum::http::request::Builder {
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::CONTENT_TYPE, "application/json")
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn auth_hands_back_a_cookie_carrying_the_presented_token() {
        let response = app(relay())
            .oneshot(
                authed("POST", "/api/client/auth")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .expect("cookie");
        assert!(
            cookie.contains(TOKEN),
            "the cookie has to carry a token the auth layer will accept: {cookie}"
        );
        assert!(cookie.contains("HttpOnly"), "page scripts must not read it");
        assert!(cookie.contains("SameSite=Strict"));
    }

    #[tokio::test]
    async fn the_cookie_alone_authenticates_a_request() {
        // The SSE endpoint is only reachable this way, because EventSource
        // cannot set headers.
        let relay = relay();
        relay.register("s1", None, None, None, None).await;

        let response = app(relay)
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/client/sessions")
                    .header(header::COOKIE, format!("relay_token={TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_wrong_cookie_is_refused() {
        let response = app(relay())
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/client/sessions")
                    .header(header::COOKIE, "relay_token=wrong-token-but-long-enough-x")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn the_session_list_carries_the_label_the_runner_registered() {
        let relay = relay();
        relay
            .register(
                "s1",
                Some("dev".into()),
                Some("0.1.7".into()),
                Some("claurst".into()),
                Some("/home/k/claurst".into()),
            )
            .await;

        let response = app(relay)
            .oneshot(
                authed("GET", "/api/client/sessions")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let body = body_json(response).await;
        assert_eq!(body[0]["session_id"], "s1");
        assert_eq!(body[0]["label"], "claurst");
        assert_eq!(body[0]["cwd"], "/home/k/claurst");
    }

    #[tokio::test]
    async fn a_prompt_reaches_the_runner_queue() {
        let relay = relay();
        relay.register("s1", None, None, None, None).await;

        let response = app(relay.clone())
            .oneshot(
                authed("POST", "/api/client/sessions/s1/prompt")
                    .body(Body::from(json!({ "content": "hello" }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let queued = relay.take_inbound("s1").await;
        match &queued[..] {
            [BridgeMessage::UserMessage { content, .. }] => assert_eq!(content, "hello"),
            other => panic!("expected one user message, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_prompt_is_refused() {
        let relay = relay();
        relay.register("s1", None, None, None, None).await;

        let response = app(relay.clone())
            .oneshot(
                authed("POST", "/api/client/sessions/s1/prompt")
                    .body(Body::from(json!({ "content": "   " }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(relay.take_inbound("s1").await.is_empty());
    }

    #[tokio::test]
    async fn a_prompt_for_a_disconnected_session_is_reported() {
        // Silently accepting it would leave the user waiting for a reply that
        // can never come.
        let response = app(relay())
            .oneshot(
                authed("POST", "/api/client/sessions/gone/prompt")
                    .body(Body::from(json!({ "content": "hello" }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_permission_answer_reaches_the_runner_queue() {
        let relay = relay();
        relay.register("s1", None, None, None, None).await;

        let response = app(relay.clone())
            .oneshot(
                authed("POST", "/api/client/sessions/s1/permission")
                    .body(Body::from(
                        json!({
                            "request_id": "r1",
                            "tool_use_id": "t1",
                            "decision": "allow"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let queued = relay.take_inbound("s1").await;
        match &queued[..] {
            [BridgeMessage::PermissionResponse {
                request_id,
                decision,
                ..
            }] => {
                assert_eq!(request_id, "r1");
                assert_eq!(*decision, PermissionDecision::Allow);
            }
            other => panic!("expected one permission response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_cancel_needs_no_body() {
        let relay = relay();
        relay.register("s1", None, None, None, None).await;

        let response = app(relay.clone())
            .oneshot(
                authed("POST", "/api/client/sessions/s1/cancel")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(
            relay.take_inbound("s1").await[..],
            [BridgeMessage::Cancel { .. }]
        ));
    }

    #[tokio::test]
    async fn streaming_an_unknown_session_is_a_not_found() {
        let response = app(relay())
            .oneshot(
                authed("GET", "/api/client/sessions/gone/stream")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_stream_replays_buffered_events_from_since() {
        let relay = relay();
        relay.register("s1", None, None, None, None).await;
        relay
            .push_events(
                "s1",
                vec![
                    json!({ "type": "text_delta", "text": "a" }),
                    json!({ "type": "text_delta", "text": "b" }),
                ],
            )
            .await;

        let response = app(relay)
            .oneshot(
                authed("GET", "/api/client/sessions/s1/stream?since=1")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // Read only the first frame; the stream stays open by design.
        let mut body = response.into_body().into_data_stream();
        let frame = futures::StreamExt::next(&mut body)
            .await
            .expect("a frame")
            .expect("bytes");
        let text = String::from_utf8_lossy(&frame).to_string();

        assert!(text.contains("id: 2"), "resume must skip seq 1: {text}");
        assert!(text.contains("\"text\":\"b\""), "{text}");
    }
}
