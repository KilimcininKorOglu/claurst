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
        "Usage: /plugin [list|info <name>|enable <name>|disable <name>|install <source>|update <name>|remove <name>|reload]\n\
         Manage Claurst plugins.\n\n\
         Subcommands:\n\
           /plugin              — list all installed plugins\n\
           /plugin list         — list all installed plugins\n\
           /plugin info <name>  — show detailed info about a plugin\n\
           /plugin enable <name>   — enable a plugin (persisted to settings)\n\
           /plugin disable <name>  — disable a plugin (persisted to settings)\n\
           /plugin install <source> — install from a local directory, an owner/repo\n\
                                      on GitHub, or a git URL\n\
           /plugin update <name>   — pull the latest commit for a plugin from git\n\
           /plugin remove <name>   — delete an installed plugin\n\
           /plugin reload       — reread the plugin directories and apply what changed\n\n\
         Install sources:\n\
           /plugin install ./my-plugin\n\
           /plugin install acme/my-plugin\n\
           /plugin install acme/my-plugin@v1.2.0\n\
           /plugin install https://gitlab.com/acme/my-plugin.git\n\n\
         A repository holding a .claude-plugin/marketplace.json installs every\n\
         plugin it lists."
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
                    "Plugin '{}' enabled. Run /plugin reload to load it into this session.",
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
                    "Plugin '{}' disabled. Run /plugin reload to drop it from this session.",
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
            claurst_plugins::PluginSubCommand::Install(ref source) if source.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin install <source>\n\
                     A source is a local directory, an owner/repo on GitHub \
                     (optionally owner/repo@branch), or a git URL."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Install(source) => {
                let parsed = match claurst_plugins::parse_install_source(&source) {
                    Ok(parsed) => parsed,
                    Err(reason) => return CommandResult::Error(reason),
                };
                let installed = match parsed {
                    claurst_plugins::InstallSource::Local(path) => {
                        claurst_plugins::install_from_local(&path).map(|one| vec![one])
                    }
                    claurst_plugins::InstallSource::Git { url, reference } => {
                        claurst_plugins::install_from_git(&url, reference.as_deref()).await
                    }
                };
                match installed {
                    Ok(plugins) => {
                        let names: Vec<String> = plugins
                            .iter()
                            .map(|p| format!("{} v{}", p.name, p.version))
                            .collect();
                        CommandResult::Message(format!(
                            "Installed {}. Run /plugin reload to activate {}.",
                            names.join(", "),
                            if plugins.len() == 1 { "it" } else { "them" }
                        ))
                    }
                    Err(reason) => CommandResult::Error(format!("Install failed: {}", reason)),
                }
            }
            claurst_plugins::PluginSubCommand::Update(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin update <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Update(name) => {
                match claurst_plugins::update_installed(&name).await {
                    Ok(claurst_plugins::UpdateOutcome::AlreadyCurrent) => {
                        CommandResult::Message(format!("Plugin '{}' is already current.", name))
                    }
                    Ok(claurst_plugins::UpdateOutcome::Updated(range)) => {
                        CommandResult::Message(format!(
                            "Plugin '{}' updated ({}). Run /plugin reload to apply it.",
                            name, range
                        ))
                    }
                    Err(reason) => CommandResult::Error(format!("Update failed: {}", reason)),
                }
            }
            claurst_plugins::PluginSubCommand::Remove(ref name) if name.is_empty() => {
                CommandResult::Error(
                    "Usage: /plugin remove <name>\nRun /plugin list to see installed plugins."
                        .to_string(),
                )
            }
            claurst_plugins::PluginSubCommand::Remove(name) => {
                match claurst_plugins::uninstall(&name) {
                    Ok(path) => CommandResult::Message(format!(
                        "Removed {}. Run /plugin reload to drop it from this session.",
                        path.display()
                    )),
                    Err(reason) => CommandResult::Error(format!("Remove failed: {}", reason)),
                }
            }
            claurst_plugins::PluginSubCommand::Reload => CommandResult::ReloadPlugins,
            claurst_plugins::PluginSubCommand::Help => CommandResult::Message(
                "Plugin commands:\n\
                     /plugin              — list all installed plugins\n\
                     /plugin list         — list all installed plugins\n\
                     /plugin info <name>  — show plugin details\n\
                     /plugin enable <name>   — enable a plugin\n\
                     /plugin disable <name>  — disable a plugin\n\
                     /plugin install <source> — install from a directory, owner/repo, or git URL\n\
                     /plugin update <name>   — pull the latest commit for a plugin from git\n\
                     /plugin remove <name>   — delete an installed plugin\n\
                     /plugin reload       — reread the plugin directories and apply what changed"
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
        "Reload the plugins from disk"
    }
    fn help(&self) -> &str {
        "Usage: /reload-plugins\n\
         Rereads the plugin directories and applies what changed to this \
         session: commands, skills, agents, hooks, output styles and \
         language servers. An MCP server that a plugin added or dropped \
         reconnects with it."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::ReloadPlugins
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
                        .env("CLAUDE_PLUGIN_NAME", &self.def.plugin_name)
                        .envs(claurst_plugins::plugin_config_env(&self.def.plugin_name))
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
