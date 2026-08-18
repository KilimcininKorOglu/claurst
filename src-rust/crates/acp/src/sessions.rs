//! Per-session state for the ACP server.

use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol_schema as acp;
use claurst_core::types::Message;
use claurst_tools::PendingPermissionStore;
use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

/// What the connected client has changed for this session alone.
///
/// Every field is an override on top of the runtime's own configuration, and
/// none of it is written to `settings.json`: a choice made in an editor panel
/// belongs to that panel's session, not to the user's next terminal run.
#[derive(Debug, Clone, Default)]
pub struct SessionSettings {
    pub permission_mode: Option<claurst_core::PermissionMode>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub effort: Option<claurst_core::effort::EffortLevel>,
}

/// One ACP session — a logical conversation with its own cwd, transcript,
/// MCP server roster, and cancellation token.
pub struct SessionState {
    pub session_id: acp::SessionId,
    pub cwd: PathBuf,
    pub messages: parking_lot::Mutex<Vec<Message>>,
    pub cancel_token: CancellationToken,
    pub pending_permissions: Arc<parking_lot::Mutex<PendingPermissionStore>>,
    pub file_history: Arc<parking_lot::Mutex<claurst_core::file_history::FileHistory>>,
    pub current_turn: Arc<std::sync::atomic::AtomicUsize>,
    pub settings: parking_lot::Mutex<SessionSettings>,
}

impl SessionState {
    pub fn new(session_id: acp::SessionId, cwd: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            cwd,
            messages: parking_lot::Mutex::new(Vec::new()),
            cancel_token: CancellationToken::new(),
            pending_permissions: Arc::new(parking_lot::Mutex::new(
                PendingPermissionStore::default(),
            )),
            file_history: Arc::new(parking_lot::Mutex::new(
                claurst_core::file_history::FileHistory::new(),
            )),
            current_turn: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            settings: parking_lot::Mutex::new(SessionSettings::default()),
        })
    }
}

/// Map of active sessions keyed by ACP session id.
#[derive(Default)]
pub struct SessionRegistry {
    inner: DashMap<acp::SessionId, Arc<SessionState>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, state: Arc<SessionState>) {
        self.inner.insert(state.session_id.clone(), state);
    }

    pub fn get(&self, id: &acp::SessionId) -> Option<Arc<SessionState>> {
        self.inner.get(id).map(|r| r.value().clone())
    }

    pub fn remove(&self, id: &acp::SessionId) -> Option<Arc<SessionState>> {
        self.inner.remove(id).map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_session_starts_with_nothing_recorded() {
        let id = acp::SessionId::new("session-1");
        let cwd = PathBuf::from("/tmp/claurst-test");
        let state = SessionState::new(id.clone(), cwd.clone());

        assert_eq!(state.session_id, id);
        assert_eq!(state.cwd, cwd);
        assert!(state.messages.lock().is_empty());
        assert_eq!(
            state.current_turn.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        // A fresh token, so a cancelled predecessor cannot abort this session.
        assert!(!state.cancel_token.is_cancelled());
    }

    #[test]
    fn two_sessions_cancel_independently() {
        let first = SessionState::new(acp::SessionId::new("a"), PathBuf::from("/tmp/a"));
        let second = SessionState::new(acp::SessionId::new("b"), PathBuf::from("/tmp/b"));

        first.cancel_token.cancel();

        assert!(first.cancel_token.is_cancelled());
        assert!(!second.cancel_token.is_cancelled());
    }

    #[test]
    fn a_session_survives_a_round_trip_through_the_registry() {
        let registry = SessionRegistry::new();
        let id = acp::SessionId::new("session-2");
        let state = SessionState::new(id.clone(), PathBuf::from("/tmp"));

        assert!(registry.get(&id).is_none());
        registry.insert(Arc::clone(&state));

        let fetched = registry.get(&id).expect("present after insert");
        assert!(
            Arc::ptr_eq(&fetched, &state),
            "the registry cloned the state"
        );

        let removed = registry.remove(&id).expect("present before remove");
        assert!(Arc::ptr_eq(&removed, &state));
        assert!(registry.get(&id).is_none());
    }

    #[test]
    fn removing_an_unknown_session_is_not_an_error() {
        let registry = SessionRegistry::new();
        assert!(registry.remove(&acp::SessionId::new("missing")).is_none());
    }
}
