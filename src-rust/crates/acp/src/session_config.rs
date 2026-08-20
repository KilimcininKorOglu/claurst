//! What a connected client may change about a running session, expressed in
//! the protocol's own vocabulary: modes for how permissions are answered, and
//! configuration options for the model, the account and the reasoning effort.
//!
//! Both mechanisms are part of the stable schema, so a client renders them
//! natively instead of having to know anything about MikMik.

use agent_client_protocol_schema as acp;
use mikmik_core::PermissionMode;

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

/// Configuration option ids. These reach the client, so they are part of the
/// wire contract and not free to rename.
pub const OPTION_MODEL: &str = "model";
pub const OPTION_PROVIDER: &str = "provider";
pub const OPTION_EFFORT: &str = "effort";

/// A model id with one named account's prefix stripped, for catalogue lookup.
///
/// Not `Config::resolve_route`: this answers "how does the catalogue for
/// *this* account spell it", so another account's prefix is deliberately left
/// in place, where it shows the client that the current selection belongs
/// somewhere else. Nothing here reaches the wire.
pub fn model_id_for_account(account: &str, model: &str) -> String {
    model
        .strip_prefix(&format!("{account}/"))
        .unwrap_or(model)
        .to_string()
}

/// The accounts this session can be routed to: everything with a credential,
/// everything configured by hand, and whatever is active right now.
fn available_accounts(config: &mikmik_core::config::Config) -> Vec<String> {
    let mut accounts: Vec<String> = mikmik_core::auth_store::AuthStore::load()
        .credentials
        .keys()
        .cloned()
        .collect();
    accounts.extend(config.provider_configs.keys().cloned());
    accounts.push(config.selected_provider_id().to_string());
    accounts.sort();
    accounts.dedup();
    accounts
}

/// Build a select option, keeping `current` in the list even when the source
/// of the values does not know about it.
///
/// A current value the client cannot see is a selector that shows one thing
/// and offers another.
fn select(
    id: &str,
    name: &str,
    current: &str,
    values: Vec<(String, String)>,
) -> acp::SessionConfigOption {
    let mut values = values;
    if !values.iter().any(|(value_id, _)| value_id == current) {
        values.insert(0, (current.to_string(), current.to_string()));
    }
    let options: Vec<acp::SessionConfigSelectOption> = values
        .into_iter()
        .map(|(value_id, label)| acp::SessionConfigSelectOption::new(value_id, label))
        .collect();
    acp::SessionConfigOption::select(id.to_string(), name, current.to_string(), options)
}

/// Every option a client may set, with the values it may set them to.
///
/// Rebuilt from the session's current configuration on every call, because
/// the model list belongs to the account and the effort ladder belongs to the
/// model: change one and the other two are stale.
///
/// `model` is the id the next turn would send, which is not always
/// `config.model`: with nothing configured the registry resolves one, and
/// reporting the unresolved fallback would name a model the session is not
/// using.
pub fn config_options(
    config: &mikmik_core::config::Config,
    registry: &mikmik_api::ModelRegistry,
    model: &str,
    effort: Option<mikmik_core::effort::EffortLevel>,
) -> Vec<acp::SessionConfigOption> {
    let account = config.selected_provider_id().to_string();
    let vendor = config.vendor_id_for_account(&account);
    let model = model_id_for_account(&account, model);

    let models: Vec<(String, String)> = registry
        .list_visible_by_provider(&vendor)
        .into_iter()
        .map(|entry| (entry.info.id.to_string(), entry.info.name.clone()))
        .collect();

    let accounts: Vec<(String, String)> = available_accounts(config)
        .into_iter()
        .map(|id| (id.clone(), id))
        .collect();

    let current_effort = effort.map(|level| level.as_str()).unwrap_or("medium");
    let efforts: Vec<(String, String)> =
        mikmik_api::effort_support::supported_efforts(&vendor, &model, Some(registry))
            .into_iter()
            .map(|level| (level.as_str().to_string(), level.label().to_string()))
            .collect();

    vec![
        select(OPTION_MODEL, "Model", &model, models)
            .category(Some(acp::SessionConfigOptionCategory::Model)),
        // No category: the spec reserves every name that does not start with
        // `_`, and a client handles a missing one by rendering a plain select.
        select(OPTION_PROVIDER, "Account", &account, accounts),
        select(OPTION_EFFORT, "Effort", current_effort, efforts)
            .category(Some(acp::SessionConfigOptionCategory::ThoughtLevel)),
    ]
}

