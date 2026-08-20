// claurst-plugins: Plugin runtime for the Claurst CLI.
//
// This crate handles plugin discovery, manifest parsing, hook registration,
// and the /plugin + /reload-plugins command definitions.
//
// Dependency order: cc-plugins → cc-core only.
// cc-commands → cc-plugins (not the other way around).

pub mod hooks;
pub mod install;
pub mod loader;
pub mod manifest;
pub mod plugin;
pub mod registry;

// Re-export the most commonly used items at the crate root.
pub use hooks::{register_plugin_hooks, HookOutcome, HookRegistry, RegisteredHook};
pub use install::{
    install_from_git, install_from_local, list_installed, parse_install_source, uninstall,
    update_installed, InstallSource, InstalledPlugin, UpdateOutcome,
};
pub use loader::{default_user_plugins_dir, discover_plugins, project_plugins_dir};
pub use manifest::{
    HookEventKind, PluginAuthor, PluginHookEntry, PluginHookMatcher, PluginHooksConfig,
    PluginLspServer, PluginManifest, PluginMcpServer, UserConfigValueType,
};
pub use plugin::{
    CommandRunAction, LoadedPlugin, PluginCommandDef, PluginError, PluginSource, ReloadDiff,
};
pub use registry::PluginRegistry;

// ---------------------------------------------------------------------------
// User configuration
// ---------------------------------------------------------------------------

/// The environment a plugin's own process sees for the options it declares
/// under `userConfig`.
///
/// Two shapes, because a plugin written in a shell script and one written in
/// something that parses JSON want different things:
/// - `CLAUDE_PLUGIN_CONFIG`, the whole object as JSON
/// - `CLAUDE_PLUGIN_CONFIG_<OPTION>` per option, upper-cased, with anything
///   outside `A-Z0-9` replaced by `_`
///
/// A string value is passed through unquoted; anything else is its JSON form,
/// so a boolean reads as `true` rather than `"true"`.
///
/// Returns nothing when the plugin has no values set, which keeps a hook's
/// environment as it was.
pub fn plugin_config_env(plugin_name: &str) -> Vec<(String, String)> {
    let Ok(settings) = mikmik_core::config::Settings::load_sync() else {
        return Vec::new();
    };
    let Some(options) = settings.plugin_config.get(plugin_name) else {
        return Vec::new();
    };
    if options.is_empty() {
        return Vec::new();
    }

    let mut env: Vec<(String, String)> = Vec::new();
    if let Ok(whole) = serde_json::to_string(options) {
        env.push(("CLAUDE_PLUGIN_CONFIG".to_string(), whole));
    }
    for (key, value) in options {
        let name: String = key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        let rendered = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        env.push((format!("CLAUDE_PLUGIN_CONFIG_{name}"), rendered));
    }
    env.sort();
    env
}

// ---------------------------------------------------------------------------
// Capability enforcement
// ---------------------------------------------------------------------------

/// Check whether a plugin command is allowed to execute based on its declared
/// capability grants and the capability the action requires.
///
/// # Policy
/// - If the manifest has **no `capabilities` field** (`None`), the plugin
///   predates capability enforcement and is trusted unconditionally (backwards
///   compatibility with existing plugins).
/// - If the manifest declares an explicit list (even an empty one), the
///   required capability **must** appear in that list.
///
/// Returns `Ok(())` when execution is permitted, or `Err(reason)` when it
/// should be blocked.  The caller should convert `Err` into a `ToolResult::error`.
pub fn check_plugin_capability(def: &PluginCommandDef) -> Result<(), String> {
    // Determine what capability this action needs.
    let required = match def.run_action.required_capability() {
        None => return Ok(()), // StaticResponse — no capability needed.
        Some(cap) => cap,
    };

    match &def.plugin_capabilities {
        // No `capabilities` field — old-style manifest, allow everything.
        None => Ok(()),
        // Explicit capability list — enforce it.
        Some(granted) => {
            if granted.iter().any(|g| g.as_str() == required) {
                Ok(())
            } else {
                Err(format!(
                    "Plugin '{}' is not allowed to use capability '{}'. \
                     Add '{}' to the 'capabilities' list in its manifest to enable this action.",
                    def.plugin_name, required, required
                ))
            }
        }
    }
}

