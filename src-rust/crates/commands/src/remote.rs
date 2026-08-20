// Remote-control commands: `/remote-control` (`/rc`) and `/remote-env`.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct RemoteControlCommand;
pub struct RemoteEnvCommand;

// ---- /remote-control (/rc) -----------------------------------------------

#[async_trait]
impl SlashCommand for RemoteControlCommand {
    fn name(&self) -> &str {
        "remote-control"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["rc"]
    }
    fn description(&self) -> &str {
        "Show or manage the remote control (Bridge) connection"
    }
    fn help(&self) -> &str {
        "Usage: /remote-control [start|stop|status]\n\n\
         The bridge lets a phone or browser drive this session through a\n\
         relay you host yourself. See docs/remote-control.md.\n\n\
         Subcommands:\n\
         /remote-control          Show current bridge status and configuration\n\
         /remote-control start    Enable the bridge at startup\n\
         /remote-control stop     Disable the bridge at startup\n\
         /remote-control status   Show bridge status"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let settings = match mikmik_core::config::Settings::load().await {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("Failed to load settings: {}", e)),
        };

        let remote_at_startup = settings.remote_control_at_startup;

        match args.trim() {
            "" | "status" => {
                let hostname = hostname::get()
                    .map(|h| h.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "(unknown host)".to_string());

                let (bridge_url, token_status) = resolved_relay(&settings);

                let startup_status = if remote_at_startup {
                    "enabled at startup"
                } else {
                    "disabled"
                };

                // Active session info from context
                let session_section = if let Some(ref url) = ctx.remote_session_url {
                    format!(
                        "\nActive Session\n\
                         ──────────────\n\
                         Session URL:  {url}\n\
                         Share this URL or QR code with others to let them connect\n\
                         to this MikMik session from the claude.ai web UI.\n",
                        url = url
                    )
                } else {
                    "\nNo active bridge session in this process.\n".to_string()
                };

                // Device fingerprint (first 12 chars are enough for display)
                let fingerprint = mikmik_bridge::device_fingerprint();
                let fp_short = &fingerprint[..fingerprint.len().min(12)];

                let permission_note = permission_note(&ctx.config.permission_mode);

                CommandResult::Message(format!(
                    "Remote Control (Bridge)\n\
                     ═══════════════════════\n\
                     Lets a phone or browser drive this session through a relay you\n\
                     host yourself. The CLI dials out, so this machine needs no\n\
                     inbound port.\n\
                     \n\
                     Local Machine\n\
                     ─────────────\n\
                     Hostname:     {hostname}\n\
                     Device ID:    {fp_short}… (SHA-256 fingerprint)\n\
                     \n\
                     Bridge Configuration\n\
                     ────────────────────\n\
                     Relay:           {bridge_url}\n\
                     Token:           {token_status}\n\
                     Startup mode:    {startup_status}\n\
                     Permissions:     {permission_note}\n\
                     {session_section}\n\
                     How to connect\n\
                     ──────────────\n\
                     1. Run the relay:  cd relay && docker compose up -d\n\
                     2. Put its address and token in settings.json under\n\
                        \"remoteControl\" (the token must be 32 characters or more)\n\
                     3. Enable:  /remote-control start\n\
                     4. Restart MikMik — the bridge connects automatically\n\
                     5. Open the relay in a browser and enter the same token\n\
                     \n\
                     MIKMIK_BRIDGE_URL and MIKMIK_BRIDGE_TOKEN override the\n\
                     settings file when set.\n\
                     \n\
                     Use /remote-control start   to enable bridge at next startup\n\
                     Use /remote-control stop    to disable bridge at startup",
                    hostname = hostname,
                    fp_short = fp_short,
                    bridge_url = bridge_url,
                    token_status = token_status,
                    startup_status = startup_status,
                    permission_note = permission_note,
                    session_section = session_section,
                ))
            }
            "start" => {
                if let Err(e) = save_settings_mutation(|s| s.remote_control_at_startup = true) {
                    return CommandResult::Error(format!("Failed to save settings: {}", e));
                }
                let (bridge_url, token_status) = resolved_relay(&settings);
                CommandResult::Message(format!(
                    "Remote control bridge enabled at startup.\n\
                     Restart MikMik to activate the bridge connection.\n\
                     \n\
                     Relay:  {bridge_url}\n\
                     Token:  {token_status}",
                    bridge_url = bridge_url,
                    token_status = token_status,
                ))
            }
            "stop" => {
                if let Err(e) = save_settings_mutation(|s| s.remote_control_at_startup = false) {
                    return CommandResult::Error(format!("Failed to save settings: {}", e));
                }
                CommandResult::Message(
                    "Remote control bridge disabled.\n\
                     The bridge will not start on next launch."
                        .to_string(),
                )
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand: '{}'\nUsage: /remote-control [start|stop|status]",
                other
            )),
        }
    }
}

