// Display commands: `/context`, `/vim` (`/vi`) and `/timeline`.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct ContextCommand;
pub struct VimCommand;
pub struct TimelineCommand;

// ---- /context ------------------------------------------------------------

#[async_trait]
impl SlashCommand for ContextCommand {
    fn name(&self) -> &str {
        "context"
    }
    fn aliases(&self) -> Vec<&str> {
        // `/ctx-viz` was a second command that estimated the same thing and got
        // every part of it wrong. Its names still reach this one.
        vec!["ctx", "ctx-viz", "context-visualizer"]
    }
    fn description(&self) -> &str {
        "Show context window usage, broken down by category"
    }
    fn help(&self) -> &str {
        "Usage: /context\n\n\
         Reports two figures, because they describe different moments:\n\
         - What the API counted for the last request, against this model's window.\n\
           That figure covers the system prompt and the tool definitions too.\n\
         - An estimate of the current messages, split into conversation,\n\
           tool results and attachments.\n\n\
         Recommends a compaction strategy once the window is filling up."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        use mikmik_query::context_analyzer::{analyze_context, format_context_report};

        let analysis = analyze_context(&ctx.messages);
        CommandResult::Message(format_context_report(
            &analysis,
            ctx.config.effective_model(),
            ctx.context_used_tokens,
            ctx.context_window,
            ctx.messages.len(),
        ))
    }
}

// ---- /vim (/vi) ----------------------------------------------------------

#[async_trait]
impl SlashCommand for VimCommand {
    fn name(&self) -> &str {
        "vim"
    }
    fn aliases(&self) -> Vec<&str> {
        vec!["vi"]
    }
    fn description(&self) -> &str {
        "Toggle vim keybinding mode on/off"
    }
    fn help(&self) -> &str {
        "Usage: /vim [on|off]\n\n\
         Toggles vim keybinding mode in the REPL input.\n\
         When enabled, use Esc to switch between INSERT and NORMAL modes.\n\n\
         The setting is persisted to ~/.config/mikmik/ui-settings.json."
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let current = load_ui_settings();
        let current_mode = current.editor_mode.as_deref().unwrap_or("normal");

        let new_mode = match args.trim() {
            "on" | "vim" => "vim",
            "off" | "normal" => "normal",
            "" => {
                // Toggle
                if current_mode == "vim" {
                    "normal"
                } else {
                    "vim"
                }
            }
            other => {
                return CommandResult::Error(format!(
                    "Unknown argument '{}'. Use: /vim [on|off]",
                    other
                ))
            }
        };

        match mutate_ui_settings(|s| s.editor_mode = Some(new_mode.to_string())) {
            Ok(_) => CommandResult::Message(format!(
                "Editor mode set to {}.\n{}",
                new_mode,
                if new_mode == "vim" {
                    "Use Esc to switch between INSERT and NORMAL modes.\n\
                     Restart the REPL for the change to take effect."
                } else {
                    "Using standard (readline-style) keyboard bindings.\n\
                     Restart the REPL for the change to take effect."
                }
            )),
            Err(e) => CommandResult::Error(format!("Failed to save setting: {}", e)),
        }
    }
}

// ---- /timeline -----------------------------------------------------------

// The terminal owns the panel, so it intercepts this command and acts on the
// parsed argument. The implementation below is what a caller with no terminal
// (headless `--print`, a remote client) gets instead.

#[async_trait]
impl SlashCommand for TimelineCommand {
    fn name(&self) -> &str {
        "timeline"
    }
    fn description(&self) -> &str {
        "Show, hide or clear the live execution timeline panel"
    }
    fn help(&self) -> &str {
        "Usage: /timeline [show|hide|toggle|clear]\n\n\
         The panel lists every tool call and finished turn as it happens, with\n\
         how long each step took and what the turn spent.\n\n\
         Ctrl+Shift+L does the same as /timeline toggle. Once the panel has\n\
         focus, up and down move the cursor, right expands the selected row,\n\
         left collapses it and Esc returns to the prompt.\n\n\
         Recording is off until `timelineEnabled` is turned on in /settings."
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        if let Err(message) = mikmik_core::timeline::parse_timeline_action(args) {
            return CommandResult::Error(message);
        }
        if !ctx.config.timeline_enabled {
            return CommandResult::Message(
                mikmik_core::timeline::TIMELINE_DISABLED_HINT.to_string(),
            );
        }
        CommandResult::Message(
            "The timeline panel is drawn by the terminal UI and is not available here.".to_string(),
        )
    }
}
