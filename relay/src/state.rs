//! Session registry: one inbound queue and one bounded event buffer per
//! session.
//!
//! Everything lives in memory. A relay restart drops the sessions and the CLI
//! re-registers on its next poll, which is cheap; persisting the events would
//! mean keeping a durable copy of the user's code on the relay host.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Notify, RwLock};
use tracing::warn;

use crate::protocol::{BridgeEvent, BridgeMessage, RegisterBody};

/// An event as stored, tagged with the sequence number a client resumes from.
#[derive(Debug, Clone, Serialize)]
pub struct SeqEvent {
    pub seq: u64,
    pub event: BridgeEvent,
}

/// One connected runner.
pub struct Session {
    /// Messages waiting for the runner to collect on its next poll.
    to_runner: VecDeque<BridgeMessage>,
    /// Recent events, oldest first, capped at `event_buffer`.
    events: VecDeque<SeqEvent>,
    next_seq: u64,
    last_seen: Instant,
    pub device_id: Option<String>,
    pub client_version: Option<String>,
    /// Display name for the session list.
    pub label: Option<String>,
    pub cwd: Option<String>,
    /// Facts that change while the session runs, refreshed by re-registration
    /// so the list can tell two sessions apart before either is opened.
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub cost_usd: Option<f64>,
    /// Title the operator gave the session, shown instead of the label.
    pub title: Option<String>,
    /// Woken when either queue gains an entry, so pollers do not spin.
    notify: Arc<Notify>,
}

/// A session as shown to a client.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub device_id: Option<String>,
    pub client_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Seconds since the runner last talked to the relay.
    pub idle_secs: u64,
    /// Highest sequence number issued so far, so a client can resume from it.
    pub latest_seq: u64,
}

/// Limits shared by every session.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Events retained per session.
    pub event_buffer: usize,
    /// Messages queued for a runner before the oldest is dropped.
    pub inbound_queue: usize,
    /// How long a session survives without the runner polling.
    pub session_ttl: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            event_buffer: 500,
            inbound_queue: 100,
            session_ttl: Duration::from_secs(15 * 60),
        }
    }
}

impl Session {
    fn new(limits: Limits) -> Self {
        let _ = limits;
        Self {
            to_runner: VecDeque::new(),
            events: VecDeque::new(),
            next_seq: 1,
            last_seen: Instant::now(),
            device_id: None,
            client_version: None,
            label: None,
            cwd: None,
            model: None,
            permission_mode: None,
            cost_usd: None,
            title: None,
            notify: Arc::new(Notify::new()),
        }
    }

    fn touch(&mut self) {
        self.last_seen = Instant::now();
    }
}

/// The whole relay's state.
pub struct Relay {
    sessions: RwLock<HashMap<String, Session>>,
    limits: Limits,
}

impl Relay {
    pub fn new(limits: Limits) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            limits,
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Create or refresh a session. Re-registering keeps the event buffer, so a
    /// runner that reconnects does not blank the client's view.
    /// Record a registration, merging over whatever is already stored.
    ///
    /// A field left absent keeps its previous value: the runner re-registers
    /// when the model, mode or cost changes and does not resend the rest, so
    /// overwriting unconditionally would blank the label on every update.
    pub async fn register(&self, body: &RegisterBody) {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(body.session_id.clone())
            .or_insert_with(|| Session::new(self.limits));
        session.touch();
        if body.device_id.is_some() {
            session.device_id = body.device_id.clone();
        }
        if body.client_version.is_some() {
            session.client_version = body.client_version.clone();
        }
        if body.label.is_some() {
            session.label = body.label.clone();
        }
        if body.cwd.is_some() {
            session.cwd = body.cwd.clone();
        }
        if body.model.is_some() {
            session.model = body.model.clone();
        }
        if body.permission_mode.is_some() {
            session.permission_mode = body.permission_mode.clone();
        }
        if body.cost_usd.is_some() {
            session.cost_usd = body.cost_usd;
        }
        if body.title.is_some() {
            session.title = body.title.clone();
        }
    }

