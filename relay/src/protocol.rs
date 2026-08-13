//! Wire types shared with `claurst-bridge`.
//!
//! These mirror `crates/bridge/src/lib.rs` field for field. The relay does not
//! interpret them, but it does re-serialise them, so the shapes have to match
//! or the CLI silently drops messages it cannot parse.
//!
//! Kept as a local copy rather than a dependency: pulling in `claurst-bridge`
//! would drag the whole claurst dependency tree into the relay image for four
//! enums.

use serde::{Deserialize, Serialize};

/// A file attachment bundled with an inbound user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeAttachment {
    pub name: String,
    pub content: String,
    pub mime_type: Option<String>,
}

/// A tool-use permission decision sent by the client back to the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    AllowPermanently,
    Deny,
    DenyPermanently,
}

/// Messages flowing from the client into the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeMessage {
    UserMessage {
        content: String,
        session_id: String,
        message_id: String,
        #[serde(default)]
        attachments: Vec<BridgeAttachment>,
    },
    PermissionResponse {
        request_id: String,
        tool_use_id: Option<String>,
        decision: PermissionDecision,
    },
    Cancel {
        session_id: String,
        reason: Option<String>,
    },
    Ping,
}

/// Events flowing from the CLI up to the client.
///
/// Held as an opaque `serde_json::Value` rather than a typed enum: the relay
/// only stores and forwards them, and a typed copy would have to be updated in
/// lockstep with the CLI or it would reject events it does not recognise.
pub type BridgeEvent = serde_json::Value;

/// Body of `POST /api/claude_code/sessions`.
#[derive(Debug, Clone, Deserialize)]
pub struct RegisterBody {
    pub session_id: String,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    /// Human-readable name for this machine. Sent by newer CLIs; without it the
    /// client can only show an opaque session id.
    #[serde(default)]
    pub label: Option<String>,
    /// Working directory of the session, used as a fallback label.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Body of `POST /api/claude_code/sessions/{id}/events`.
#[derive(Debug, Clone, Deserialize)]
pub struct EventsBody {
    #[serde(default)]
    pub events: Vec<BridgeEvent>,
}
