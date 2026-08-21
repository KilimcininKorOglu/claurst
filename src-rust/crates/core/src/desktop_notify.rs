//! Desktop notifications for the moments a session needs the user back.
//!
//! A long turn runs while the user is in another window. The three events
//! below are the points where the session either stops and waits, or has
//! nothing left to do, and the terminal alone gives no sign of it.
//!
//! Delivery is best-effort. A machine with no notification daemon, or a
//! terminal without notification permission, must not stall a turn, so a
//! failed send is logged and dropped rather than propagated.

use crate::config::Settings;
use tracing::debug;

/// Longest body we hand to the notification server.
///
/// A plan is thousands of characters and every backend truncates somewhere
/// of its own accord; cutting here keeps that cut predictable.
const MAX_BODY_CHARS: usize = 180;

/// A moment worth interrupting the user for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent {
    /// The model asked a question and the turn is blocked on the answer.
    QuestionAsked,
    /// A plan is waiting for approval.
    PlanReady,
    /// The turn finished and the prompt is free again.
    TurnComplete,
}

impl NotifyEvent {
    /// The notification title.
    fn summary(self) -> &'static str {
        match self {
            Self::QuestionAsked => "MikMik is waiting on an answer",
            Self::PlanReady => "MikMik has a plan ready",
            Self::TurnComplete => "MikMik finished",
        }
    }

    /// Whether this event's own setting is on.
    fn enabled_in(self, settings: &Settings) -> bool {
        match self {
            Self::QuestionAsked => settings.notify_on_question,
            Self::PlanReady => settings.notify_on_plan_ready,
            Self::TurnComplete => settings.notify_on_turn_complete,
        }
    }
}

/// Whether `event` should reach the desktop under `settings`.
///
/// The master switch wins: with `notifications` off, nothing is sent however
/// the per-event settings read.
pub fn should_notify(settings: &Settings, event: NotifyEvent) -> bool {
    settings.notifications && event.enabled_in(settings)
}

/// Send one notification, if the settings allow it.
///
/// Returns without touching the notification server when the event is
/// switched off, so the caller does not have to ask first.
pub fn notify(settings: &Settings, event: NotifyEvent, body: &str) {
    if !should_notify(settings, event) {
        return;
    }

    let summary = event.summary();
    let body = trim_body(body);

    // Off the caller's thread: `show()` talks to D-Bus or the platform's
    // notification service, and the caller is usually the TUI event loop,
    // where a blocked frame is visible as a stutter.
    std::thread::spawn(move || {
        if let Err(error) = notify_rust::Notification::new()
            .summary(summary)
            .body(&body)
            .show()
        {
            // Not an error the user can act on mid-turn: no daemon, no
            // permission, no session bus. Logged so it is still diagnosable.
            debug!(%error, summary, "desktop notification was not delivered");
        }
    });
}

/// Cut `body` to [`MAX_BODY_CHARS`], on a character boundary, with an ellipsis.
fn trim_body(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= MAX_BODY_CHARS {
        return body.to_string();
    }
    let cut: String = body
        .chars()
        .take(MAX_BODY_CHARS.saturating_sub(1))
        .collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings with every notification switch on.
    fn all_on() -> Settings {
        Settings {
            notifications: true,
            notify_on_question: true,
            notify_on_plan_ready: true,
            notify_on_turn_complete: true,
            ..Default::default()
        }
    }

    const EVENTS: [NotifyEvent; 3] = [
        NotifyEvent::QuestionAsked,
        NotifyEvent::PlanReady,
        NotifyEvent::TurnComplete,
    ];

    #[test]
    fn the_master_switch_silences_every_event() {
        let settings = Settings {
            notifications: false,
            ..all_on()
        };
        for event in EVENTS {
            assert!(
                !should_notify(&settings, event),
                "{event:?} escaped the master switch"
            );
        }
    }

    #[test]
    fn each_event_is_switched_off_on_its_own() {
        for event in EVENTS {
            let mut settings = all_on();
            match event {
                NotifyEvent::QuestionAsked => settings.notify_on_question = false,
                NotifyEvent::PlanReady => settings.notify_on_plan_ready = false,
                NotifyEvent::TurnComplete => settings.notify_on_turn_complete = false,
            }
            assert!(!should_notify(&settings, event), "{event:?} stayed on");
            // The other two are untouched: one switch must not silence a
            // sibling event.
            for other in EVENTS.into_iter().filter(|other| *other != event) {
                assert!(
                    should_notify(&settings, other),
                    "turning off {event:?} also silenced {other:?}"
                );
            }
        }
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary() {
        // Multi-byte on purpose: a byte-wise cut would panic here.
        let body = "ş".repeat(MAX_BODY_CHARS * 2);
        let trimmed = trim_body(&body);

        assert_eq!(trimmed.chars().count(), MAX_BODY_CHARS);
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn a_short_body_is_passed_through_trimmed() {
        assert_eq!(trim_body("  keep me  "), "keep me");
    }
}