/// The models a session can be switched to, with the one it is using marked.
///
/// The same set the `model` configuration option offers, in the shape the
/// dedicated model methods use. Both are published: those methods are unstable
/// in the schema, and a client that does not know them still needs a way to
/// pick a model.
pub fn model_state(
    config: &mikmik_core::config::Config,
    registry: &mikmik_api::ModelRegistry,
    model: &str,
) -> acp::SessionModelState {
    let account = config.selected_provider_id().to_string();
    let vendor = config.vendor_id_for_account(&account);
    let current = model_id_for_account(&account, model);

    let mut models: Vec<acp::ModelInfo> = registry
        .list_visible_by_provider(&vendor)
        .into_iter()
        .map(|entry| {
            acp::ModelInfo::new(
                acp::ModelId::new(entry.info.id.to_string()),
                entry.info.name.clone(),
            )
        })
        .collect();
    // A model the catalog has never heard of is still the one answering, and
    // a state whose current model is missing from its own list is a selector
    // that shows one thing and offers another.
    if !models
        .iter()
        .any(|info| info.model_id.0.as_ref() == current)
    {
        models.insert(
            0,
            acp::ModelInfo::new(acp::ModelId::new(current.clone()), current.clone()),
        );
    }

    acp::SessionModelState::new(acp::ModelId::new(current), models)
}

/// Record a client's choice on the session's overrides.
///
/// Returns the ids of the options that could not be honoured, so the caller
/// can refuse the request rather than answering with a list that contradicts
/// what was asked.
pub fn apply_config_option(
    overrides: &mut SessionSettings,
    config: &mikmik_core::config::Config,
    registry: &mikmik_api::ModelRegistry,
    option_id: &str,
    value: &str,
) -> Result<(), String> {
    match option_id {
        OPTION_MODEL => {
            overrides.model = Some(value.to_string());
            Ok(())
        }
        OPTION_PROVIDER => {
            if !available_accounts(config).iter().any(|id| id == value) {
                return Err(format!("no account named \"{value}\""));
            }
            overrides.provider = Some(value.to_string());
            // The old model belongs to the old account. Carrying it over would
            // send an id the new account has never heard of.
            // An account the catalogue does not cover still needs a model,
            // and leaving the override unset would silently keep the old
            // account's one. Canonical, because several of the per-provider
            // fallbacks are slashed ids of their own
            // (`"anthropic/claude-sonnet-4"` for OpenRouter): stored bare,
            // `resolve_route` reads that namespace as an account prefix and
            // sends the session to Anthropic instead of to the account the
            // client just picked.
            let probe = mikmik_core::config::Config {
                provider: Some(value.to_string()),
                provider_configs: config.provider_configs.clone(),
                ..Default::default()
            };
            let route = mikmik_api::resolve_effective_route(&probe, registry);
            overrides.model = Some(probe.canonical_model(&route.account, &route.model));
            Ok(())
        }
        OPTION_EFFORT => match mikmik_core::effort::EffortLevel::from_str(value) {
            Some(level) => {
                overrides.effort = Some(level);
                Ok(())
            }
            None => Err(format!("no effort level named \"{value}\"")),
        },
        other => Err(format!("no configuration option named \"{other}\"")),
    }
}