/// Describe whether a tool will ask for approval, and who can answer.
///
/// There is no separate remote permission policy: once a tool asks, the answer
/// may come from the keyboard or the remote client. What varies is whether
/// anything asks at all, which the session's own `permission_mode` decides.
fn permission_note(session_mode: &mikmik_core::config::PermissionMode) -> &'static str {
    use mikmik_core::config::PermissionMode;

    match session_mode {
        PermissionMode::BypassPermissions => {
            "no tool asks in bypassPermissions, so the relay token alone runs tools unattended"
        }
        PermissionMode::Plan => "no tool asks in plan mode; writes are refused outright",
        _ => "a tool that asks can be answered from the terminal or the remote client",
    }
}

/// Describe where the bridge will connect and whether it has a usable token.
///
/// Mirrors the CLI's own resolution order so the status screen cannot claim
/// one thing while the bridge does another: an environment override wins, then
/// the `remoteControl` block, then the built-in default.
fn resolved_relay(settings: &mikmik_core::config::Settings) -> (String, String) {
    let env_url = std::env::var("MIKMIK_BRIDGE_URL")
        .or_else(|_| std::env::var("CLAUDE_BRIDGE_BASE_URL"))
        .ok()
        .filter(|url| !url.trim().is_empty());
    let env_token = std::env::var("MIKMIK_BRIDGE_TOKEN")
        .or_else(|_| std::env::var("CLAUDE_BRIDGE_OAUTH_TOKEN"))
        .is_ok();

    let configured = settings.remote_control.as_ref();
    let invalid = configured.and_then(|remote| remote.validate().err());

    let url = match (&env_url, configured) {
        (Some(url), _) => format!("{} (from the environment)", url.trim_end_matches('/')),
        (None, Some(remote)) if invalid.is_none() => {
            format!("{} (from settings.json)", remote.url.trim_end_matches('/'))
        }
        _ => "not configured".to_string(),
    };

    let token = match (env_token, configured, invalid) {
        (true, _, _) => "set in the environment".to_string(),
        (false, Some(_), Some(error)) => format!("unusable: {error}"),
        (false, Some(_), None) => "set in settings.json".to_string(),
        (false, None, _) => "not set (required to connect)".to_string(),
    };

    (url, token)
}

// ---- /remote-env ---------------------------------------------------------

#[async_trait]
impl SlashCommand for RemoteEnvCommand {
    fn name(&self) -> &str {
        "remote-env"
    }
    fn description(&self) -> &str {
        "Show and manage environment variables for remote sessions"
    }
    fn help(&self) -> &str {
        "Usage: /remote-env [set <KEY> <VALUE> | unset <KEY> | list]\n\n\
         Manages env vars stored in config that are forwarded to remote MikMik sessions.\n\
         These are persisted to settings under the 'env' key."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let args = args.trim();

        if args.is_empty() || args == "list" {
            if ctx.config.env.is_empty() {
                return CommandResult::Message(
                    "No remote environment variables configured.\n\
                     Use /remote-env set <KEY> <VALUE> to add one."
                        .to_string(),
                );
            }
            let mut lines = vec!["Remote environment variables:".to_string()];
            let mut keys: Vec<_> = ctx.config.env.keys().collect();
            keys.sort();
            for key in keys {
                let val = &ctx.config.env[key];
                // Mask values that look like secrets
                let display = if key.to_uppercase().contains("KEY")
                    || key.to_uppercase().contains("TOKEN")
                    || key.to_uppercase().contains("SECRET")
                    || key.to_uppercase().contains("PASSWORD")
                {
                    format!("{}***", &val[..val.len().min(4)])
                } else {
                    val.clone()
                };
                lines.push(format!("  {} = {}", key, display));
            }
            return CommandResult::Message(lines.join("\n"));
        }

        let mut parts = args.splitn(3, ' ');
        let sub = parts.next().unwrap_or("").trim();
        let key = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();

        match sub {
            "set" => {
                if key.is_empty() || val.is_empty() {
                    return CommandResult::Error(
                        "Usage: /remote-env set <KEY> <VALUE>".to_string(),
                    );
                }
                let key_owned = key.to_string();
                let val_owned = val.to_string();
                if let Err(e) = save_settings_mutation(|s| {
                    s.config.env.insert(key_owned.clone(), val_owned.clone());
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                let mut new_config = ctx.config.clone();
                new_config.env.insert(key.to_string(), val.to_string());
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Set remote env: {} = {}", key, val),
                )
            }
            "unset" | "remove" | "delete" => {
                if key.is_empty() {
                    return CommandResult::Error("Usage: /remote-env unset <KEY>".to_string());
                }
                if !ctx.config.env.contains_key(key) {
                    return CommandResult::Message(format!("Key '{}' is not set.", key));
                }
                let key_owned = key.to_string();
                if let Err(e) = save_settings_mutation(|s| {
                    s.config.env.remove(&key_owned);
                }) {
                    return CommandResult::Error(format!("Failed to save: {}", e));
                }
                let mut new_config = ctx.config.clone();
                new_config.env.remove(key);
                CommandResult::ConfigChangeMessage(
                    new_config,
                    format!("Removed remote env var: {}", key),
                )
            }
            other => CommandResult::Error(format!(
                "Unknown subcommand: '{}'\nUsage: /remote-env [list|set <K> <V>|unset <K>]",
                other
            )),
        }
    }
}

#[cfg(test)]
mod resolved_relay_tests {
    use super::*;
    use mikmik_core::config::{RemoteControlSettings, Settings};

