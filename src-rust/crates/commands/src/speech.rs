// Speech-mode commands: `/caveman`, `/rocky`, `/normal`.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct CavemanCommand;
pub struct RockyCommand;
pub struct NormalCommand;

// ---- /caveman, /rocky, /normal -------------------------------------------

fn parse_speech_level(args: &str) -> String {
    match args.trim().to_lowercase().as_str() {
        "lite" | "light" => "lite".to_string(),
        "ultra" | "heavy" => "ultra".to_string(),
        "" | "full" | "moderate" | "default" => "full".to_string(),
        other => {
            // Unknown level, default to full
            tracing::warn!(level = other, "Unknown speech level, using full");
            "full".to_string()
        }
    }
}

#[async_trait]
impl SlashCommand for CavemanCommand {
    fn name(&self) -> &str { "caveman" }
    fn description(&self) -> &str { "Caveman speech mode — why use many token when few token do trick" }
    fn help(&self) -> &str {
        "Usage: /caveman [lite|full|ultra]\n\n\
         Activates caveman speech mode that cuts ~75% of output tokens.\n\
         - lite:  Remove pleasantries and hedging only (~40% reduction)\n\
         - full:  Drop articles, compress sentences (default, ~75% reduction)\n\
         - ultra: Maximum compression, imperative phrases only (~85% reduction)\n\n\
         Use /normal to deactivate."
    }
    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let level = parse_speech_level(args);
        CommandResult::SpeechMode { mode: Some("caveman".to_string()), level }
    }
}

#[async_trait]
impl SlashCommand for RockyCommand {
    fn name(&self) -> &str { "rocky" }
    fn description(&self) -> &str { "Rocky speech mode — Eridian alien engineer from Project Hail Mary. Save big token. Good good good." }
    fn help(&self) -> &str {
        "Usage: /rocky [lite|full|ultra]\n\n\
         Speak like Rocky from Project Hail Mary. Saves big token. Amaze amaze amaze.\n\
         - lite:  Grammar rules only, minimal emphasis (~40% reduction)\n\
         - full:  Full Rocky grammar + regular emphasis (default, ~75% reduction)\n\
         - ultra: Maximum Rocky personality, frequent emphasis, alien observations\n\n\
         Use /normal to deactivate."
    }
    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let level = parse_speech_level(args);
        CommandResult::SpeechMode { mode: Some("rocky".to_string()), level }
    }
}

#[async_trait]
impl SlashCommand for NormalCommand {
    fn name(&self) -> &str { "normal" }
    fn description(&self) -> &str { "Deactivate speech mode (caveman/rocky)" }
    fn help(&self) -> &str {
        "Usage: /normal\n\nDeactivate any active speech mode and return to normal output."
    }
    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        CommandResult::SpeechMode { mode: None, level: "full".to_string() }
    }
}