use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Global hook registry (set once at startup, read during tool execution)
// ---------------------------------------------------------------------------

static GLOBAL_HOOK_REGISTRY: parking_lot::RwLock<Option<Arc<HookRegistry>>> =
    parking_lot::RwLock::new(None);

// ---------------------------------------------------------------------------
// Global plugin registry (replaced on every load, read by commands / tools)
// ---------------------------------------------------------------------------

static GLOBAL_PLUGIN_REGISTRY: parking_lot::RwLock<Option<Arc<PluginRegistry>>> =
    parking_lot::RwLock::new(None);

/// Publish the loaded `PluginRegistry` so slash commands and tools can query
/// it without carrying the registry through every call frame.
///
/// A later call replaces the previous registry, which is what makes
/// `/plugin reload` change the running session. A reader that already holds
/// the old `Arc` keeps using it until it drops the handle.
pub fn set_global_registry(registry: PluginRegistry) {
    *GLOBAL_PLUGIN_REGISTRY.write() = Some(Arc::new(registry));
}

/// Access the published `PluginRegistry`, if a session has published one.
pub fn global_plugin_registry() -> Option<Arc<PluginRegistry>> {
    GLOBAL_PLUGIN_REGISTRY.read().clone()
}

/// Publish the hook registry built from the loaded plugins so the tool loop
/// and the `/hooks` browser can reach it. A later call replaces it.
pub fn set_global_hooks(registry: HookRegistry) {
    *GLOBAL_HOOK_REGISTRY.write() = Some(Arc::new(registry));
}

/// Access the hooks the session registered from its plugins, if any.
///
/// The `/hooks` browser reads this so a plugin's hook is visible next to the
/// ones from `settings.json`.
pub fn global_hook_registry() -> Option<Arc<HookRegistry>> {
    GLOBAL_HOOK_REGISTRY.read().clone()
}

/// Run every plugin hook registered for `event`.
///
/// `matcher_target` is what a hook's `matcher` is tested against, which is the
/// tool name for the tool events and `None` for an event that carries no such
/// subject. `payload` is written to each hook's stdin with the event name
/// folded in.
///
/// Returns the first `Deny` a blocking hook produced, so a caller that can
/// stop the operation has one thing to check. A caller that cannot stop
/// anything may ignore the result.
pub async fn run_global_hook(
    event: HookEventKind,
    matcher_target: Option<&str>,
    payload: serde_json::Value,
) -> hooks::HookOutcome {
    let Some(registry) = global_hook_registry() else {
        return hooks::HookOutcome::Allow;
    };

    let event_key = event.to_string();
    let Some(hooks_for_event) = registry.get(&event_key) else {
        return hooks::HookOutcome::Allow;
    };

    let mut event_json = payload;
    if let Some(object) = event_json.as_object_mut() {
        object.insert(
            "event".to_string(),
            serde_json::Value::String(event_key.clone()),
        );
    }
    let event_json = event_json.to_string();

    for hook in hooks_for_event {
        if let Some(target) = matcher_target {
            if !hooks::matcher_selects(hook.matcher.as_deref(), target) {
                continue;
            }
        }
        if let hooks::HookOutcome::Deny(reason) = hooks::run_hook(hook, &event_json).await {
            return hooks::HookOutcome::Deny(reason);
        }
    }

    hooks::HookOutcome::Allow
}

/// Run the `PreToolUse` hooks for one tool call.
pub async fn run_global_pre_tool_hook(
    tool_name: &str,
    tool_input: &serde_json::Value,
) -> hooks::HookOutcome {
    run_global_hook(
        HookEventKind::PreToolUse,
        Some(tool_name),
        serde_json::json!({
            "tool_name": tool_name,
            "tool_input": tool_input,
        }),
    )
    .await
}