    /// The environment variables are process-wide, so these tests share a lock
    /// and clear what they set.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_env() {
        for name in [
            "MIKMIK_BRIDGE_URL",
            "CLAUDE_BRIDGE_BASE_URL",
            "MIKMIK_BRIDGE_TOKEN",
            "CLAUDE_BRIDGE_OAUTH_TOKEN",
        ] {
            std::env::remove_var(name);
        }
    }

    fn settings_with(url: &str, token: &str) -> Settings {
        Settings {
            remote_control: Some(RemoteControlSettings {
                url: url.to_string(),
                token: token.to_string(),
                label: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn an_unconfigured_relay_says_so() {
        let _guard = ENV_LOCK.lock();
        clear_env();

        let (url, token) = resolved_relay(&Settings::default());

        assert_eq!(url, "not configured");
        assert!(token.contains("not set"));
    }

    #[test]
    fn the_settings_file_is_reported_when_the_environment_is_quiet() {
        let _guard = ENV_LOCK.lock();
        clear_env();

        let (url, token) = resolved_relay(&settings_with(
            "https://relay.example/",
            &"a".repeat(mikmik_core::config::MIN_REMOTE_TOKEN_LEN),
        ));

        assert_eq!(url, "https://relay.example (from settings.json)");
        assert!(token.contains("settings.json"));
    }

    #[test]
    fn a_short_token_is_reported_as_unusable() {
        let _guard = ENV_LOCK.lock();
        clear_env();

        let (_, token) = resolved_relay(&settings_with("https://relay.example", "short"));

        assert!(
            token.starts_with("unusable:"),
            "the operator has to see why the bridge will not start, got: {token}"
        );
    }

    #[test]
    fn the_environment_wins_over_the_settings_file() {
        let _guard = ENV_LOCK.lock();
        clear_env();
        std::env::set_var("MIKMIK_BRIDGE_URL", "https://dev.example");
        std::env::set_var("MIKMIK_BRIDGE_TOKEN", "whatever");

        let (url, token) = resolved_relay(&settings_with(
            "https://relay.example",
            &"a".repeat(mikmik_core::config::MIN_REMOTE_TOKEN_LEN),
        ));

        clear_env();

        assert_eq!(url, "https://dev.example (from the environment)");
        assert!(token.contains("environment"));
    }
}

#[cfg(test)]
mod permission_note_tests {
    use super::*;
    use mikmik_core::config::PermissionMode;

    #[test]
    fn bypass_permissions_says_the_token_alone_runs_tools() {
        let note = permission_note(&PermissionMode::BypassPermissions);

        assert!(
            note.contains("unattended"),
            "the operator has to see that no approval step remains, got: {note}"
        );
    }

    #[test]
    fn plan_mode_says_nothing_asks() {
        assert!(permission_note(&PermissionMode::Plan).contains("no tool asks"));
    }

    #[test]
    fn a_mode_that_asks_names_both_answering_sides() {
        for mode in [PermissionMode::Default, PermissionMode::AcceptEdits] {
            let note = permission_note(&mode);
            assert!(note.contains("terminal"), "got: {note}");
            assert!(note.contains("remote client"), "got: {note}");
        }
    }
}
