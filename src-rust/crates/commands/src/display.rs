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
    fn description(&self) -> &str {
        "Show context window usage (tokens used / available)"
    }
    fn help(&self) -> &str {
        "Usage: /context\n\n\
         Displays the current context window utilization:\n\
         - Estimated tokens consumed by current conversation\n\
         - Context window limit for the active model\n\
         - Percentage used"
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let model = ctx.config.effective_model();

        // Every currently-supported Claude model family (3.5, opus, sonnet,
        // haiku) shares a 200k-token context window, so this is constant for now.
        let context_window: u64 = 200_000;

        let used_tokens = ctx.cost_tracker.total_tokens();
        let pct = if context_window > 0 {
            (used_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };

        let bar_width = 40usize;
        let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
        let bar: String = "█".repeat(filled) + &"░".repeat(bar_width.saturating_sub(filled));

        // Estimate approximate message tokens from the message list
        let msg_char_count: usize = ctx.messages.iter().map(|m| m.get_all_text().len()).sum();
        // Rough estimate: ~4 chars per token for message text
        let msg_token_estimate = msg_char_count / 4;

        CommandResult::Message(format!(
            "Context Window Usage\n\
             ────────────────────\n\
             Model:          {model}\n\
             Context window: {window:>10} tokens\n\
             API tokens used:{used:>10} tokens  ({pct:.1}%)\n\
             Est. msg size:  {msg:>10} tokens  (approx)\n\
             Messages:       {msgs:>10}\n\n\
             [{bar}] {pct:.1}%\n\n\
             Use /compact to reduce context usage.",
            model = model,
            window = context_window,
            used = used_tokens,
            pct = pct,
            msg = msg_token_estimate,
            msgs = ctx.messages.len(),
            bar = bar,
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
