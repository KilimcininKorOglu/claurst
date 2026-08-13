// Assorted commands: `/advisor`, `/fast`, `/color` (full).
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;
use claurst_core::advisor_target::parse_advisor_model;
use claurst_core::types::Role;

pub struct AdvisorCommand;
pub struct FastCommand;
pub struct ColorSetCommand;

// ---- /advisor ------------------------------------------------------------

#[async_trait]
impl SlashCommand for AdvisorCommand {
    fn name(&self) -> &str {
        "advisor"
    }
    fn description(&self) -> &str {
        "Set the second model that reviews decisions on request"
    }
    fn help(&self) -> &str {
        "Usage: /advisor [<model>|review|off|unset]\n\n\
         The advisor is a second model that reviews a decision on request.\n\
         The main model can consult it through the Advisor tool, and you can\n\
         run it yourself over the last reply with `review`.\n\n\
         A model can be given as `model`, `provider/model`, or\n\
         `provider:account/model`. The account form runs the advisor on a\n\
         second stored login (see `/accounts`) while the session stays on the\n\
         active one. Only anthropic and codex keep separate accounts.\n\n\
         Examples:\n\
           /advisor claude-opus-4-6           set the advisor model\n\
           /advisor openai/gpt-4o             set a model on another provider\n\
           /advisor anthropic:personal/sonnet run the advisor on another account\n\
           /advisor review                    have the advisor review the last reply\n\
           /advisor off                       disable the advisor\n\
           /advisor                           show the current setting"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let arg = args.trim();

        // Load and save through the typed `Settings` path. A raw JSON rewrite
        // loses every key the struct does not model, and treats a malformed
        // file as empty rather than refusing to overwrite it.
        let mut settings = match claurst_core::config::Settings::load_sync() {
            Ok(settings) => settings,
            Err(e) => {
                return CommandResult::Error(format!(
                    "Could not read settings: {e}. Fix the file, then try again."
                ))
            }
        };

        match arg {
            "" => {
                let current = settings.advisor_model.as_deref().unwrap_or("(not set)");
                CommandResult::Message(format!("Advisor model: {current}"))
            }
            "review" => review_last_reply(ctx).await,
            "off" | "unset" | "none" => {
                settings.advisor_model = None;
                if let Err(e) = settings.save_sync() {
                    return CommandResult::Error(format!("Could not save settings: {e}"));
                }
                let mut config = ctx.config.clone();
                config.advisor_model = None;
                CommandResult::ConfigChangeMessage(config, "Advisor model unset.".to_string())
            }
            model => {
                // The model id itself is not checked: the advisor runs
                // client-side, so it works on every provider and there is no
                // list to validate against. A named account *is* checked,
                // because a typo there would only surface much later as a
                // failed advisor call.
                if let Err(message) =
                    validate_advisor_model(model, ctx.config.selected_provider_id())
                {
                    return CommandResult::Error(message);
                }

                settings.advisor_model = Some(model.to_string());
                if let Err(e) = settings.save_sync() {
                    return CommandResult::Error(format!("Could not save settings: {e}"));
                }
                let mut config = ctx.config.clone();
                config.advisor_model = Some(model.to_string());
                CommandResult::ConfigChangeMessage(
                    config,
                    format!(
                        "Advisor model set to: {model}. \
                         `/advisor review` works now; the model gains the Advisor tool \
                         on the next session."
                    ),
                )
            }
        }
    }
}

/// Reject an advisor model the user could not have meant, before it is saved.
///
/// Only the syntax and any named account are checked. The model id is left
/// alone because the advisor runs client-side against any provider, so there
/// is no authoritative list to compare it with.
fn validate_advisor_model(model: &str, active_provider: &str) -> Result<(), String> {
    let target = parse_advisor_model(model, active_provider)
        .map_err(|e| format!("Could not read '{model}': {e}."))?;

    let Some(profile_id) = target.profile_id else {
        return Ok(());
    };
    let provider_id = target.provider_id;

    if !claurst_core::accounts::provider_supports_profiles(provider_id) {
        return Err(format!(
            "'{provider_id}' stores a single credential, so it has no accounts to choose from. \
             Drop the ':{profile_id}' part."
        ));
    }

    let registry = claurst_core::accounts::AccountRegistry::load();
    if registry.get(provider_id, profile_id).is_some() {
        return Ok(());
    }

    let stored = registry.list(provider_id);
    if stored.is_empty() {
        Err(format!(
            "No '{provider_id}' accounts are stored, so '{profile_id}' cannot be used. \
             Run `/login` to add one."
        ))
    } else {
        let ids: Vec<&str> = stored.iter().map(|profile| profile.id.as_str()).collect();
        Err(format!(
            "There is no '{provider_id}' account named '{profile_id}'. \
             Stored accounts: {}.",
            ids.join(", ")
        ))
    }
}