    pub async fn deregister(&self, session_id: &str) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(session_id) {
            // Wake anyone still parked on this session so they observe the
            // removal instead of waiting out the poll timeout.
            session.notify.notify_waiters();
        }
    }

    pub async fn exists(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }

    /// Take everything queued for the runner. Registers the session on first
    /// contact, so a poll that arrives before the register call still works.
    pub async fn take_inbound(&self, session_id: &str) -> Vec<BridgeMessage> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| Session::new(self.limits));
        session.touch();
        session.to_runner.drain(..).collect()
    }

    /// Queue a message for the runner and wake its poll.
    ///
    /// Returns `false` when the session is unknown, so a client posting to a
    /// dead session is told rather than having the message vanish.
    pub async fn push_inbound(&self, session_id: &str, message: BridgeMessage) -> bool {
        let mut sessions = self.sessions.write().await;
        let Some(session) = sessions.get_mut(session_id) else {
            return false;
        };
        if session.to_runner.len() >= self.limits.inbound_queue {
            warn!(
                session_id,
                limit = self.limits.inbound_queue,
                "inbound queue full; dropping the oldest message"
            );
            session.to_runner.pop_front();
        }
        session.to_runner.push_back(message);
        session.notify.notify_waiters();
        true
    }

    /// Append runner events and wake any listening clients.
    pub async fn push_events(&self, session_id: &str, events: Vec<BridgeEvent>) {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| Session::new(self.limits));
        session.touch();
        for event in events {
            let seq = session.next_seq;
            session.next_seq += 1;
            if session.events.len() >= self.limits.event_buffer {
                session.events.pop_front();
            }
            session.events.push_back(SeqEvent { seq, event });
        }
        session.notify.notify_waiters();
    }

    /// Events newer than `since`, plus the highest sequence number issued.
    ///
    /// A client that has fallen further behind than the buffer holds simply
    /// gets the oldest retained events; the gap is visible in the sequence
    /// numbers rather than silently papered over.
    pub async fn events_since(&self, session_id: &str, since: u64) -> Option<(Vec<SeqEvent>, u64)> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(session_id)?;
        let pending = session
            .events
            .iter()
            .filter(|entry| entry.seq > since)
            .cloned()
            .collect();
        Some((pending, session.next_seq.saturating_sub(1)))
    }

    /// Handle to wait on for new activity in a session.
    pub async fn notifier(&self, session_id: &str) -> Option<Arc<Notify>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|s| s.notify.clone())
    }

    /// Every live session, newest activity first.
    pub async fn summaries(&self) -> Vec<SessionSummary> {
        let sessions = self.sessions.read().await;
        // Ordered on the raw instant, not on `idle_secs`: that field is rounded
        // to whole seconds for display, so sorting by it would order two
        // sessions active a few hundred milliseconds apart arbitrarily.
        let mut ordered: Vec<(&String, &Session)> = sessions.iter().collect();
        ordered.sort_by_key(|(_, session)| std::cmp::Reverse(session.last_seen));

        ordered
            .into_iter()
            .map(|(id, session)| SessionSummary {
                session_id: id.clone(),
                label: session.label.clone(),
                cwd: session.cwd.clone(),
                device_id: session.device_id.clone(),
                client_version: session.client_version.clone(),
                model: session.model.clone(),
                permission_mode: session.permission_mode.clone(),
                cost_usd: session.cost_usd,
                title: session.title.clone(),
                idle_secs: session.last_seen.elapsed().as_secs(),
                latest_seq: session.next_seq.saturating_sub(1),
            })
            .collect()
    }

    /// Drop sessions whose runner has gone quiet past the TTL.
    ///
    /// Returns how many were removed so the caller can log it.
    pub async fn sweep_expired(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let ttl = self.limits.session_ttl;
        let before = sessions.len();
        sessions.retain(|id, session| {
            let alive = session.last_seen.elapsed() < ttl;
            if !alive {
                warn!(session_id = %id, "session expired; dropping it");
                session.notify.notify_waiters();
            }
            alive
        });
        before - sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn relay() -> Relay {
        Relay::new(Limits {
            event_buffer: 3,
            inbound_queue: 2,
            session_ttl: Duration::from_secs(60),
        })
    }

    fn ping() -> BridgeMessage {
        BridgeMessage::Ping
    }

    #[tokio::test]
    async fn a_message_for_an_unknown_session_is_refused() {
        let relay = relay();
        assert!(
            !relay.push_inbound("nope", ping()).await,
            "a client must learn its message went nowhere"
        );
    }

    #[tokio::test]
    async fn the_runner_collects_what_a_client_queued() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        assert!(relay.push_inbound("s1", ping()).await);

        let taken = relay.take_inbound("s1").await;
        assert_eq!(taken.len(), 1);
        assert!(
            relay.take_inbound("s1").await.is_empty(),
            "a collected message must not be delivered twice"
        );
    }

    #[tokio::test]
    async fn a_full_inbound_queue_drops_the_oldest() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        for _ in 0..5 {
            relay.push_inbound("s1", ping()).await;
        }
        assert_eq!(relay.take_inbound("s1").await.len(), 2);
    }

    #[tokio::test]
    async fn the_event_buffer_keeps_the_newest_and_its_sequence_numbers() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        relay
            .push_events("s1", (1..=5).map(|n| json!({ "n": n })).collect())
            .await;

        let (events, latest) = relay.events_since("s1", 0).await.expect("session");
        assert_eq!(latest, 5);
        assert_eq!(events.len(), 3, "buffer holds 3");
        assert_eq!(events[0].seq, 3, "the oldest retained event keeps its seq");
        assert_eq!(events[2].event, json!({ "n": 5 }));
    }

    #[tokio::test]
    async fn a_client_resumes_from_its_last_sequence_number() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        relay
            .push_events("s1", vec![json!({ "n": 1 }), json!({ "n": 2 })])
            .await;

        let (events, _) = relay.events_since("s1", 1).await.expect("session");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 2);
    }

    #[tokio::test]
    async fn reregistering_keeps_the_events_already_buffered() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        relay.push_events("s1", vec![json!({ "n": 1 })]).await;
        relay
            .register(&RegisterBody {
                session_id: "s1".into(),
                device_id: Some("dev".into()),
                label: Some("work".into()),
                ..Default::default()
            })
            .await;

        let (events, _) = relay.events_since("s1", 0).await.expect("session");
        assert_eq!(
            events.len(),
            1,
            "a reconnecting runner must not blank the client's view"
        );
    }

    #[tokio::test]
    async fn registering_again_without_a_label_keeps_the_old_one() {
        let relay = relay();
        relay
            .register(&RegisterBody {
                session_id: "s1".into(),
                label: Some("work".into()),
                ..Default::default()
            })
            .await;
        relay.register(&RegisterBody::new("s1")).await;

        let summaries = relay.summaries().await;
        assert_eq!(summaries[0].label.as_deref(), Some("work"));
    }

    #[tokio::test]
    async fn a_reregistration_updates_only_what_it_carries() {
        // The runner re-registers whenever the model, mode or cost changes and
        // does not resend the rest. If absent fields overwrote, every update
        // would blank the label and cwd the list is built from.
        let relay = relay();
        relay
            .register(&RegisterBody {
                session_id: "s1".into(),
                label: Some("work".into()),
                model: Some("claude-sonnet-4-5".into()),
                permission_mode: Some("plan".into()),
                cost_usd: Some(0.0421),
                title: Some("parser rewrite".into()),
                ..Default::default()
            })
            .await;
        relay
            .register(&RegisterBody {
                session_id: "s1".into(),
                cost_usd: Some(0.0833),
                ..Default::default()
            })
            .await;

        let summaries = relay.summaries().await;
        assert_eq!(summaries[0].label.as_deref(), Some("work"));
        assert_eq!(summaries[0].model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(summaries[0].permission_mode.as_deref(), Some("plan"));
        assert_eq!(summaries[0].cost_usd, Some(0.0833));
        assert_eq!(summaries[0].title.as_deref(), Some("parser rewrite"));
    }

    #[tokio::test]
    async fn deregistering_removes_the_session() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        relay.deregister("s1").await;
        assert!(!relay.exists("s1").await);
        assert!(relay.events_since("s1", 0).await.is_none());
    }

    #[tokio::test]
    async fn an_idle_session_is_swept() {
        let relay = Relay::new(Limits {
            session_ttl: Duration::from_millis(1),
            ..Limits::default()
        });
        relay.register(&RegisterBody::new("s1")).await;
        tokio::time::sleep(Duration::from_millis(5)).await;

        assert_eq!(relay.sweep_expired().await, 1);
        assert!(!relay.exists("s1").await);
    }

    #[tokio::test]
    async fn an_active_session_survives_the_sweep() {
        let relay = relay();
        relay.register(&RegisterBody::new("s1")).await;
        assert_eq!(relay.sweep_expired().await, 0);
        assert!(relay.exists("s1").await);
    }

    #[tokio::test]
    async fn summaries_list_the_most_recently_active_first() {
        let relay = relay();
        relay.register(&RegisterBody::new("old")).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        relay.register(&RegisterBody::new("new")).await;

        let summaries = relay.summaries().await;
        assert_eq!(summaries[0].session_id, "new");
    }
}
