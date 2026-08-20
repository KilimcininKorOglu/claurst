// ConfigTool: get or set Claurst configuration settings at runtime.
//
// Reads from and persists to `settings.json` under the resolved config root.
// Supported settings: model, provider, effort, max_tokens, verbose,
// permission_mode, auto_compact.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

pub struct ConfigTool;

#[derive(Debug, Deserialize)]
struct ConfigInput {
    setting: String,
    value: Option<Value>,
}

static SUPPORTED_SETTINGS: &[(&str, &str)] = &[
    ("model", "LLM model to use (e.g. 'claude-opus-4-6')"),
    (
        "provider",
        "Account the turn is routed to (e.g. 'anthropic', 'openai')",
    ),
    (
        "effort",
        "Reasoning effort: none | minimal | low | medium | high | xhigh | max | ultracode",
    ),
    ("max_tokens", "Maximum output tokens per response"),
    ("verbose", "Enable verbose logging (true/false)"),
    (
        "permission_mode",
        "Permission mode: default | accept_edits | bypass_permissions | plan",
    ),
    (
        "auto_compact",
        "Auto-compact conversation when context fills (true/false)",
    ),
];

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "Config"
    }

    fn description(&self) -> &str {
        "Get or set Claurst configuration settings. Omit 'value' to read the current value. \
         Supported settings: model, provider, effort, max_tokens, verbose, permission_mode, \
         auto_compact. Changes persist to settings.json and apply to the next session."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::Write
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "setting": {
                    "type": "string",
                    "description": "Setting key (e.g. 'model', 'provider', 'effort', 'verbose', 'max_tokens', 'permission_mode')"
                },
                "value": {
                    "description": "New value to set. Omit to read the current value."
                }
            },
            "required": ["setting"]
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> ToolResult {
        let params: ConfigInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let key = params.setting.trim();

        // List all supported settings
        if key == "list" || key == "help" {
            let lines: Vec<String> = SUPPORTED_SETTINGS
                .iter()
                .map(|(k, d)| format!("  {} — {}", k, d))
                .collect();
            return ToolResult::success(format!("Supported settings:\n{}", lines.join("\n")));
        }

        // Load current settings
        let mut settings = match mikmik_core::config::Settings::load().await {
            Ok(s) => s,
            Err(e) => return ToolResult::error(format!("Failed to load settings: {}", e)),
        };

        if let Some(new_value) = params.value {
            // SET operation
            match key {
                "model" => {
                    let s = match new_value.as_str() {
                        Some(s) => s.to_string(),
                        None => return ToolResult::error("'model' must be a string".to_string()),
                    };
                    // Written canonically, and the account written beside it,
                    // the same way `/model` does. Storing the argument
                    // verbatim left `provider` naming the previous account
                    // while the model named a new one.
                    let route = settings.config.resolve_route(&s);
                    let canonical = settings
                        .config
                        .canonical_model(&route.account, &route.model);
                    settings.config.model = Some(canonical.clone());
                    settings.config.provider = Some(route.account.clone());
                    settings.provider = Some(route.account.clone());
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!(
                        "model = \"{}\" on account \"{}\"",
                        route.model, route.account
                    ))
                }
                "provider" => {
                    let s = match new_value.as_str() {
                        Some(s) => s.to_string(),
                        None => {
                            return ToolResult::error("'provider' must be a string".to_string())
                        }
                    };
                    settings.config.provider = Some(s.clone());
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("provider = \"{}\"", s))
                }
                "effort" => {
                    use mikmik_core::effort::EffortLevel;
                    let s = match new_value.as_str() {
                        Some(s) => s,
                        None => return ToolResult::error("'effort' must be a string".to_string()),
                    };
                    // Reject rather than store: an unparseable name would be
                    // written to disk and then silently ignored on every turn.
                    let Some(level) = EffortLevel::from_str(s) else {
                        let valid: Vec<&str> =
                            EffortLevel::ALL.iter().map(EffortLevel::as_str).collect();
                        return ToolResult::error(format!(
                            "Unknown effort '{}'. Use: {}",
                            s,
                            valid.join(" | ")
                        ));
                    };
                    settings.config.effort = Some(level.as_str().to_string());
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("effort = \"{}\"", level.as_str()))
                }
                "max_tokens" => {
                    let n = match new_value.as_u64() {
                        Some(n) => n as u32,
                        None => {
                            return ToolResult::error(
                                "'max_tokens' must be a positive integer".to_string(),
                            )
                        }
                    };
                    settings.config.max_tokens = Some(n);
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("max_tokens = {}", n))
                }
                "verbose" => {
                    let b = match new_value.as_bool() {
                        Some(b) => b,
                        None => {
                            return ToolResult::error("'verbose' must be true or false".to_string())
                        }
                    };
                    settings.config.verbose = b;
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("verbose = {}", b))
                }
                "auto_compact" => {
                    let b = match new_value.as_bool() {
                        Some(b) => b,
                        None => {
                            return ToolResult::error(
                                "'auto_compact' must be true or false".to_string(),
                            )
                        }
                    };
                    settings.config.auto_compact = Some(b);
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("auto_compact = {}", b))
                }
                "permission_mode" => {
                    use mikmik_core::config::PermissionMode;
                    let s = match new_value.as_str() {
                        Some(s) => s,
                        None => {
                            return ToolResult::error(
                                "'permission_mode' must be a string".to_string(),
                            )
                        }
                    };
                    let mode = match s {
                        "default" => PermissionMode::Default,
                        "accept_edits" | "acceptEdits" => PermissionMode::AcceptEdits,
                        "bypass_permissions" | "bypassPermissions" => {
                            PermissionMode::BypassPermissions
                        }
                        "plan" => PermissionMode::Plan,
                        _ => {
                            return ToolResult::error(format!(
                                "Unknown permission_mode '{}'. Use: default | accept_edits | bypass_permissions | plan",
                                s
                            ))
                        }
                    };
                    settings.config.permission_mode = mode;
                    if let Err(e) = settings.save().await {
                        return ToolResult::error(format!("Failed to save settings: {}", e));
                    }
                    ToolResult::success(format!("permission_mode = \"{}\"", s))
                }
                _ => ToolResult::error(format!(
                    "Unknown setting '{}'. Use setting='list' to see all supported settings.",
                    key
                )),
            }
        } else {
            // GET operation
            match key {
                "model" => ToolResult::success(format!(
                    "model = \"{}\"",
                    settings.config.effective_model()
                )),
                "provider" => ToolResult::success(format!(
                    "provider = \"{}\"",
                    settings.config.selected_provider_id()
                )),
                "effort" => match settings.config.effective_effort_level() {
                    Some(level) => ToolResult::success(format!("effort = \"{}\"", level.as_str())),
                    // Not the same as a level named "none": nothing is set, so
                    // the query loop decides.
                    None => ToolResult::success("effort = unset".to_string()),
                },
                "max_tokens" => ToolResult::success(format!(
                    "max_tokens = {}",
                    settings.config.effective_max_tokens()
                )),
                "verbose" => ToolResult::success(format!("verbose = {}", settings.config.verbose)),
                "auto_compact" => ToolResult::success(format!(
                    "auto_compact = {}",
                    settings.effective_auto_compact()
                )),
                "permission_mode" => ToolResult::success(format!(
                    "permission_mode = \"{}\"",
                    permission_mode_str(&settings.config.permission_mode)
                )),
                _ => ToolResult::error(format!(
                    "Unknown setting '{}'. Use setting='list' to see all supported settings.",
                    key
                )),
            }
        }
    }
}