/// Run the advisor over the most recent assistant reply and show the result.
///
/// This path never reaches the main model: the review is for the user.
async fn review_last_reply(ctx: &CommandContext) -> CommandResult {
    let configured = match ctx.config.advisor_model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => model,
        _ => {
            return CommandResult::Error(
                "No advisor model is set. Run `/advisor <model>` first.".to_string(),
            )
        }
    };

    let last_reply = ctx
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.get_all_text())
        .filter(|text| !text.trim().is_empty());
    let last_reply = match last_reply {
        Some(text) => text,
        None => return CommandResult::Error("There is no assistant reply to review.".to_string()),
    };

    let target = match parse_advisor_model(configured, ctx.config.selected_provider_id()) {
        Ok(target) => target,
        Err(e) => {
            return CommandResult::Error(format!(
                "Could not read the advisor model '{configured}': {e}."
            ))
        }
    };
    let model = target.model;

    let provider = match claurst_api::provider_by_id_for_profile(
        &ctx.config,
        target.provider_id,
        target.profile_id,
    )
    .await
    {
        Ok(provider) => provider,
        Err(e) => return CommandResult::Error(format!("Advisor unavailable: {e}.")),
    };

    let request = claurst_api::ProviderRequest {
        model: model.to_string(),
        messages: vec![Message::user(format!(
            "Review the assistant reply below. Name the most important problem first, \
             then concrete failure cases and wrong assumptions. If it is sound, say so \
             in one line.\n\n---\n\n{last_reply}"
        ))],
        system_prompt: Some(claurst_api::SystemPrompt::Text(
            "You are a senior engineer giving a second opinion. Be a critic, not a \
             cheerleader. Be specific and brief, with no preamble."
                .to_string(),
        )),
        tools: vec![],
        max_tokens: 2048,
        temperature: None,
        top_p: None,
        top_k: None,
        stop_sequences: vec![],
        thinking: None,
        provider_options: serde_json::Value::Object(Default::default()),
    };

    match provider.create_message(request).await {
        Err(e) => CommandResult::Error(format!("Advisor call failed: {e}")),
        Ok(response) => {
            ctx.cost_tracker.add_usage(
                response.usage.input_tokens,
                response.usage.output_tokens,
                response.usage.cache_creation_input_tokens,
                response.usage.cache_read_input_tokens,
            );
            let advice = text_from_content_blocks(&response.content);
            if advice.trim().is_empty() {
                CommandResult::Error(format!("Advisor model '{model}' returned no text."))
            } else {
                CommandResult::Message(format!("Advisor ({model}):\n\n{advice}"))
            }
        }
    }
}

// ---- /fast (/speed) ------------------------------------------------------

#[async_trait]
impl SlashCommand for FastCommand {
    fn name(&self) -> &str {
        "fast"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["speed"]
    }
    fn description(&self) -> &str {
        "Toggle fast mode (uses a faster/cheaper model)"
    }
    fn help(&self) -> &str {
        "Usage: /fast [on|off]\n\n\
         Fast mode switches to the active provider's smaller, faster model\n\
         for quick responses. Toggle without argument to switch.\n\
         The setting is persisted to ~/.claurst/ui-settings.json."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let current = load_ui_settings();
        let currently_on = current.fast_mode.unwrap_or(false);

        let enable = match args.trim() {
            "on" | "enable" | "true" | "1" => true,
            "off" | "disable" | "false" | "0" => false,
            "" => !currently_on,
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /fast [on|off]",
                    other
                ))
            }
        };

        if let Err(e) = mutate_ui_settings(|s| s.fast_mode = Some(enable)) {
            return CommandResult::Error(format!("Failed to save setting: {}", e));
        }

        let provider_id = ctx.config.selected_provider_id();
        let fast_model = resolve_fast_model_id(&ctx.config);
        let normal_model =
            stripped_model_for_provider(provider_id, ctx.config.effective_model()).to_string();

        if enable {
            let mut new_config = ctx.config.clone();
            new_config.model = Some(canonical_model_for_provider(provider_id, &fast_model));
            CommandResult::ConfigChangeMessage(
                new_config,
                format!(
                    "Fast mode ON. Using {} for quicker, cheaper responses.\n\
                     Use /fast off to return to {}.",
                    fast_model, normal_model
                ),
            )
        } else {
            let mut new_config = ctx.config.clone();
            // Restore default / saved model
            new_config.model = None;
            let restored_model =
                stripped_model_for_provider(provider_id, new_config.effective_model()).to_string();
            CommandResult::ConfigChangeMessage(
                new_config,
                format!(
                    "Fast mode OFF. Restored to default model ({}).",
                    restored_model
                ),
            )
        }
    }
}