/// Run the `PostToolUse` hooks for one tool call, or the
/// `PostToolUseFailure` hooks when the tool returned an error.
pub async fn run_global_post_tool_hook(
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_output: &str,
    is_error: bool,
) {
    let event = if is_error {
        HookEventKind::PostToolUseFailure
    } else {
        HookEventKind::PostToolUse
    };
    run_global_hook(
        event,
        Some(tool_name),
        serde_json::json!({
            "tool_name": tool_name,
            "tool_input": tool_input,
            "tool_output": tool_output,
            "is_error": is_error,
        }),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Top-level async API (called from cc-commands / cc-cli)
// ---------------------------------------------------------------------------

/// Discover and load all plugins from the standard locations.
///
/// Search order:
/// 1. `~/.claurst/plugins/`  (user-global)
/// 2. `<project_dir>/.claurst/plugins/`  (project-local)
/// 3. Any paths listed in `extra_paths`
///
/// Returns a fully populated `PluginRegistry`.  Errors encountered during
/// loading are stored in `registry.errors` rather than propagated, so the
/// caller always gets a usable registry even when individual plugins fail.
pub async fn load_plugins(
    project_dir: &Path,
    extra_paths: &[std::path::PathBuf],
) -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    let mut search_dirs: Vec<std::path::PathBuf> = Vec::new();

    // 1. User-global plugins directory.
    if let Some(user_dir) = default_user_plugins_dir() {
        search_dirs.push(user_dir);
    }

    // 2. Project-local plugins directory.
    search_dirs.push(project_plugins_dir(project_dir));

    // 3. Extra paths (from --plugin-dir or settings).
    search_dirs.extend_from_slice(extra_paths);

    // User plugins.
    if let Some(user_dir) = default_user_plugins_dir() {
        let (plugins, errors) = discover_plugins(&[user_dir], PluginSource::User).await;
        registry.extend(plugins, errors);
    }

    // Project plugins.
    let proj_dir = project_plugins_dir(project_dir);
    let (plugins, errors) = discover_plugins(&[proj_dir], PluginSource::Project).await;
    registry.extend(plugins, errors);

    // Extra paths.
    for path in extra_paths {
        let (plugins, errors) = discover_plugins(
            std::slice::from_ref(path),
            PluginSource::Extra(path.to_string_lossy().into_owned()),
        )
        .await;
        registry.extend(plugins, errors);
    }

    apply_disabled_plugins(&mut registry, &load_disabled_plugin_names());
    registry
}

/// Names the user turned off with `/plugin disable`.
///
/// Read here rather than taken as an argument so no caller can forget it: a
/// plugin that is still enabled contributes commands, hooks and MCP servers,
/// and one of those launches a process.
fn load_disabled_plugin_names() -> std::collections::HashSet<String> {
    mikmik_core::config::Settings::load_sync()
        .map(|settings| settings.disabled_plugins)
        .unwrap_or_default()
}

/// Turn off every discovered plugin the user disabled.
///
/// Discovery enables whatever it finds, so this is what makes `/plugin disable`
/// mean anything. `enabledPlugins` is not consulted: `/plugin enable` writes it
/// and clears the name from `disabledPlugins` in the same step, so the disabled
/// set alone decides.
fn apply_disabled_plugins(
    registry: &mut PluginRegistry,
    disabled: &std::collections::HashSet<String>,
) {
    for name in disabled {
        registry.disable(name);
    }
}

/// Reload plugins: produce a new registry, compute the diff, and replace the old one.
///
/// Returns the new registry and a `ReloadDiff` describing what changed.
pub async fn reload_plugins(
    old_registry: &PluginRegistry,
    project_dir: &Path,
    extra_paths: &[std::path::PathBuf],
) -> (PluginRegistry, ReloadDiff) {
    let new_registry = load_plugins(project_dir, extra_paths).await;
    let diff = new_registry.diff_against(old_registry);
    (new_registry, diff)
}

// ---------------------------------------------------------------------------
// /plugin command definition (data-only, no SlashCommand impl here)
// ---------------------------------------------------------------------------

/// Sub-commands supported by `/plugin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSubCommand {
    /// `/plugin list` — show all installed plugins with enabled/disabled status.
    List,
    /// `/plugin enable <name>` — enable a plugin.
    Enable(String),
    /// `/plugin disable <name>` — disable a plugin.
    Disable(String),
    /// `/plugin info <name>` — show details about a plugin.
    Info(String),
    /// `/plugin install <source>` — install from a local path or a git
    /// repository.
    Install(String),
    /// `/plugin update <name>` — pull the latest commit for a plugin
    /// installed from git.
    Update(String),
    /// `/plugin remove <name>` — delete an installed plugin's directory.
    Remove(String),
    /// `/plugin reload` — reload plugins from disk.
    Reload,
    /// Show usage / help.
    Help,
}