fn permission_mode_str(mode: &mikmik_core::config::PermissionMode) -> &'static str {
    use mikmik_core::config::PermissionMode;
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdits => "accept_edits",
        PermissionMode::BypassPermissions => "bypass_permissions",
        PermissionMode::Plan => "plan",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::allow_all_context;

    /// `MIKMIK_HOME` is process-global, so the tests that redirect it run one
    /// at a time and put it back afterwards.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    async fn run(setting: &str, value: Option<Value>) -> ToolResult {
        let input = match value {
            Some(v) => json!({ "setting": setting, "value": v }),
            None => json!({ "setting": setting }),
        };
        let ctx = allow_all_context(std::env::temp_dir());
        ConfigTool.execute(input, &ctx).await
    }

    #[tokio::test]
    async fn an_effort_survives_the_round_trip_to_disk() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let set = run("effort", Some(json!("high"))).await;
        assert!(!set.is_error, "{}", set.content);
        assert!(set.content.contains("high"), "{}", set.content);

        let read = run("effort", None).await;
        assert!(read.content.contains("high"), "{}", read.content);
    }

    #[tokio::test]
    async fn an_unknown_effort_is_refused_and_names_the_valid_ones() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        // Storing it would write a name to disk that every turn then ignores.
        let result = run("effort", Some(json!("very high"))).await;
        assert!(result.is_error, "{}", result.content);
        assert!(result.content.contains("ultracode"), "{}", result.content);

        let read = run("effort", None).await;
        assert!(read.content.contains("unset"), "{}", read.content);
    }

    #[tokio::test]
    async fn a_provider_survives_the_round_trip_to_disk() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let set = run("provider", Some(json!("openai"))).await;
        assert!(!set.is_error, "{}", set.content);

        let read = run("provider", None).await;
        assert!(read.content.contains("openai"), "{}", read.content);
    }

    #[tokio::test]
    async fn an_unset_provider_reads_as_the_default_account() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("temp home");
        let _guard = HomeGuard::pointing_at(home.path());

        let read = run("provider", None).await;
        assert!(read.content.contains("anthropic"), "{}", read.content);
    }

    #[tokio::test]
    async fn the_listing_names_the_settings_the_tool_accepts() {
        // A setting missing from the list is unreachable: the model reads this
        // to learn what it may write.
        let listed = run("list", None).await;
        for (key, _) in SUPPORTED_SETTINGS {
            assert!(listed.content.contains(key), "{key} missing from the list");
        }
    }
}