// ---- /color (full implementation) ----------------------------------------

#[async_trait]
impl SlashCommand for ColorSetCommand {
    fn name(&self) -> &str {
        "color-set"
    }
    fn hidden(&self) -> bool {
        true
    }
    fn description(&self) -> &str {
        "Internal: set prompt color — use /color instead"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let color = args.trim();
        if color.is_empty() {
            let current = load_ui_settings();
            return CommandResult::Message(format!(
                "Current prompt color: {}\n\
                 Use /color <name|#RRGGBB|default> to change it.\n\n\
                 Named colors: red, green, blue, yellow, cyan, magenta, white, orange, purple",
                current.prompt_color.as_deref().unwrap_or("default"),
            ));
        }

        let normalized = if color == "default" {
            None
        } else {
            // Validate hex or named color
            let known_colors = [
                "red", "green", "blue", "yellow", "cyan", "magenta", "white", "orange", "purple",
                "pink", "gray", "grey",
            ];
            let is_hex = color.starts_with('#')
                && (color.len() == 4 || color.len() == 7)
                && color[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !is_hex && !known_colors.contains(&color.to_lowercase().as_str()) {
                return CommandResult::Error(format!(
                    "Unknown color '{}'. Use a color name (red, green, …) or a hex code (#RGB or #RRGGBB).",
                    color
                ));
            }
            Some(color.to_string())
        };

        match mutate_ui_settings(|s| s.prompt_color = normalized.clone()) {
            Ok(_) => CommandResult::Message(format!(
                "Prompt color set to {}.\n\
                 Restart the REPL for the change to take effect.",
                normalized.as_deref().unwrap_or("default")
            )),
            Err(e) => CommandResult::Error(format!("Failed to save color: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::cost::CostTracker;

    /// `Settings::config_dir` reads process-global env, so every test that
    /// repoints it must run serially. CI already passes `--test-threads=1`.
    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().expect("temp dir");
            let previous = std::env::var_os("CLAURST_HOME");
            std::env::set_var("CLAURST_HOME", dir.path());
            Self {
                previous,
                _dir: dir,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("CLAURST_HOME", value),
                None => std::env::remove_var("CLAURST_HOME"),
            }
        }
    }

    fn make_ctx() -> CommandContext {
        CommandContext {
            config: claurst_core::config::Config::default(),
            cost_tracker: CostTracker::new(),
            messages: vec![],
            working_dir: std::path::PathBuf::from("."),
            session_id: "test-session".to_string(),
            session_title: None,
            remote_session_url: None,
            mcp_manager: None,
            mcp_auth_runner: None,
        }
    }

    #[tokio::test]
    async fn setting_the_advisor_keeps_unrelated_settings() {
        let _home = HomeGuard::new();
        let path = claurst_core::config::Settings::global_settings_path();
        std::fs::create_dir_all(path.parent().expect("settings dir")).expect("mkdir");
        std::fs::write(&path, r#"{"showMessageTimestamps":true}"#).expect("seed settings");

        let mut ctx = make_ctx();
        let result = AdvisorCommand.execute("openai/gpt-4o", &mut ctx).await;
        assert!(
            matches!(result, CommandResult::ConfigChangeMessage(_, _)),
            "setting a model should update the live config"
        );

        let saved = claurst_core::config::Settings::load_sync().expect("settings still parse");
        assert_eq!(saved.advisor_model.as_deref(), Some("openai/gpt-4o"));
        assert!(
            saved.show_message_timestamps,
            "an unrelated setting must survive the write"
        );
    }

    #[tokio::test]
    async fn a_malformed_settings_file_is_never_overwritten() {
        let _home = HomeGuard::new();
        let path = claurst_core::config::Settings::global_settings_path();
        std::fs::create_dir_all(path.parent().expect("settings dir")).expect("mkdir");
        let malformed = r#"{"showMessageTimestamps": true,,, }"#;
        std::fs::write(&path, malformed).expect("seed settings");

        let mut ctx = make_ctx();
        let result = AdvisorCommand.execute("claude-opus-4-6", &mut ctx).await;
        assert!(
            matches!(result, CommandResult::Error(_)),
            "a settings file that cannot be parsed must surface an error"
        );

        let on_disk = std::fs::read_to_string(&path).expect("file still there");
        assert_eq!(
            on_disk, malformed,
            "the user's file must be left exactly as it was"
        );
    }

    #[tokio::test]
    async fn an_unknown_account_is_rejected_before_the_setting_is_written() {
        let _home = HomeGuard::new();
        let path = claurst_core::config::Settings::global_settings_path();
        std::fs::create_dir_all(path.parent().expect("settings dir")).expect("mkdir");
        std::fs::write(&path, r#"{"advisorModel":"sonnet"}"#).expect("seed settings");

        let mut ctx = make_ctx();
        let result = AdvisorCommand
            .execute("anthropic:missing/sonnet", &mut ctx)
            .await;

        match result {
            CommandResult::Error(message) => assert!(
                message.contains("missing"),
                "the error should name the account, got {message:?}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
        let saved = claurst_core::config::Settings::load_sync().expect("settings still parse");
        assert_eq!(
            saved.advisor_model.as_deref(),
            Some("sonnet"),
            "a rejected value must not replace the working one"
        );
    }

    #[tokio::test]
    async fn an_account_on_a_single_credential_provider_is_rejected() {
        let _home = HomeGuard::new();
        let mut ctx = make_ctx();

        let result = AdvisorCommand.execute("openai:work/gpt-4o", &mut ctx).await;

        match result {
            CommandResult::Error(message) => assert!(
                message.contains("single credential"),
                "the error should explain why, got {message:?}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_malformed_account_form_is_rejected() {
        let _home = HomeGuard::new();
        let mut ctx = make_ctx();

        let result = AdvisorCommand.execute("anthropic:/sonnet", &mut ctx).await;

        assert!(
            matches!(result, CommandResult::Error(_)),
            "an empty account must not be read as a provider name"
        );
    }

    #[tokio::test]
    async fn a_known_account_is_accepted() {
        let _home = HomeGuard::new();
        let mut registry = claurst_core::accounts::AccountRegistry::load();
        registry
            .upsert(
                claurst_core::accounts::PROVIDER_ANTHROPIC,
                claurst_core::accounts::AccountProfile {
                    id: "personal".to_string(),
                    ..Default::default()
                },
                false,
            )
            .expect("upsert");

        let mut ctx = make_ctx();
        let result = AdvisorCommand
            .execute("anthropic:personal/sonnet", &mut ctx)
            .await;

        assert!(
            matches!(result, CommandResult::ConfigChangeMessage(_, _)),
            "a stored account must be accepted, got {result:?}"
        );
        let saved = claurst_core::config::Settings::load_sync().expect("settings parse");
        assert_eq!(
            saved.advisor_model.as_deref(),
            Some("anthropic:personal/sonnet")
        );
    }

    #[tokio::test]
    async fn review_without_a_configured_model_reports_it() {
        let _home = HomeGuard::new();
        let mut ctx = make_ctx();
        let result = AdvisorCommand.execute("review", &mut ctx).await;
        match result {
            CommandResult::Error(message) => assert!(
                message.contains("/advisor <model>"),
                "the error should point at the fix, got {message:?}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn review_without_an_assistant_reply_reports_it() {
        let _home = HomeGuard::new();
        let mut ctx = make_ctx();
        ctx.config.advisor_model = Some("claude-opus-4-6".to_string());
        let result = AdvisorCommand.execute("review", &mut ctx).await;
        match result {
            CommandResult::Error(message) => assert!(
                message.contains("no assistant reply"),
                "expected the empty-transcript error, got {message:?}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
    }
}
