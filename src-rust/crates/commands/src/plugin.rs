// Plugin commands: `/plugin`, `/reload-plugins`, and the plugin slash adapter.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct PluginCommand;
pub struct ReloadPluginsCommand;

// ---- /plugin -------------------------------------------------------------

#[async_trait]
impl SlashCommand for PluginCommand {
    fn name(&self) -> &str {
        "plugin"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["plugins"]
    }
    fn description(&self) -> &str {
        "Manage plugins"
    }
    fn help(&self) -> &str {
        "Usage: /plugin [list|info <name>|enable <name>|disable <name>|install <path>|reload]\n\
         Manage Claurst plugins.\n\n\
         Subcommands:\n\
           /plugin              — list all installed plugins\n\
           /plugin list         — list all installed plugins\n\
           /plugin info <name>  — show detailed info about a plugin\n\
           /plugin enable <name>   — enable a plugin (persisted to settings)\n\
           /plugin disable <name>  — disable a plugin (persisted to settings)\n\
           /plugin install <path>  — install a plugin from a local directory\n\
           /plugin reload       — rescan the plugin directories on disk"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        let project_dir = ctx.working_dir.clone();

        // Helper: read the directories every time rather than the registry the
        // session published at startup, so an install or an edit made since
        // then shows up here.
        async fn get_registry(project_dir: &std::path::Path) -> claurst_plugins::PluginRegistry {
            claurst_plugins::load_plugins(project_dir, &[]).await
        }

        let parsed = claurst_plugins::parse_plugin_args(args);
        match parsed {
            claurst_plugins::PluginSubCommand::List => {
                let registry = get_registry(&project_dir).await;
                CommandResult::Message(claurst_plugins::format_plugin_list(&registry))
            }
            claurst_plugins::PluginSubCommand::Enable(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin enable <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Enable(name) => {
                let registry = get_registry(&project_dir).await;
                if registry.get(&name).is_none() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not found. Use `/plugin list` to see installed plugins.",
                        name
                    ));
                }
                let mut settings = claurst_core::config::Settings::load_sync().unwrap_or_default();
                settings.enabled_plugins.insert(name.clone());
                settings.disabled_plugins.remove(&name);
                if let Err(err) = settings.save_sync() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not enabled: settings could not be saved ({}).",
                        name, err
                    ));
                }
                CommandResult::Message(format!(
                    "Plugin '{}' enabled. It loads on the next session.",
                    name
                ))
            }
            claurst_plugins::PluginSubCommand::Disable(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin disable <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Disable(name) => {
                let registry = get_registry(&project_dir).await;
                if registry.get(&name).is_none() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not found. Use `/plugin list` to see installed plugins.",
                        name
                    ));
                }
                let mut settings = claurst_core::config::Settings::load_sync().unwrap_or_default();
                settings.disabled_plugins.insert(name.clone());
                settings.enabled_plugins.remove(&name);
                if let Err(err) = settings.save_sync() {
                    return CommandResult::Error(format!(
                        "Plugin '{}' not disabled: settings could not be saved ({}).",
                        name, err
                    ));
                }
                CommandResult::Message(format!(
                    "Plugin '{}' disabled. Its hooks and MCP servers stop on the next session.",
                    name
                ))
            }
            claurst_plugins::PluginSubCommand::Info(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin info <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Info(name) => {
                let registry = get_registry(&project_dir).await;
                CommandResult::Message(claurst_plugins::format_plugin_info(&registry, &name))
            }
            claurst_plugins::PluginSubCommand::Install(ref path) if path.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin install <path>\nProvide the path to a local plugin directory."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Install(path) => {
                let result = claurst_plugins::install_plugin_from_path(std::path::Path::new(&path));
                match result {
                    Ok(name) => CommandResult::Message(format!(
                        "Plugin '{}' installed. Restart Claurst to activate it.",
                        name
                    )),
                    Err(e) => CommandResult::Error(format!("Install failed: {}", e)),
                }
            }
            claurst_plugins::PluginSubCommand::Reload => {
                // The session's plugin set is fixed at startup, so there is
                // nothing to diff against: report what is on disk now.
                let registry = claurst_plugins::load_plugins(&project_dir, &[]).await;
                CommandResult::Message(claurst_plugins::format_reload_summary(
                    &registry,
                    &claurst_plugins::ReloadDiff::default(),
                ))
            }
            claurst_plugins::PluginSubCommand::Help => CommandResult::Message(
                "Plugin commands:\n\
                     /plugin              — list all installed plugins\n\
                     /plugin list         — list all installed plugins\n\
                     /plugin info <name>  — show plugin details\n\
                     /plugin enable <name>   — enable a plugin\n\
                     /plugin disable <name>  — disable a plugin\n\
                     /plugin install <path>  — install plugin from local path\n\
                     /plugin reload       — rescan the plugin directories on disk"
                    .to_string(),
            ),
        }
    }
}

