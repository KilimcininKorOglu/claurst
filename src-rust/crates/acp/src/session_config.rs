//! What a connected client may change about a running session, expressed in
//! the protocol's own vocabulary: modes for how permissions are answered, and
//! configuration options for the model, the account and the reasoning effort.
//!
//! Both mechanisms are part of the stable schema, so a client renders them
//! natively instead of having to know anything about Claurst.

use agent_client_protocol_schema as acp;
use claurst_core::PermissionMode;

use crate::sessions::SessionSettings;

/// Mode ids, in the camelCase the protocol's other agents use.
const MODE_DEFAULT: &str = "default";
const MODE_ACCEPT_EDITS: &str = "acceptEdits";
const MODE_BYPASS_PERMISSIONS: &str = "bypassPermissions";

/// The modes a session can be switched between.
///
/// Plan mode is deliberately absent: it ends with `ExitPlanMode` asking a
/// human to approve a plan, and the protocol has no surface for that, so
/// `AgentRuntime::build` already downgrades it.
pub fn available_modes() -> Vec<acp::SessionMode> {
    vec![
        acp::SessionMode::new(MODE_DEFAULT, "Ask")
            .description(Some("Ask before running anything that writes.".to_string())),
        acp::SessionMode::new(MODE_ACCEPT_EDITS, "Accept edits")
            .description(Some("Apply file edits without asking.".to_string())),
        acp::SessionMode::new(MODE_BYPASS_PERMISSIONS, "Bypass permissions").description(Some(
            "Run every tool without asking. Nothing is confirmed.".to_string(),
        )),
    ]
}

/// The mode id naming a permission mode.
pub fn mode_id_for(mode: &PermissionMode) -> &'static str {
    match mode {
        PermissionMode::AcceptEdits => MODE_ACCEPT_EDITS,
        PermissionMode::BypassPermissions => MODE_BYPASS_PERMISSIONS,
        // A session that starts in plan mode is downgraded before it runs, so
        // reporting it as anything else would misdescribe what happens.
        PermissionMode::Default | PermissionMode::Plan => MODE_DEFAULT,
    }
}

/// The permission mode a client asked for, or `None` if the id is unknown.
pub fn permission_mode_for(mode_id: &str) -> Option<PermissionMode> {
    match mode_id {
        MODE_DEFAULT => Some(PermissionMode::Default),
        MODE_ACCEPT_EDITS => Some(PermissionMode::AcceptEdits),
        MODE_BYPASS_PERMISSIONS => Some(PermissionMode::BypassPermissions),
        _ => None,
    }
}

/// The full mode state for a session currently in `mode`.
pub fn mode_state(mode: &PermissionMode) -> acp::SessionModeState {
    acp::SessionModeState::new(mode_id_for(mode), available_modes())
}

/// Lay a session's overrides over the runtime's configuration.
///
/// The turn reads the account and the model from this `Config`, so an
/// override that stops here never reaches the request.
pub fn apply_overrides(config: &mut claurst_core::config::Config, overrides: &SessionSettings) {
    if let Some(mode) = &overrides.permission_mode {
        config.permission_mode = mode.clone();
    }
    if let Some(model) = &overrides.model {
        config.model = Some(model.clone());
    }
    if let Some(provider) = &overrides.provider {
        config.provider = Some(provider.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_mode_can_be_set() {
        // A client can only send back an id from this list, so an id with no
        // parse arm would be an offer the agent then rejects.
        for mode in available_modes() {
            assert!(
                permission_mode_for(mode.id.0.as_ref()).is_some(),
                "offered mode {} has no parse arm",
                mode.id.0
            );
        }
    }

    #[test]
    fn a_mode_survives_the_round_trip() {
        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
        ] {
            assert_eq!(permission_mode_for(mode_id_for(&mode)), Some(mode.clone()));
        }
    }

    #[test]
    fn plan_mode_reports_as_the_mode_it_is_downgraded_to() {
        // The runtime turns plan into default before the first turn; reporting
        // "plan" would tell the editor something that is not happening.
        assert_eq!(mode_id_for(&PermissionMode::Plan), MODE_DEFAULT);
    }

    #[test]
    fn an_unknown_mode_id_is_refused() {
        assert_eq!(permission_mode_for("architect"), None);
        assert_eq!(permission_mode_for("plan"), None);
    }

    #[test]
    fn a_session_without_overrides_runs_the_runtime_configuration() {
        let mut config = claurst_core::config::Config {
            model: Some("claude-opus-5".to_string()),
            provider: Some("anthropic".to_string()),
            ..Default::default()
        };
        apply_overrides(&mut config, &SessionSettings::default());

        assert_eq!(config.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(config.provider.as_deref(), Some("anthropic"));
        assert_eq!(config.permission_mode, PermissionMode::Default);
    }

    #[test]
    fn an_override_reaches_the_configuration_the_turn_reads() {
        // The account and the model are resolved from this Config, so an
        // override that never lands here changes nothing about the request.
        let mut config = claurst_core::config::Config {
            model: Some("claude-opus-5".to_string()),
            provider: Some("anthropic".to_string()),
            ..Default::default()
        };
        apply_overrides(
            &mut config,
            &SessionSettings {
                permission_mode: Some(PermissionMode::AcceptEdits),
                model: Some("gpt-5".to_string()),
                provider: Some("openai".to_string()),
                effort: None,
            },
        );

        assert_eq!(config.model.as_deref(), Some("gpt-5"));
        assert_eq!(config.provider.as_deref(), Some("openai"));
        assert_eq!(config.permission_mode, PermissionMode::AcceptEdits);
    }

    #[test]
    fn the_state_names_the_mode_the_session_is_in() {
        let state = mode_state(&PermissionMode::AcceptEdits);
        assert_eq!(state.current_mode_id.0.as_ref(), MODE_ACCEPT_EDITS);
        assert_eq!(state.available_modes.len(), 3);
    }
}