/// Parse the arguments string for `/plugin`.
pub fn parse_plugin_args(args: &str) -> PluginSubCommand {
    let args = args.trim();
    // No args → show list
    if args.is_empty() {
        return PluginSubCommand::List;
    }
    let parts: Vec<&str> = args.splitn(3, char::is_whitespace).collect();
    match parts.first().map(|s| s.to_lowercase()).as_deref() {
        Some("list") | Some("ls") => PluginSubCommand::List,
        Some("enable") => PluginSubCommand::Enable(parts.get(1).unwrap_or(&"").to_string()),
        Some("disable") => PluginSubCommand::Disable(parts.get(1).unwrap_or(&"").to_string()),
        Some("info") | Some("show") => {
            PluginSubCommand::Info(parts.get(1).unwrap_or(&"").to_string())
        }
        Some("install") | Some("i") | Some("add") => {
            PluginSubCommand::Install(parts.get(1).unwrap_or(&"").to_string())
        }
        Some("update") | Some("upgrade") => {
            PluginSubCommand::Update(parts.get(1).unwrap_or(&"").to_string())
        }
        Some("remove") | Some("uninstall") | Some("rm") => {
            PluginSubCommand::Remove(parts.get(1).unwrap_or(&"").to_string())
        }
        Some("reload") | Some("refresh") => PluginSubCommand::Reload,
        Some("help") | Some("--help") | Some("-h") => PluginSubCommand::Help,
        _ => PluginSubCommand::Help,
    }
}

/// Build the text output for `/plugin list`.
pub fn format_plugin_list(registry: &PluginRegistry) -> String {
    let mut out = String::new();
    let mut all: Vec<&LoadedPlugin> = registry.all();
    all.sort_by(|a, b| a.name.cmp(&b.name));

    if all.is_empty() {
        return "No plugins installed.\n\nUse `/plugin install <source>` with a local directory, an \
                owner/repo on GitHub, or a git URL."
            .to_string();
    }

    let total = all.len();
    let enabled_count = all.iter().filter(|p| registry.is_enabled(&p.name)).count();
    out.push_str(&format!(
        "Installed plugins: {} ({} enabled)\n\n",
        total, enabled_count
    ));
    for p in &all {
        let status = if registry.is_enabled(&p.name) {
            "enabled"
        } else {
            "disabled"
        };
        let version = p.manifest.version.as_deref().unwrap_or("(no version)");
        let desc = p.manifest.description.as_deref().unwrap_or("");

        // Count commands and hooks for this plugin.
        let cmd_count = loader::collect_command_defs(p).len();
        let hook_count = p
            .hooks_config
            .as_ref()
            .map(|hc| hc.events.values().map(|v| v.len()).sum::<usize>())
            .unwrap_or(0);

        out.push_str(&format!("  {} [{}] v{}", p.name, status, version));
        if !desc.is_empty() {
            out.push_str(&format!(" — {}", desc));
        }
        let mut extras: Vec<String> = Vec::new();
        if cmd_count > 0 {
            extras.push(format!(
                "{} cmd{}",
                cmd_count,
                if cmd_count == 1 { "" } else { "s" }
            ));
        }
        if hook_count > 0 {
            extras.push(format!(
                "{} hook{}",
                hook_count,
                if hook_count == 1 { "" } else { "s" }
            ));
        }
        if !extras.is_empty() {
            out.push_str(&format!(" ({})", extras.join(", ")));
        }
        out.push('\n');
    }

    if registry.error_count() > 0 {
        out.push_str(&format!(
            "\n{} plugin{} failed to load. Use `/plugin info <name>` for details.\n",
            registry.error_count(),
            if registry.error_count() == 1 { "" } else { "s" }
        ));
    }

    out
}

