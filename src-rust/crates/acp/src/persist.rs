//! Where an ACP session lives between connections.
//!
//! Sessions are written to the same store the terminal uses, so one started
//! in an editor is listed by `/resume` and can be loaded back after the
//! editor, the agent, or the machine restarts.

use std::sync::Arc;

use claurst_core::history::{self, ConversationSession};

use crate::sessions::SessionState;

/// Write a session's current state to disk.
///
/// Failures are logged and swallowed: a transcript that cannot be filed is
/// worth a warning, not a refused turn the user has already paid for.
pub async fn save(session: &Arc<SessionState>, model: &str) {
    let mut stored = ConversationSession::new(model.to_string());
    stored.id = session.session_id.0.to_string();
    stored.created_at = session.created_at;
    stored.updated_at = chrono::Utc::now();
    stored.messages = session.messages.lock().clone();
    stored.title = session.title.lock().clone();
    stored.working_dir = Some(session.cwd.display().to_string());
    if let Some((parent_id, at_message)) = session.forked_from.clone() {
        stored.parent_session_id = Some(parent_id);
        stored.fork_point_message_index = Some(at_message);
    }

    if let Err(e) = history::save_session(&stored).await {
        tracing::warn!(?e, session_id = %stored.id, "ACP: could not save the session");
    }
}

/// The longest a derived title runs before it is cut short.
const TITLE_CHARS: usize = 60;

/// Name a session after the first thing the user asked it.
///
/// A session listed only by its uuid tells the user nothing, and nothing in
/// the protocol names a session on their behalf, so the opening request is
/// used: it is what a person would call the conversation anyway. An explicit
/// rename replaces this and is never overwritten.
pub fn derive_title(messages: &[claurst_core::types::Message]) -> Option<String> {
    let first = messages
        .iter()
        .find(|m| m.role == claurst_core::types::Role::User)?;
    let line = first
        .get_text()?
        .lines()
        .find(|l| !l.trim().is_empty())?
        .trim();
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    Some(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::types::Message;
    use std::path::PathBuf;

    /// `CLAURST_HOME` is process-wide, so the tests that move it run one at a
    /// time and put it back when they are done.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        previous: Option<String>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn set() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let previous = std::env::var("CLAURST_HOME").ok();
            unsafe { std::env::set_var("CLAURST_HOME", dir.path()) };
            Self {
                previous,
                _dir: dir,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var("CLAURST_HOME", v) },
                None => unsafe { std::env::remove_var("CLAURST_HOME") },
            }
        }
    }

    fn session_with(messages: Vec<Message>) -> Arc<SessionState> {
        let state = SessionState::new(
            agent_client_protocol_schema::SessionId::new("acp-session-1"),
            PathBuf::from("/tmp/claurst-persist-test"),
        );
        *state.messages.lock() = messages;
        state
    }

    #[test]
    fn a_session_is_named_after_the_first_thing_asked_of_it() {
        let messages = vec![
            Message::user("rename the crate"),
            Message::assistant("done"),
            Message::user("now the tests"),
        ];
        assert_eq!(derive_title(&messages).as_deref(), Some("rename the crate"));
    }

    #[test]
    fn a_long_first_request_is_cut_short() {
        let long = "a".repeat(TITLE_CHARS + 20);
        let title = derive_title(&[Message::user(long)]).expect("a title");
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_request_that_opens_with_blank_lines_is_named_by_its_first_words() {
        let messages = vec![Message::user("\n\n  fix the parser  \nand the lexer")];
        assert_eq!(derive_title(&messages).as_deref(), Some("fix the parser"));
    }

    #[test]
    fn a_session_with_nothing_said_yet_has_no_name() {
        assert!(derive_title(&[]).is_none());
        assert!(derive_title(&[Message::assistant("hello")]).is_none());
    }

    #[tokio::test]
    async fn a_saved_session_comes_back_with_its_transcript_and_directory() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::set();

        let session = session_with(vec![Message::user("hello"), Message::assistant("hi")]);
        *session.title.lock() = Some("greeting".to_string());
        save(&session, "claude-opus-4").await;

        let stored = history::load_session("acp-session-1")
            .await
            .expect("the session was filed under its ACP id");
        assert_eq!(stored.messages.len(), 2);
        assert_eq!(stored.title.as_deref(), Some("greeting"));
        assert_eq!(
            stored.working_dir.as_deref(),
            Some("/tmp/claurst-persist-test")
        );
        assert_eq!(stored.model, "claude-opus-4");
        assert_eq!(stored.created_at, session.created_at);
    }

    #[tokio::test]
    async fn saving_twice_keeps_the_creation_time_and_replaces_the_transcript() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::set();

        let session = session_with(vec![Message::user("first")]);
        save(&session, "m").await;
        let first = history::load_session("acp-session-1").await.expect("saved");

        session.messages.lock().push(Message::assistant("second"));
        save(&session, "m").await;
        let second = history::load_session("acp-session-1").await.expect("saved");

        assert_eq!(second.messages.len(), 2);
        assert_eq!(second.created_at, first.created_at);
        assert!(second.updated_at >= first.updated_at);
    }

    #[tokio::test]
    async fn a_forked_session_records_where_it_split() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::set();

        let mut parent = ConversationSession::new("m".to_string());
        parent.id = "parent-session".to_string();
        parent.messages = vec![Message::user("one"), Message::assistant("two")];

        let forked = SessionState::forked(
            agent_client_protocol_schema::SessionId::new("acp-session-1"),
            PathBuf::from("/tmp/claurst-persist-test"),
            &parent,
        );
        save(&forked, "m").await;

        let stored = history::load_session("acp-session-1").await.expect("saved");
        assert_eq!(stored.parent_session_id.as_deref(), Some("parent-session"));
        assert_eq!(stored.fork_point_message_index, Some(2));
        assert_eq!(stored.messages.len(), 2);
    }
}