/// Lay a session's overrides over the runtime's configuration.
///
/// The turn reads the account and the model from this `Config`, so an
/// override that stops here never reaches the request.
pub fn apply_overrides(config: &mut mikmik_core::config::Config, overrides: &SessionSettings) {
    if let Some(mode) = &overrides.permission_mode {
        config.permission_mode = *mode;
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

    /// `MIKMIK_HOME` is process-global, so the tests that redirect it run one
    /// at a time and put it back afterwards. Without it the account list comes
    /// from whatever the developer happens to be logged into.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct HomeGuard {
        saved: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn pointing_at(dir: &std::path::Path) -> Self {
            let saved = std::env::var_os("MIKMIK_HOME");
            std::env::set_var("MIKMIK_HOME", dir);
            Self { saved }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("MIKMIK_HOME", value),
                None => std::env::remove_var("MIKMIK_HOME"),
            }
        }
    }

    fn option_named<'a>(
        options: &'a [acp::SessionConfigOption],
        id: &str,
    ) -> &'a acp::SessionConfigOption {
        options
            .iter()
            .find(|option| option.id.0.as_ref() == id)
            .unwrap_or_else(|| panic!("no option named {id}"))
    }

    fn select_of(option: &acp::SessionConfigOption) -> &acp::SessionConfigSelect {
        match &option.kind {
            acp::SessionConfigKind::Select(select) => select,
            other => panic!("expected a select, got {other:?}"),
        }
    }

    fn values_of(option: &acp::SessionConfigOption) -> Vec<String> {
        match &select_of(option).options {
            acp::SessionConfigSelectOptions::Ungrouped(values) => values
                .iter()
                .map(|value| value.value.0.to_string())
                .collect(),
            other => panic!("expected an ungrouped list, got {other:?}"),
        }
    }

    fn anthropic_config() -> mikmik_core::config::Config {
        mikmik_core::config::Config {
            model: Some("claude-opus-5".to_string()),
            provider: Some("anthropic".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn a_session_offers_a_model_an_account_and_an_effort() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let options = config_options(
            &anthropic_config(),
            &mikmik_api::ModelRegistry::new(),
            "claude-opus-5",
            None,
        );

        let ids: Vec<&str> = options.iter().map(|o| o.id.0.as_ref()).collect();
        assert_eq!(ids, vec![OPTION_MODEL, OPTION_PROVIDER, OPTION_EFFORT]);
        assert_eq!(
            option_named(&options, OPTION_MODEL).category,
            Some(acp::SessionConfigOptionCategory::Model)
        );
        assert_eq!(
            option_named(&options, OPTION_EFFORT).category,
            Some(acp::SessionConfigOptionCategory::ThoughtLevel)
        );
    }

    #[test]
    fn the_current_value_is_always_one_of_the_offered_ones() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        // An empty catalog is the hard case: a model the registry has never
        // heard of must still appear, or the selector shows one value and
        // offers another.
        let options = config_options(
            &anthropic_config(),
            &mikmik_api::ModelRegistry::new(),
            "claude-opus-5",
            None,
        );

        for option in &options {
            let current = select_of(option).current_value.0.to_string();
            assert!(
                values_of(option).contains(&current),
                "{} offers no value for its current {current}",
                option.id.0
            );
        }
    }

    #[test]
    fn the_model_state_marks_the_model_the_turn_would_send() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let state = model_state(
            &anthropic_config(),
            &mikmik_api::ModelRegistry::new(),
            "anthropic/claude-opus-5",
        );

        // The account prefix is not part of the id the catalog is keyed by.
        assert_eq!(state.current_model_id.0.as_ref(), "claude-opus-5");
        // An empty catalog is the hard case: the model in use must still be
        // offered, or the selector shows one thing and offers another.
        assert!(
            state
                .available_models
                .iter()
                .any(|m| m.model_id.0.as_ref() == "claude-opus-5"),
            "the current model is missing from the list it belongs to"
        );
    }

    #[test]
    fn the_two_model_selectors_read_the_same_choice() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        // `session/set_model` and the `model` config option write the same
        // override, so a client showing both cannot see them disagree.
        let mut overrides = SessionSettings::default();
        let config = anthropic_config();
        let registry = mikmik_api::ModelRegistry::new();
        apply_config_option(&mut overrides, &config, &registry, OPTION_MODEL, "gpt-5")
            .expect("a model can be chosen");

        let chosen = overrides.model.clone().expect("a model override");
        let state = model_state(&config, &registry, &chosen);
        let options = config_options(&config, &registry, &chosen, None);
        let option_current = select_of(option_named(&options, OPTION_MODEL))
            .current_value
            .0
            .to_string();

        assert_eq!(state.current_model_id.0.as_ref(), option_current);
    }

    #[test]
    fn an_account_prefix_is_stripped_from_the_model_it_names() {
        // The catalog is keyed by the bare id, so "anthropic/claude-opus-5"
        // would match nothing in it.
        assert_eq!(
            model_id_for_account("anthropic", "anthropic/claude-opus-5"),
            "claude-opus-5"
        );
        assert_eq!(
            model_id_for_account("anthropic", "claude-opus-5"),
            "claude-opus-5"
        );
        // Another account's prefix is not this account's to strip.
        assert_eq!(
            model_id_for_account("openai", "anthropic/claude-opus-5"),
            "anthropic/claude-opus-5"
        );
    }

    #[test]
    fn switching_account_moves_the_model_with_it() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let mut config = anthropic_config();
        config
            .provider_configs
            .insert("openai".to_string(), Default::default());
        let mut overrides = SessionSettings::default();

        apply_config_option(
            &mut overrides,
            &config,
            &mikmik_api::ModelRegistry::new(),
            OPTION_PROVIDER,
            "openai",
        )
        .expect("a configured account can be selected");

        assert_eq!(overrides.provider.as_deref(), Some("openai"));
        // The old model belonged to the old account; carrying it over would
        // send an id the new one has never heard of. An empty catalog is the
        // hard case: leaving the override unset would keep the old model.
        let model = overrides
            .model
            .as_deref()
            .expect("the new account needs a model");
        assert_ne!(model, "claude-opus-5");

        // Stored canonically, so the override names the account it came from
        // even before `apply_overrides` gets to the provider half.
        let mut applied = config.clone();
        applied.model = Some(model.to_string());
        let route = applied.effective_route();
        assert_eq!(route.account, "openai");
        assert!(
            route.model.as_str().starts_with("gpt"),
            "expected an openai model, got {}",
            route.model
        );

        // And the option list still shows the bare catalogue id.
        assert!(model_id_for_account("openai", model).starts_with("gpt"));
    }

    #[test]
    fn an_account_with_no_credential_is_refused() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let mut overrides = SessionSettings::default();
        let error = apply_config_option(
            &mut overrides,
            &anthropic_config(),
            &mikmik_api::ModelRegistry::new(),
            OPTION_PROVIDER,
            "openai",
        )
        .expect_err("an account nobody is logged into cannot serve a turn");

        assert!(error.contains("openai"), "{error}");
        assert_eq!(overrides.provider, None);
    }

    #[test]
    fn an_effort_is_taken_by_name_and_refused_when_unknown() {
        let mut overrides = SessionSettings::default();
        let config = anthropic_config();
        let registry = mikmik_api::ModelRegistry::new();

        apply_config_option(&mut overrides, &config, &registry, OPTION_EFFORT, "xhigh")
            .expect("a known level is accepted");
        assert_eq!(
            overrides.effort,
            Some(mikmik_core::effort::EffortLevel::XHigh)
        );

        apply_config_option(
            &mut overrides,
            &config,
            &registry,
            OPTION_EFFORT,
            "colossal",
        )
        .expect_err("an unknown level is refused");
        assert_eq!(
            overrides.effort,
            Some(mikmik_core::effort::EffortLevel::XHigh),
            "a refused value must not disturb the one already set"
        );
    }

    #[test]
    fn an_option_the_agent_never_offered_is_refused() {
        let mut overrides = SessionSettings::default();
        apply_config_option(
            &mut overrides,
            &anthropic_config(),
            &mikmik_api::ModelRegistry::new(),
            "temperature",
            "0.7",
        )
        .expect_err("only the offered options can be set");
    }

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
            assert_eq!(permission_mode_for(mode_id_for(&mode)), Some(mode));
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
        let mut config = mikmik_core::config::Config {
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
        let mut config = mikmik_core::config::Config {
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