/// Build the text output for `/plugin info <name>`.
pub fn format_plugin_info(registry: &PluginRegistry, name: &str) -> String {
    match registry.get(name) {
        None => format!(
            "Plugin '{}' not found. Use `/plugin list` to see installed plugins.",
            name
        ),
        Some(p) => {
            let mut out = String::new();
            out.push_str(&format!("Plugin: {}\n", p.name));
            if let Some(v) = &p.manifest.version {
                out.push_str(&format!("Version: {}\n", v));
            }
            if let Some(d) = &p.manifest.description {
                out.push_str(&format!("Description: {}\n", d));
            }
            if let Some(author) = &p.manifest.author {
                out.push_str(&format!("Author: {}\n", author.name));
            }
            out.push_str(&format!(
                "Status: {}\n",
                if registry.is_enabled(name) {
                    "enabled"
                } else {
                    "disabled"
                }
            ));
            out.push_str(&format!("Source: {}\n", p.source_id));
            out.push_str(&format!("Path: {}\n", p.path.display()));

            // Count commands.
            let cmd_defs = loader::collect_command_defs(p);
            if !cmd_defs.is_empty() {
                out.push_str(&format!("\nCommands ({}):\n", cmd_defs.len()));
                for cmd in &cmd_defs {
                    out.push_str(&format!("  /{} — {}\n", cmd.name, cmd.description));
                }
            }

            // Hooks.
            if let Some(ref hooks_config) = p.hooks_config {
                let hook_count: usize = hooks_config.events.values().map(|v| v.len()).sum();
                if hook_count > 0 {
                    out.push_str(&format!("\nHooks ({}):\n", hook_count));
                    for (event, matchers) in &hooks_config.events {
                        for matcher in matchers {
                            for hook in &matcher.hooks {
                                let blocking = if hook.blocking { " [blocking]" } else { "" };
                                out.push_str(&format!(
                                    "  {} {}{}\n",
                                    event, hook.command, blocking
                                ));
                            }
                        }
                    }
                }
            }

            // MCP servers.
            if !p.manifest.mcp_servers.is_empty() {
                out.push_str(&format!(
                    "\nMCP servers ({}):\n",
                    p.manifest.mcp_servers.len()
                ));
                for srv in &p.manifest.mcp_servers {
                    out.push_str(&format!("  {}\n", srv.name));
                }
            }

            // LSP servers.
            if !p.manifest.lsp_servers.is_empty() {
                out.push_str(&format!(
                    "\nLSP servers ({}):\n",
                    p.manifest.lsp_servers.len()
                ));
                for srv in &p.manifest.lsp_servers {
                    out.push_str(&format!("  {}\n", srv.name));
                }
            }

            out
        }
    }
}

// ---------------------------------------------------------------------------
// /reload-plugins summary formatting
// ---------------------------------------------------------------------------

