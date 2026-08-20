//! What `session/list` answers with.
//!
//! The store keeps every session the agent has ever written, terminal ones
//! included, so the list is filtered to what the client asked for and handed
//! over a page at a time. Nothing here reads the disk: it shapes the records
//! it is given, which is what makes the rules testable.

use std::path::Path;

use agent_client_protocol_schema as acp;
use mikmik_core::history::ConversationSession;

/// How many sessions one page carries.
const PAGE_SIZE: usize = 50;

/// One page of the session list.
pub struct Page {
    pub sessions: Vec<acp::SessionInfo>,
    /// The token that fetches the next page, absent on the last one.
    pub next_cursor: Option<String>,
}

/// The page of `stored` that `cursor` points at, filtered by `cwd`.
///
/// `stored` arrives newest first. The cursor is the id of the last session on
/// the previous page rather than an offset, so a session saved in between
/// pages cannot shift the window and hide a record. A cursor whose session is
/// gone is reported rather than silently treated as the beginning, which would
/// hand the client the same page twice.
pub fn page(
    stored: &[ConversationSession],
    cwd: Option<&Path>,
    cursor: Option<&str>,
) -> Result<Page, String> {
    let listable: Vec<&ConversationSession> = stored
        .iter()
        .filter(|s| match (&s.working_dir, cwd) {
            // The protocol requires an absolute directory per session, and a
            // record without one cannot be described at all.
            (None, _) => false,
            (Some(dir), Some(wanted)) => Path::new(dir) == wanted,
            (Some(dir), None) => Path::new(dir).is_absolute(),
        })
        .collect();

    let start = match cursor {
        None => 0,
        Some(cursor) => {
            let at = listable
                .iter()
                .position(|s| s.id == cursor)
                .ok_or_else(|| format!("no session {cursor} in this listing"))?;
            at + 1
        }
    };

    let end = (start + PAGE_SIZE).min(listable.len());
    let sessions: Vec<acp::SessionInfo> = listable[start..end].iter().map(|s| info(s)).collect();
    let next_cursor = (end < listable.len()).then(|| listable[end - 1].id.clone());

    Ok(Page {
        sessions,
        next_cursor,
    })
}

/// One stored session as the client sees it.
fn info(stored: &ConversationSession) -> acp::SessionInfo {
    let cwd = stored.working_dir.clone().unwrap_or_default();
    acp::SessionInfo::new(acp::SessionId::new(stored.id.as_str()), cwd)
        .title(stored.title.clone())
        .updated_at(Some(stored.updated_at.to_rfc3339()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stored(id: &str, dir: Option<&str>) -> ConversationSession {
        let mut session = ConversationSession::new("m".to_string());
        session.id = id.to_string();
        session.working_dir = dir.map(str::to_string);
        session
    }

    #[test]
    fn only_the_sessions_in_the_named_directory_are_listed() {
        let sessions = vec![
            stored("a", Some("/work/one")),
            stored("b", Some("/work/two")),
            stored("c", Some("/work/one")),
        ];

        let page = page(&sessions, Some(&PathBuf::from("/work/one")), None).expect("a page");

        let ids: Vec<&str> = page
            .sessions
            .iter()
            .map(|s| s.session_id.0.as_ref())
            .collect();
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn a_session_with_no_directory_is_left_out_rather_than_given_a_made_up_one() {
        // The protocol requires an absolute cwd per session and the record
        // has none, so there is nothing truthful to report.
        let sessions = vec![stored("a", None), stored("b", Some("/work"))];

        let page = page(&sessions, None, None).expect("a page");

        assert_eq!(page.sessions.len(), 1);
        assert_eq!(page.sessions[0].session_id.0.as_ref(), "b");
    }

    #[test]
    fn a_short_list_fits_in_one_page() {
        let sessions: Vec<ConversationSession> = (0..3)
            .map(|i| stored(&format!("s{i}"), Some("/work")))
            .collect();

        let page = page(&sessions, None, None).expect("a page");

        assert_eq!(page.sessions.len(), 3);
        assert!(page.next_cursor.is_none(), "nothing left to fetch");
    }

    #[test]
    fn a_long_list_is_handed_over_a_page_at_a_time_without_repeating_or_skipping() {
        let sessions: Vec<ConversationSession> = (0..PAGE_SIZE + 10)
            .map(|i| stored(&format!("s{i}"), Some("/work")))
            .collect();

        let first = page(&sessions, None, None).expect("a first page");
        assert_eq!(first.sessions.len(), PAGE_SIZE);
        let cursor = first.next_cursor.clone().expect("more to fetch");

        let second = page(&sessions, None, Some(&cursor)).expect("a second page");
        assert_eq!(second.sessions.len(), 10);
        assert!(second.next_cursor.is_none());

        let seen: Vec<&str> = first
            .sessions
            .iter()
            .chain(second.sessions.iter())
            .map(|s| s.session_id.0.as_ref())
            .collect();
        assert_eq!(seen.len(), PAGE_SIZE + 10);
        let expected: Vec<String> = (0..PAGE_SIZE + 10).map(|i| format!("s{i}")).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_cursor_pointing_at_a_session_that_is_gone_is_reported() {
        // Starting over instead would hand the client the first page again as
        // if it were the second.
        let sessions = vec![stored("a", Some("/work"))];

        let Err(error) = page(&sessions, None, Some("deleted")) else {
            panic!("a stale cursor must be refused");
        };

        assert!(error.contains("deleted"), "{error}");
    }

    #[test]
    fn a_listed_session_carries_its_name_and_when_it_last_moved() {
        let mut session = stored("a", Some("/work"));
        session.title = Some("the parser".to_string());

        let page = page(&[session.clone()], None, None).expect("a page");

        assert_eq!(page.sessions[0].title.as_deref(), Some("the parser"));
        assert_eq!(
            page.sessions[0].updated_at.as_deref(),
            Some(session.updated_at.to_rfc3339().as_str())
        );
    }
}
