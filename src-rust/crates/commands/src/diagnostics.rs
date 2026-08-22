// Diagnostic commands: `/btw`, `/heapdump`, `/insights`.
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::*;
use async_trait::async_trait;

pub struct BtwCommand;
pub struct HeapdumpCommand;
pub struct InsightsCommand;

// ---- /btw ----------------------------------------------------------------

#[async_trait]
impl SlashCommand for BtwCommand {
    fn name(&self) -> &str {
        "btw"
    }
    fn description(&self) -> &str {
        "Ask a side question without adding it to conversation history"
    }
    fn help(&self) -> &str {
        "Usage: /btw <question>\n\n\
         Submits a background question to the model without it becoming part of\n\
         the main conversation context. The response is shown inline but not\n\
         stored in the message history.\n\n\
         Example:\n\
           /btw what is the capital of France?"
    }

    async fn execute(&self, args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let question = args.trim();
        if question.is_empty() {
            return CommandResult::Error(
                "Usage: /btw <question>  — provide a question after /btw".to_string(),
            );
        }

        // Surface as a special user message tagged as a side-question so the
        // REPL/TUI can handle it as a non-history query. We inject a system tag
        // that tells the backend to answer but not record the exchange.
        CommandResult::UserMessage(format!(
            "[/btw side-question — answer inline, do not store in history]: {}",
            question
        ))
    }
}

// ---- /heapdump -----------------------------------------------------------

#[async_trait]
impl SlashCommand for HeapdumpCommand {
    fn name(&self) -> &str {
        "heapdump"
    }
    fn description(&self) -> &str {
        "Show process memory and diagnostic information"
    }
    fn help(&self) -> &str {
        "Usage: /heapdump\n\n\
         Displays a diagnostic snapshot of the current process:\n\
         process ID, platform, architecture, and available memory info.\n\
         On Linux, reads /proc/self/status for RSS/VmPeak figures.\n\
         On other platforms, reports what is available from the OS."
    }

    async fn execute(&self, _args: &str, _ctx: &mut CommandContext) -> CommandResult {
        let pid = std::process::id();
        let platform = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("  Process ID : {}", pid));
        lines.push(format!("  Platform   : {}", platform));
        lines.push(format!("  Arch       : {}", arch));

        // On Linux, pull memory figures from /proc/self/status
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string("/proc/self/status") {
                Ok(status) => {
                    for line in status.lines() {
                        let key = line.split(':').next().unwrap_or("").trim();
                        if matches!(key, "VmPeak" | "VmRSS" | "VmSize" | "VmData" | "Threads") {
                            let value = line.split(':').nth(1).unwrap_or("").trim();
                            lines.push(format!("  {:10} : {}", key, value));
                        }
                    }
                }
                Err(e) => {
                    lines.push(format!("  (could not read /proc/self/status: {})", e));
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            lines.push("  Memory stats: not available on this platform".to_string());
            lines.push("  (Linux /proc/self/status required for detailed figures)".to_string());
        }

        let body = lines.join("\n");
        CommandResult::Message(format!(
            "Heap Diagnostic\n\
             ─────────────────────────────\n\
             {body}"
        ))
    }
}

// ---- /insights -----------------------------------------------------------

#[async_trait]
impl SlashCommand for InsightsCommand {
    fn name(&self) -> &str {
        "insights"
    }
    fn description(&self) -> &str {
        "Generate a session analysis report with conversation statistics"
    }
    fn help(&self) -> &str {
        "Usage: /insights\n\n\
         Analyses the current conversation and prints a statistics report:\n\
         turn count, token usage, tools invoked, most-used tool, and more."
    }

    async fn execute(&self, _args: &str, ctx: &mut CommandContext) -> CommandResult {
        let messages = &ctx.messages;

        // Count turns (user / assistant pairs)
        let user_turns: usize = messages
            .iter()
            .filter(|m| matches!(m.role, mikmik_core::types::Role::User))
            .count();
        let assistant_turns: usize = messages
            .iter()
            .filter(|m| matches!(m.role, mikmik_core::types::Role::Assistant))
            .count();
        let total_turns = user_turns.min(assistant_turns);

        // Count tool_use blocks and track frequency
        let mut tool_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for msg in messages {
            for block in msg.get_tool_use_blocks() {
                if let mikmik_core::types::ContentBlock::ToolUse { name, .. } = block {
                    *tool_counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
        }
        let total_tool_calls: usize = tool_counts.values().sum();
        let most_frequent_tool = tool_counts
            .iter()
            .max_by_key(|(_, &v)| v)
            .map(|(k, v)| format!("{} ({} calls)", k, v))
            .unwrap_or_else(|| "none".to_string());

        // Token stats from cost_tracker
        let input_tokens = ctx.cost_tracker.input_tokens();
        let output_tokens = ctx.cost_tracker.output_tokens();
        let total_tokens = ctx.cost_tracker.total_tokens();
        let total_cost = ctx.cost_tracker.total_cost_usd();

        let avg_tokens_per_turn = if total_turns > 0 {
            total_tokens / total_turns as u64
        } else {
            0
        };

        CommandResult::Message(format!(
            "Session Insights\n\
             ──────────────────────────────────────\n\
             Conversation\n\
             ├─ User turns          : {user_turns}\n\
             ├─ Assistant turns     : {assistant_turns}\n\
             └─ Completed exchanges : {total_turns}\n\
             \n\
             Tokens\n\
             ├─ Input               : {input_tokens}\n\
             ├─ Output              : {output_tokens}\n\
             ├─ Total               : {total_tokens}\n\
             └─ Avg per exchange    : {avg_tokens_per_turn}\n\
             \n\
             Cost\n\
             └─ Estimated USD       : ${total_cost:.4}\n\
             \n\
             Tools\n\
             ├─ Total calls         : {total_tool_calls}\n\
             └─ Most used           : {most_frequent_tool}",
            user_turns = user_turns,
            assistant_turns = assistant_turns,
            total_turns = total_turns,
            input_tokens = input_tokens,
            output_tokens = output_tokens,
            total_tokens = total_tokens,
            avg_tokens_per_turn = avg_tokens_per_turn,
            total_cost = total_cost,
            total_tool_calls = total_tool_calls,
            most_frequent_tool = most_frequent_tool,
        ))
    }
}