/// Format the result of a plugin reload into a human-readable string,
/// suitable for the `/reload-plugins` command output.
pub fn format_reload_summary(registry: &PluginRegistry, diff: &ReloadDiff) -> String {
    let enabled = registry.enabled_count();
    let total = registry.plugin_count();

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "{} plugin{} loaded ({} enabled)",
        total,
        if total == 1 { "" } else { "s" },
        enabled
    ));

    let cmd_count: usize = registry.all_command_defs().len();
    parts.push(format!(
        "{} command{}",
        cmd_count,
        if cmd_count == 1 { "" } else { "s" }
    ));

    let hook_count: usize = registry
        .build_hook_registry()
        .values()
        .map(|v| v.len())
        .sum();
    parts.push(format!(
        "{} hook{}",
        hook_count,
        if hook_count == 1 { "" } else { "s" }
    ));

    let mcp_count = registry.all_mcp_servers().len();
    parts.push(format!(
        "{} plugin MCP server{}",
        mcp_count,
        if mcp_count == 1 { "" } else { "s" }
    ));

    let lsp_count = registry.all_lsp_servers().len();
    parts.push(format!(
        "{} plugin LSP server{}",
        lsp_count,
        if lsp_count == 1 { "" } else { "s" }
    ));

    let mut msg = format!("Reloaded: {}", parts.join(" · "));

    // One line: the TUI shows this in the status line, which collapses a
    // newline into the previous word.
    if !diff.added.is_empty() {
        msg.push_str(&format!(" · added {}", diff.added.join(", ")));
    }
    if !diff.removed.is_empty() {
        msg.push_str(&format!(" · removed {}", diff.removed.join(", ")));
    }
    if !diff.updated.is_empty() {
        msg.push_str(&format!(" · updated {}", diff.updated.join(", ")));
    }
    if diff.error_count > 0 {
        msg.push_str(&format!(
            " · {} error{} during load",
            diff.error_count,
            if diff.error_count == 1 { "" } else { "s" }
        ));
    }

    msg
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, json: &serde_json::Value) {
        let path = dir.join("plugin.json");
        std::fs::write(path, serde_json::to_vec_pretty(json).unwrap()).unwrap();
    }

    #[test]
    fn parse_plugin_args_list() {
        assert_eq!(parse_plugin_args("list"), PluginSubCommand::List);
        assert_eq!(parse_plugin_args("ls"), PluginSubCommand::List);
    }

    #[test]
    fn parse_plugin_args_enable() {
        assert_eq!(
            parse_plugin_args("enable my-plugin"),
            PluginSubCommand::Enable("my-plugin".to_string())
        );
    }

    #[test]
    fn parse_plugin_args_info() {
        assert_eq!(
            parse_plugin_args("info my-plugin"),
            PluginSubCommand::Info("my-plugin".to_string())
        );
    }

    #[tokio::test]
    async fn load_plugins_empty_dirs() {
        let tmp = TempDir::new().unwrap();
        let reg = load_plugins(tmp.path(), &[]).await;
        assert_eq!(reg.plugin_count(), 0);
        assert_eq!(reg.error_count(), 0);
    }

    #[tokio::test]
    async fn load_plugins_finds_project_plugin() {
        let tmp = TempDir::new().unwrap();
        let plugin_dir = tmp
            .path()
            .join(".claurst")
            .join("plugins")
            .join("test-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_manifest(
            &plugin_dir,
            &serde_json::json!({ "name": "test-plugin", "version": "1.0.0", "description": "A test plugin" }),
        );

        let reg = load_plugins(tmp.path(), &[]).await;
        assert_eq!(reg.plugin_count(), 1);
        assert!(reg.get("test-plugin").is_some());
        assert!(reg.is_enabled("test-plugin"));
    }

    #[test]
    fn manifest_parse_json() {
        let json = serde_json::json!({
            "name": "my-plugin",
            "version": "0.1.0",
            "description": "Test",
            "mcpServers": {
                "my-server": { "command": "node", "args": ["server.js"] }
            }
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let manifest = PluginManifest::from_json(&bytes).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert_eq!(manifest.mcp_servers.len(), 1);
        assert_eq!(manifest.mcp_servers[0].name, "my-server");
    }

    #[test]
    fn format_plugin_list_empty() {
        let reg = PluginRegistry::new();
        let out = format_plugin_list(&reg);
        assert!(out.contains("No plugins installed"));
    }

    #[test]
    fn format_reload_summary_basic() {
        let reg = PluginRegistry::new();
        let diff = ReloadDiff::default();
        let out = format_reload_summary(&reg, &diff);
        assert!(out.contains("Reloaded"));
    }

    /// Build a plugin directory the loader will accept.
    fn write_plugin(root: &std::path::Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("plugin dir");
        std::fs::write(
            dir.join("plugin.json"),
            format!(r#"{{"name": "{name}", "version": "0.1.0"}}"#),
        )
        .expect("manifest");
    }

    #[tokio::test]
    async fn a_disabled_plugin_does_not_load_enabled() {
        let tmp = tempfile::tempdir().expect("tmp");
        let plugins = tmp.path().join(".claurst").join("plugins");
        std::fs::create_dir_all(&plugins).expect("plugins dir");
        write_plugin(&plugins, "alpha");
        write_plugin(&plugins, "beta");

        let mut registry = load_plugins(tmp.path(), &[]).await;
        assert!(registry.is_enabled("alpha"));
        assert!(registry.is_enabled("beta"));

        let disabled = std::collections::HashSet::from(["beta".to_string()]);
        apply_disabled_plugins(&mut registry, &disabled);

        assert!(registry.is_enabled("alpha"));
        assert!(
            !registry.is_enabled("beta"),
            "discovery enables whatever it finds, so /plugin disable means nothing \
             unless the disabled set is applied"
        );
        assert!(
            registry.enabled().iter().all(|p| p.name != "beta"),
            "a disabled plugin must contribute no commands, hooks or MCP servers"
        );
    }

    #[test]
    fn disabling_a_name_that_was_never_found_is_harmless() {
        let mut registry = PluginRegistry::new();
        let disabled = std::collections::HashSet::from(["ghost".to_string()]);

        apply_disabled_plugins(&mut registry, &disabled);

        assert_eq!(registry.enabled_count(), 0);
    }

    /// `CLAURST_HOME` is process-global, so the tests that point it somewhere
    /// cannot run at the same time.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn with_settings(body: &str) -> Self {
            let dir = tempfile::tempdir().expect("tmp home");
            std::fs::write(dir.path().join("settings.json"), body).expect("settings");
            let previous = std::env::var_os("CLAURST_HOME");
            // SAFETY: HOME_LOCK serialises every test that touches this
            // variable, and no other thread reads it meanwhile.
            unsafe { std::env::set_var("CLAURST_HOME", dir.path()) };
            Self {
                previous,
                _dir: dir,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                // SAFETY: same as above.
                Some(value) => unsafe { std::env::set_var("CLAURST_HOME", value) },
                None => unsafe { std::env::remove_var("CLAURST_HOME") },
            }
        }
    }

    #[tokio::test]
    async fn a_plugins_configured_options_reach_its_environment() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::with_settings(
            r#"{"pluginConfig": {"acme": {"apiKey": "k-1", "maxDepth": 3, "verbose": true}}}"#,
        );

        let env = plugin_config_env("acme");
        let lookup = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };

        assert_eq!(
            lookup("CLAUDE_PLUGIN_CONFIG_APIKEY"),
            Some("k-1"),
            "a string is passed through unquoted"
        );
        assert_eq!(lookup("CLAUDE_PLUGIN_CONFIG_MAXDEPTH"), Some("3"));
        assert_eq!(
            lookup("CLAUDE_PLUGIN_CONFIG_VERBOSE"),
            Some("true"),
            "a boolean reads as true, not as a quoted string"
        );

        let whole = lookup("CLAUDE_PLUGIN_CONFIG").expect("the object as JSON");
        let parsed: serde_json::Value = serde_json::from_str(whole).expect("valid JSON");
        assert_eq!(parsed["apiKey"], serde_json::json!("k-1"));
        assert_eq!(parsed["maxDepth"], serde_json::json!(3));

        assert!(
            plugin_config_env("other").is_empty(),
            "a plugin with nothing configured leaves the environment alone"
        );
    }

    #[tokio::test]
    async fn load_plugins_reads_the_disabled_set_from_settings() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::with_settings(r#"{"disabledPlugins": ["beta"]}"#);

        let project = tempfile::tempdir().expect("tmp project");
        let plugins = project.path().join(".claurst").join("plugins");
        std::fs::create_dir_all(&plugins).expect("plugins dir");
        write_plugin(&plugins, "alpha");
        write_plugin(&plugins, "beta");

        let registry = load_plugins(project.path(), &[]).await;

        assert!(registry.is_enabled("alpha"));
        assert!(
            !registry.is_enabled("beta"),
            "/plugin disable writes disabledPlugins, so loading has to read it"
        );
    }
}