// ---- /reload-plugins -----------------------------------------------------

#[async_trait]
impl SlashCommand for ReloadPluginsCommand {
    fn name(&self) -> &str {
        "reload-plugins"
    }
    fn description(&self) -> &str {
        "Rescan the plugin directories on disk"
    }
    fn help(&self) -> &str {
        "Usage: /reload-plugins\n\
         Rescans the plugin directories and reports what is installed. \
         The running session keeps the plugins it started with."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let project_dir = ctx.working_dir.clone();
        let registry = claurst_plugins::load_plugins(&project_dir, &[]).await;

        CommandResult::Message(claurst_plugins::format_reload_summary(
            &registry,
            &claurst_plugins::ReloadDiff::default(),
        ))
    }
}

// ---- Plugin slash command adapter ----------------------------------------

/// Wraps a plugin-defined `PluginCommandDef` so it can be executed like a
/// built-in slash command.  The adapter is created on-the-fly inside
/// `execute_command` when no built-in matches the input.
pub struct PluginSlashCommandAdapter {
    pub def: claurst_plugins::PluginCommandDef,
}

#[async_trait]
impl SlashCommand for PluginSlashCommandAdapter {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        // Enforce capability grants before the action runs.
        if let Err(reason) = claurst_plugins::check_plugin_capability(&self.def) {
            return CommandResult::Error(reason);
        }

        match &self.def.run_action {
            claurst_plugins::CommandRunAction::StaticResponse(msg) => {
                CommandResult::Message(msg.clone())
            }
            claurst_plugins::CommandRunAction::MarkdownPrompt {
                file_path,
                plugin_root: _,
            } => {
                // Read the markdown file and inject it into the conversation.
                // A plugin command is written like a skill, so it expands like
                // one: the frontmatter goes, and the placeholders take the
                // arguments. Only a body with no placeholder gets them
                // appended, which is how an author who wrote none still sees
                // what the user typed.
                match std::fs::read_to_string(file_path) {
                    Ok(content) => {
                        let body = claurst_core::strip_frontmatter(&content);
                        let mut words = args.split_whitespace();
                        let arg1 = words.next().unwrap_or("");
                        let arg2 = words.next().unwrap_or("");
                        let has_placeholder = body.contains("$ARGUMENTS")
                            || body.contains("$1")
                            || body.contains("$2");
                        let expanded = body
                            .replace("$ARGUMENTS", args)
                            .replace("$1", arg1)
                            .replace("$2", arg2);
                        let full_prompt = if args.is_empty() || has_placeholder {
                            expanded
                        } else {
                            format!("{}\n\nArguments: {}", expanded, args)
                        };
                        CommandResult::UserMessage(full_prompt)
                    }
                    Err(e) => CommandResult::Error(format!(
                        "Could not read plugin command file '{}': {}",
                        file_path, e
                    )),
                }
            }
            claurst_plugins::CommandRunAction::ShellCommand {
                command,
                plugin_root,
            } => {
                let full_cmd = if args.is_empty() {
                    command.clone()
                } else {
                    format!("{} {}", command, args)
                };
                let cmd_result =
                    std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" })
                        .args(if cfg!(windows) {
                            vec!["/C", &full_cmd]
                        } else {
                            vec!["-c", &full_cmd]
                        })
                        .env("CLAUDE_PLUGIN_ROOT", plugin_root)
                        .output();
                match cmd_result {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        if out.status.success() {
                            CommandResult::Message(stdout.to_string())
                        } else {
                            CommandResult::Error(format!("Command failed:\n{}", stderr))
                        }
                    }
                    Err(e) => CommandResult::Error(format!("Failed to run command: {}", e)),
                }
            }
        }
    }
}
