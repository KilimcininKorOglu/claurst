// Auto-compact service for cc-query.
//
// When the conversation context window fills up (~90%+), we automatically
// summarise older messages to free space. This mirrors the TypeScript
// autoCompact / compact service behaviour.
//
// Strategy:
//   1. Keep as many recent messages as fit a `KEEP_RECENT_TOKENS` budget
//      verbatim (mirrors pi's `keepRecentTokens`), rather than a fixed message
//      COUNT. The cut is snapped to a tool_use↔tool_result-safe round boundary.
//   2. Summarise everything older than that recent tail.
//   3. Replace the head of the conversation with a single synthetic
//      <compact-summary> user message, followed by the recent tail.
//
// The summary is generated in a single non-agentic API call so it doesn't
// trigger another compaction recursively.
//
// MicroCompact strategy (partial compaction):
//   When context is above `trigger_threshold` but not yet at the full
//   auto-compact level, we summarise only the oldest messages while keeping
//   the most recent `keep_recent_messages` intact.  This is lighter than a
//   full compaction and can fire proactively at 75 % capacity.

use claurst_api::{AnthropicStreamEvent, ApiMessage, CreateMessageRequest, StreamAccumulator, StreamHandler, SystemPrompt};
use claurst_core::error::ClaudeError;
use claurst_core::types::{ContentBlock, Message, MessageContent, Role};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Constants (mirrors TypeScript autoCompact.ts)
// ---------------------------------------------------------------------------

/// We target keeping this many context tokens free after compaction.
#[allow(dead_code)]
const AUTOCOMPACT_BUFFER_TOKENS: u64 = 13_000;

/// Start warning when this many tokens remain in the context window.
const WARNING_THRESHOLD_BUFFER_TOKENS: u64 = 20_000;

/// Fraction of the context window at which auto-compact triggers.
const AUTOCOMPACT_TRIGGER_FRACTION: f64 = 0.90;

/// Token budget for the recent tail we preserve verbatim after compaction.
///
/// Instead of keeping a fixed COUNT of recent messages, we keep as many recent
/// messages as fit within this many tokens (mirrors pi's `keepRecentTokens`,
/// which defaults to 20k). Keeping the tail token-budgeted means a handful of
/// huge tool results don't blow the kept context, and many tiny turns aren't
/// prematurely summarised. The cut is always snapped to a
/// tool_use↔tool_result-safe boundary via [`compute_keep_split_index`].
const KEEP_RECENT_TOKENS: u64 = 16_000;

/// Max consecutive auto-compact failures before giving up (circuit breaker).
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

// Percentage thresholds for token warning states (mirrors TS autoCompact.ts)
const WARNING_PCT: f64 = 0.80;   // 80 % full → yellow warning
const CRITICAL_PCT: f64 = 0.95;  // 95 % full → red critical

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Tracks auto-compact state across turns.
#[derive(Debug, Default, Clone)]
pub struct AutoCompactState {
    /// Total compactions performed this session.
    pub compaction_count: u32,
    /// Consecutive failures (reset on success).
    pub consecutive_failures: u32,
    /// Whether the circuit breaker is open (too many failures).
    pub disabled: bool,
}

impl AutoCompactState {
    /// Record a successful compaction.
    pub fn on_success(&mut self) {
        self.compaction_count += 1;
        self.consecutive_failures = 0;
    }

    /// Record a failed compaction; open circuit breaker if too many.
    pub fn on_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            warn!(
                failures = self.consecutive_failures,
                "Auto-compact circuit breaker opened – disabling for this session"
            );
            self.disabled = true;
        }
    }
}

/// Token-usage state relative to the context window.
/// Matches the TypeScript TokenWarningState semantics:
///   Ok      = below 80 % of context window
///   Warning = 80–95 % ("yellow" in TUI)
///   Critical= above 95 % ("red" in TUI)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenWarningState {
    /// Plenty of space left.
    Ok,
    /// Getting close – warn the user (≥ 80 %).
    Warning,
    /// Critical – compact now (≥ 95 %).
    Critical,
}

// ---------------------------------------------------------------------------
// Message grouping (from TypeScript grouping.ts)
// ---------------------------------------------------------------------------

/// A semantically coherent chunk of messages suitable for individual
/// summarisation.  Groups are formed at API-round boundaries: one group per
/// assistant response, which naturally pairs every tool_use with its result.
#[derive(Debug, Clone)]
pub struct MessageGroup {
    pub messages: Vec<Message>,
    /// First file path or tool name mentioned in this group, if any.
    pub topic_hint: Option<String>,
    /// Rough token estimate for the group (chars / 4, padded by 4/3).
    pub token_estimate: usize,
}

impl MessageGroup {
    fn from_messages(messages: Vec<Message>) -> Self {
        let topic_hint = extract_topic_hint(&messages);
        let token_estimate = estimate_tokens_for_messages(&messages);
        Self { messages, topic_hint, token_estimate }
    }
}

/// Extract a short "topic hint" from a group: first file path or tool name
/// mentioned in any tool_use or tool_result block.
fn extract_topic_hint(messages: &[Message]) -> Option<String> {
    for msg in messages {
        let blocks = match &msg.content {
            MessageContent::Blocks(b) => b,
            _ => continue,
        };
        for block in blocks {
            match block {
                ContentBlock::ToolUse { name, input, .. } => {
                    // Try to get a file_path from input, else use tool name
                    if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
                        return Some(fp.to_string());
                    }
                    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                        // Use first word of command as hint
                        let first_word = cmd.split_whitespace().next().unwrap_or(cmd);
                        return Some(first_word.to_string());
                    }
                    return Some(name.clone());
                }
                _ => {}
            }
        }
    }
    None
}

/// Rough token estimate: sum of character lengths divided by 4, padded by 4/3.
fn estimate_tokens_for_messages(messages: &[Message]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|b| estimate_block_chars(b))
                .sum(),
        })
        .sum();
    // chars / 4 = rough tokens, then * 4/3 padding
    (chars / 4) * 4 / 3
}

fn estimate_block_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => {
            name.len() + input.to_string().len()
        }
        ContentBlock::ToolResult { content, .. } => match content {
            claurst_core::types::ToolResultContent::Text(t) => t.len(),
            claurst_core::types::ToolResultContent::Blocks(blocks) => {
                blocks.iter().map(|b| estimate_block_chars(b)).sum()
            }
        },
        ContentBlock::Thinking { thinking, .. } => thinking.len(),
        ContentBlock::RedactedThinking { data } => data.len(),
        _ => 200, // default for images/documents
    }
}

/// Group messages at API-round boundaries: one group per assistant response.
/// This mirrors `groupMessagesByApiRound` from TypeScript grouping.ts.
///
/// Each group represents one complete API round:
///   [user_messages..., assistant_response]
///
/// Boundary detection:
/// - When messages have UUIDs, a new group fires at the START of each new
///   assistant message whose UUID differs from the previous one.
/// - When messages lack UUIDs (local / test messages), boundaries fire
///   when an assistant message follows a PREVIOUS assistant in the current
///   group — i.e. each assistant turn closes its own group.
///
/// The result is that user messages are grouped with the SUBSEQUENT assistant
/// response that replies to them (matching TypeScript round semantics).
pub fn group_messages_for_compact(messages: &[Message]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    let mut current: Vec<Message> = Vec::new();

    for msg in messages {
        if msg.role == Role::Assistant {
            // Add this assistant message to the current group (with any
            // accumulated user messages from this round).
            current.push(msg.clone());

            // Close the group: the next user message(s) belong to the next round.
            groups.push(MessageGroup::from_messages(current.clone()));
            current.clear();
        } else {
            current.push(msg.clone());
        }
    }

    // Any trailing non-assistant messages (shouldn't happen in practice)
    // form their own group.
    if !current.is_empty() {
        groups.push(MessageGroup::from_messages(current));
    }

    groups
}

// ---------------------------------------------------------------------------
// MicroCompact configuration & logic
// ---------------------------------------------------------------------------

/// Configuration for micro-compaction (partial, proactive summarisation).
#[derive(Debug, Clone)]
pub struct MicroCompactConfig {
    /// Compact when context is this fraction full (e.g. 0.75 = 75 %).
    pub trigger_threshold: f32,
    /// Always keep this many recent messages verbatim.
    pub keep_recent_messages: usize,
    /// Target token count for the generated summary.
    pub summary_target_tokens: usize,
}

impl Default for MicroCompactConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: 0.75,
            keep_recent_messages: 10,
            summary_target_tokens: 2048,
        }
    }
}

/// Attempt a micro-compact if the context is above `config.trigger_threshold`.
///
/// Returns `Some(new_messages)` when compaction occurred, `None` otherwise.
pub async fn micro_compact_if_needed(
    client: &claurst_api::AnthropicClient,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    config: &MicroCompactConfig,
) -> Option<Vec<Message>> {
    let window = context_window_for_model(model);
    let pct_used = input_tokens as f64 / window as f64;

    if pct_used < config.trigger_threshold as f64 {
        return None;
    }

    let total = messages.len();
    if total <= config.keep_recent_messages + 1 {
        return None;
    }

    let split_at = total.saturating_sub(config.keep_recent_messages);

    info!(
        input_tokens,
        pct_used = format!("{:.1}%", pct_used * 100.0),
        split_at,
        keep = config.keep_recent_messages,
        "MicroCompact triggered"
    );

    let target_tokens = config.summary_target_tokens as u32;
    match summarise_head(client, messages, split_at, model, target_tokens).await {
        Ok(new_msgs) => {
            info!(
                original = total,
                compacted = new_msgs.len(),
                "MicroCompact complete"
            );
            Some(new_msgs)
        }
        Err(e) => {
            warn!(error = %e, "MicroCompact failed");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Compaction prompt (matches TypeScript prompt.ts)
// ---------------------------------------------------------------------------

/// The critical preamble that prevents the summariser from making tool calls.
const NO_TOOLS_PREAMBLE: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\
\n\
- Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.\n\
- You already have all the context you need in the conversation above.\n\
- Tool calls will be REJECTED and will waste your only turn — you will fail the task.\n\
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\
\n";

/// The trailing reminder that reinforces the no-tools instruction.
const NO_TOOLS_TRAILER: &str = "\n\nREMINDER: Do NOT call any tools. Respond with plain text only — \
an <analysis> block followed by a <summary> block. \
Tool calls will be rejected and you will fail the task.";

/// The base compaction prompt (mirrors BASE_COMPACT_PROMPT from TypeScript prompt.ts).
const BASE_COMPACT_PROMPT: &str = "Your task is to create a detailed summary of the conversation \
so far, paying close attention to the user's explicit requests and your previous actions.\n\
This summary should be thorough in capturing technical details, code patterns, and architectural \
decisions that would be essential for continuing development work without losing context.\n\
\n\
Before providing your final summary, wrap your analysis in <analysis> tags to organize your \
thoughts and ensure you've covered all necessary points. In your analysis process:\n\
\n\
1. Chronologically analyze each message and section of the conversation. For each section \
thoroughly identify:\n\
   - The user's explicit requests and intents\n\
   - Your approach to addressing the user's requests\n\
   - Key decisions, technical concepts and code patterns\n\
   - Specific details like:\n\
     - file names\n\
     - full code snippets\n\
     - function signatures\n\
     - file edits\n\
   - Errors that you ran into and how you fixed them\n\
   - Pay special attention to specific user feedback that you received, especially if the user \
told you to do something differently.\n\
2. Double-check for technical accuracy and completeness, addressing each required element \
thoroughly.\n\
\n\
Your summary should include the following sections:\n\
\n\
1. Primary Request and Intent: Capture all of the user's explicit requests and intents in detail\n\
2. Key Technical Concepts: List all important technical concepts, technologies, and frameworks \
discussed.\n\
3. Files and Code Sections: Enumerate specific files and code sections examined, modified, or \
created. Pay special attention to the most recent messages and include full code snippets where \
applicable and include a summary of why this file read or edit is important.\n\
4. Errors and fixes: List all errors that you ran into, and how you fixed them. Pay special \
attention to specific user feedback that you received, especially if the user told you to do \
something differently.\n\
5. Problem Solving: Document problems solved and any ongoing troubleshooting efforts.\n\
6. All user messages: List ALL user messages that are not tool results. These are critical for \
understanding the users' feedback and changing intent.\n\
7. Pending Tasks: Outline any pending tasks that you have explicitly been asked to work on.\n\
8. Current Work: Describe in detail precisely what was being worked on immediately before this \
summary request, paying special attention to the most recent messages from both user and \
assistant. Include file names and code snippets where applicable.\n\
9. Optional Next Step: List the next step that you will take that is related to the most recent \
work you were doing. IMPORTANT: ensure that this step is DIRECTLY in line with the user's most \
recent explicit requests, and the task you were working on immediately before this summary \
request. If your last task was concluded, then only list next steps if they are explicitly in \
line with the users request. Do not start on tangential requests or really old requests that \
were already completed without confirming with the user first.\n\
                       If there is a next step, include direct quotes from the most recent \
conversation showing exactly what task you were working on and where you left off. This should \
be verbatim to ensure there's no drift in task interpretation.\n\
\n\
Format your output as:\n\
\n\
<analysis>\n\
[Your thought process, ensuring all points are covered thoroughly and accurately]\n\
</analysis>\n\
\n\
<summary>\n\
1. Primary Request and Intent:\n\
   [Detailed description]\n\
\n\
2. Key Technical Concepts:\n\
   - [Concept 1]\n\
   - [Concept 2]\n\
\n\
3. Files and Code Sections:\n\
   - [File Name 1]\n\
      - [Summary of why this file is important]\n\
      - [Summary of the changes made to this file, if any]\n\
      - [Important Code Snippet]\n\
\n\
4. Errors and fixes:\n\
    - [Detailed description of error 1]:\n\
      - [How you fixed the error]\n\
\n\
5. Problem Solving:\n\
   [Description of solved problems and ongoing troubleshooting]\n\
\n\
6. All user messages:\n\
    - [Detailed non tool use user message]\n\
\n\
7. Pending Tasks:\n\
   - [Task 1]\n\
\n\
8. Current Work:\n\
   [Precise description of current work]\n\
\n\
9. Optional Next Step:\n\
   [Optional Next step to take]\n\
</summary>\n\
\n\
Please provide your summary based on the conversation so far, following this structure and \
ensuring precision and thoroughness in your response.";

/// The iterative UPDATE compaction prompt (mirrors UPDATE_SUMMARIZATION_PROMPT
/// from the TypeScript reference). Used when a prior `<compact-summary>` already
/// exists in the history: instead of re-summarising everything from scratch, the
/// model folds the NEW activity into the PREVIOUS summary (provided in
/// `<previous-summary>` tags), preserving the exact same structured sections.
const UPDATE_COMPACT_PROMPT: &str = "Your task is to UPDATE an existing conversation summary by folding in \
the new activity since it was written. The previous summary is provided in <previous-summary> tags; the new \
messages to incorporate are in the <conversation_to_summarize> block.\n\
\n\
Do NOT re-summarise from scratch. Instead:\n\
- PRESERVE all still-relevant information from the previous summary verbatim (file names, code snippets, \
function signatures, decisions, user messages, error fixes).\n\
- ADD new progress, decisions, files, errors, and user messages from the new activity.\n\
- UPDATE the state: move finished items out of Pending Tasks / Current Work; refresh Optional Next Step to \
reflect what is happening NOW.\n\
- You may drop something only if it is clearly no longer relevant.\n\
- Preserve exact file paths, function names, and error messages.\n\
\n\
Before providing your final summary, wrap your reasoning in <analysis> tags: reconcile the previous summary \
with the new messages, note what changed, what completed, and what is now pending.\n\
\n\
Your summary MUST use the SAME sections as before:\n\
\n\
1. Primary Request and Intent: Preserve existing intent; add new requests if the task expanded.\n\
2. Key Technical Concepts: Preserve existing; add newly-introduced concepts.\n\
3. Files and Code Sections: Preserve existing entries; add newly examined/modified/created files with full \
code snippets where applicable and why each matters.\n\
4. Errors and fixes: Preserve existing; add new errors and how they were fixed, plus any user feedback.\n\
5. Problem Solving: Update with newly-solved problems and ongoing troubleshooting.\n\
6. All user messages: Preserve the existing list AND append every new non-tool-result user message.\n\
7. Pending Tasks: Update — remove completed tasks, add newly-requested ones.\n\
8. Current Work: Replace with a precise description of what was being worked on immediately before this \
summary request.\n\
9. Optional Next Step: Update to the next step directly in line with the user's most recent explicit request. \
Include verbatim quotes from the most recent conversation where applicable.\n\
\n\
Format your output as:\n\
\n\
<analysis>\n\
[Reconciliation of the previous summary with the new activity]\n\
</analysis>\n\
\n\
<summary>\n\
1. Primary Request and Intent:\n\
   [Detailed description]\n\
\n\
2. Key Technical Concepts:\n\
   - [Concept 1]\n\
\n\
3. Files and Code Sections:\n\
   - [File Name 1]\n\
      - [Why important]\n\
      - [Changes made, if any]\n\
      - [Important Code Snippet]\n\
\n\
4. Errors and fixes:\n\
    - [Error]: [How fixed]\n\
\n\
5. Problem Solving:\n\
   [Solved problems and ongoing troubleshooting]\n\
\n\
6. All user messages:\n\
    - [Non-tool-use user message]\n\
\n\
7. Pending Tasks:\n\
   - [Task 1]\n\
\n\
8. Current Work:\n\
   [Precise description of current work]\n\
\n\
9. Optional Next Step:\n\
   [Optional next step]\n\
</summary>\n\
\n\
Please provide the UPDATED summary now, following this structure and preserving the previous summary's content.";

/// Build the compaction prompt, optionally with custom instructions appended.
///
/// When `previous_summary` is a non-empty prior summary, the iterative
/// [`UPDATE_COMPACT_PROMPT`] variant is selected so the model folds the previous
/// summary forward rather than re-summarising from scratch. Otherwise the
/// from-scratch [`BASE_COMPACT_PROMPT`] is used.
pub fn get_compact_prompt(
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
) -> String {
    let is_update = previous_summary
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let base = if is_update {
        UPDATE_COMPACT_PROMPT
    } else {
        BASE_COMPACT_PROMPT
    };
    let mut prompt = format!("{}{}", NO_TOOLS_PREAMBLE, base);

    if let Some(instructions) = custom_instructions {
        let trimmed = instructions.trim();
        if !trimmed.is_empty() {
            prompt.push_str(&format!("\n\nAdditional Instructions:\n{}", trimmed));
        }
    }

    prompt.push_str(NO_TOOLS_TRAILER);
    prompt
}

/// Scan a slice of messages for the most recent `<compact-summary>…</compact-summary>`
/// block and return its inner text. This is how a compaction detects that a
/// PRIOR summary already exists in the history (injected by an earlier
/// compaction), so it can fold it forward via the UPDATE prompt instead of
/// re-summarising from zero.
fn extract_previous_summary(messages: &[Message]) -> Option<String> {
    const OPEN: &str = "<compact-summary>";
    const CLOSE: &str = "</compact-summary>";
    // Search newest-first so the most recent summary wins.
    for msg in messages.iter().rev() {
        let text = msg.get_all_text();
        if let (Some(start), Some(end)) = (text.find(OPEN), text.find(CLOSE)) {
            if end > start {
                let inner = text[start + OPEN.len()..end].trim();
                if !inner.is_empty() {
                    return Some(inner.to_string());
                }
            }
        }
    }
    None
}

/// Format the raw compact summary by stripping `<analysis>` and cleaning up
/// `<summary>` XML tags.  Mirrors `formatCompactSummary` from TypeScript
/// prompt.ts.
pub fn format_compact_summary(raw: &str) -> String {
    // Strip <analysis>…</analysis> block (scratchpad, not useful in context)
    let without_analysis = {
        if let (Some(start), Some(end)) = (raw.find("<analysis>"), raw.find("</analysis>")) {
            let before = &raw[..start];
            let after = &raw[end + "</analysis>".len()..];
            format!("{}{}", before, after)
        } else {
            raw.to_string()
        }
    };

    // Extract and reformat <summary>…</summary>
    let formatted = if let (Some(start), Some(end)) = (
        without_analysis.find("<summary>"),
        without_analysis.find("</summary>"),
    ) {
        let before = &without_analysis[..start];
        let content = without_analysis[start + "<summary>".len()..end].trim();
        let after = &without_analysis[end + "</summary>".len()..];
        format!("{}Summary:\n{}{}", before, content, after)
    } else {
        without_analysis
    };

    // Collapse multiple blank lines
    let mut result = String::new();
    let mut blank_count = 0usize;
    for line in formatted.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            result.push_str(line);
            result.push('\n');
        }
    }

    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Threshold helpers
// ---------------------------------------------------------------------------

/// Return the effective context-window size in tokens for the given model.
/// These are approximate; the API enforces the real limits server-side.
///
/// This is a Claude-centric heuristic and only recognises Anthropic models —
/// every other provider collapses to the ~100k default. Prefer
/// [`resolve_context_window`], which consults the models.dev-backed registry
/// first and only falls back to this heuristic.
pub fn context_window_for_model(model: &str) -> u64 {
    if model.contains("opus-4") || model.contains("sonnet-4") || model.contains("haiku-4") {
        200_000
    } else if model.contains("claude-3-5") || model.contains("claude-3.5") {
        200_000
    } else {
        100_000
    }
}

/// Smallest registry context-window value we treat as real.
///
/// When models.dev omits a limit, `ModelRegistry` stores a `4096` placeholder
/// (see `model_registry.rs`). Compacting a live session at ~3.7k tokens would
/// be absurd, so any registry value below this threshold is treated as
/// "unknown" and we fall back to the model-name heuristic instead.
const MIN_PLAUSIBLE_REGISTRY_WINDOW: u64 = 8192;

/// Look up a plausible context-window value in the registry for a given
/// `(provider, model_id)` pair. Returns `None` when there is no entry or the
/// stored window is an implausible placeholder.
fn registry_context_window(
    registry: &claurst_api::ModelRegistry,
    provider: &str,
    model_id: &str,
) -> Option<u64> {
    let window = registry.get(provider, model_id)?.info.context_window as u64;
    (window >= MIN_PLAUSIBLE_REGISTRY_WINDOW).then_some(window)
}

/// Resolve the effective context window for the active provider + model.
///
/// The models.dev-backed [`claurst_api::ModelRegistry`] is the source of truth:
/// it carries real per-model context windows for *every* provider (Gemini/GPT
/// 1M windows, 32k local models, …), so we prefer it. We fall back to the
/// Claude-only [`context_window_for_model`] heuristic only when the registry is
/// absent, has no matching entry, or only holds a placeholder value.
///
/// `model` may be either a bare model id (`"gemini-3-pro"`) or a canonical
/// `"provider/model"` string; both forms are handled.
pub fn resolve_context_window(
    registry: Option<&claurst_api::ModelRegistry>,
    provider: &str,
    model: &str,
) -> u64 {
    if let Some(registry) = registry {
        // The registry is keyed by bare model id, so strip a matching
        // `"<provider>/"` prefix if the caller passed a canonical string.
        let stripped = model
            .strip_prefix(&format!("{}/", provider))
            .unwrap_or(model);
        if let Some(window) = registry_context_window(registry, provider, stripped) {
            return window;
        }
        // Fall back to interpreting the model string itself as
        // `"provider/model"` (e.g. when no explicit provider was supplied).
        if let Some((embedded_provider, embedded_model)) = model.split_once('/') {
            if let Some(window) =
                registry_context_window(registry, embedded_provider, embedded_model)
            {
                return window;
            }
        }
    }
    context_window_for_model(model)
}

/// Best-effort estimate of the CURRENT context size in tokens.
///
/// Prefers the REAL context-token count the provider reported for the last
/// assistant turn (`last_real_usage`, typically `UsageInfo::total_input()` =
/// input + cache-read + cache-creation), because that is what the model
/// actually saw. The chars/4 heuristic can be off by a wide margin, and with
/// prompt caching the bare `input_tokens` field massively *undercounts* — the
/// bulk of the context is billed as cache reads. We fall back to the chars/4
/// estimate ([`estimate_tokens_for_messages`]) only before the first response,
/// or when the provider reported no usage (`None` / `0`).
///
/// Mirrors pi's `estimateContextTokens`, which likewise prefers the last
/// assistant usage and only estimates when it is absent.
pub fn estimate_context_tokens(messages: &[Message], last_real_usage: Option<u64>) -> u64 {
    match last_real_usage {
        Some(tokens) if tokens > 0 => tokens,
        _ => estimate_tokens_for_messages(messages) as u64,
    }
}

/// Determine token-warning state given current input token count and model.
///
/// Convenience wrapper that derives the window from the model-name heuristic.
/// Prefer [`calculate_token_warning_state_for_window`] with a window resolved
/// via [`resolve_context_window`] so non-Claude providers size correctly.
pub fn calculate_token_warning_state(input_tokens: u64, model: &str) -> TokenWarningState {
    calculate_token_warning_state_for_window(input_tokens, context_window_for_model(model))
}

/// Determine token-warning state against an explicit context window.
///
/// Thresholds (mirrors TypeScript autoCompact.ts):
///   ≥ 95 % → Critical (red warning)
///   ≥ 80 % → Warning  (yellow warning)
///   <  80 % → Ok
pub fn calculate_token_warning_state_for_window(
    input_tokens: u64,
    window: u64,
) -> TokenWarningState {
    let pct = input_tokens as f64 / window as f64;

    if pct >= CRITICAL_PCT {
        TokenWarningState::Critical
    } else if pct >= WARNING_PCT || window.saturating_sub(input_tokens) <= WARNING_THRESHOLD_BUFFER_TOKENS {
        TokenWarningState::Warning
    } else {
        TokenWarningState::Ok
    }
}

/// Return `true` when auto-compaction should fire.
///
/// Convenience wrapper that derives the window from the model-name heuristic.
/// Prefer [`should_auto_compact_for_window`] with a resolved window.
pub fn should_auto_compact(input_tokens: u64, model: &str, state: &AutoCompactState) -> bool {
    should_auto_compact_for_window(input_tokens, context_window_for_model(model), state)
}

/// Return `true` when auto-compaction should fire, against an explicit window.
pub fn should_auto_compact_for_window(
    input_tokens: u64,
    window: u64,
    state: &AutoCompactState,
) -> bool {
    if state.disabled {
        return false;
    }
    let threshold = (window as f64 * AUTOCOMPACT_TRIGGER_FRACTION) as u64;
    input_tokens >= threshold
}

// ---------------------------------------------------------------------------
// Core compaction logic
// ---------------------------------------------------------------------------

/// Summarise `messages[..split_at]` using the Anthropic API using the
/// carefully crafted compaction prompt from TypeScript prompt.ts.
/// Returns a new conversation: [summary user msg] + messages[split_at..].
async fn summarise_head(
    client: &claurst_api::AnthropicClient,
    messages: &[Message],
    split_at: usize,
    model: &str,
    max_summary_tokens: u32,
) -> Result<Vec<Message>, ClaudeError> {
    if split_at == 0 {
        return Ok(messages.to_vec());
    }

    let head = &messages[..split_at];

    // Iterative UPDATE mode: if a prior <compact-summary> already lives in the
    // head, fold it forward instead of re-summarising from scratch. Keep the
    // full previous summary (used later for the files-touched manifest) and a
    // manifest-stripped copy for the prompt so the model doesn't echo it.
    let previous_summary = extract_previous_summary(head);

    // Build a transcript string for the summarisation prompt.
    let mut transcript = String::new();
    let original_count = head.len();
    let original_token_estimate = estimate_tokens_for_messages(head);

    for msg in head {
        let role_label = match msg.role {
            Role::User => "Human",
            Role::Assistant => "Assistant",
        };
        let text = msg.get_all_text();
        // Skip the prior compact summary itself — it is fed separately in a
        // <previous-summary> block, so rendering it here would duplicate it.
        if !text.is_empty() && !text.contains("<compact-summary>") {
            transcript.push_str(&format!("{}: {}\n\n", role_label, text));
        }
        // Also render tool use/result blocks
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { name, input, id } => {
                        transcript.push_str(&format!(
                            "[Tool Call: {} (id={})]\nInput: {}\n\n",
                            name, id, input
                        ));
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let result_text = match content {
                            claurst_core::types::ToolResultContent::Text(t) => t.as_str().to_string(),
                            claurst_core::types::ToolResultContent::Blocks(_) => "[complex content]".to_string(),
                        };
                        let error_flag = if is_error.unwrap_or(false) { " [ERROR]" } else { "" };
                        transcript.push_str(&format!(
                            "[Tool Result (id={}){}]\n{}\n\n",
                            tool_use_id, error_flag, result_text
                        ));
                    }
                    _ => {}
                }
            }
        }
    }

    // Select the UPDATE prompt variant when a prior summary is present.
    let compact_prompt = get_compact_prompt(None, previous_summary.as_deref());

    let user_content = if let Some(prev) = previous_summary.as_deref() {
        format!(
            "{}\n\n<previous-summary>\n{}\n</previous-summary>\n\n<conversation_to_summarize original_messages=\"{}\" estimated_tokens=\"{}\">\n{}\n</conversation_to_summarize>",
            compact_prompt,
            prev,
            original_count,
            original_token_estimate,
            transcript
        )
    } else {
        format!(
            "{}\n\n<conversation_to_summarize original_messages=\"{}\" estimated_tokens=\"{}\">\n{}\n</conversation_to_summarize>",
            compact_prompt,
            original_count,
            original_token_estimate,
            transcript
        )
    };

    let api_msgs = vec![ApiMessage {
        role: "user".to_string(),
        content: Value::String(user_content),
    }];

    let request = CreateMessageRequest::builder(model, max_summary_tokens)
        .messages(api_msgs)
        .system(SystemPrompt::Text(
            "You are a helpful assistant that creates concise yet thorough conversation summaries. \
             Preserve all technical details, file names, code snippets, and decisions that would \
             be important for continuing the work. Follow the structured format exactly."
                .to_string(),
        ))
        .build();

    // Use a null handler since we just want the final accumulated message.
    let handler: Arc<dyn StreamHandler> = Arc::new(claurst_api::streaming::NullStreamHandler);
    let mut rx = client.create_message_stream(request, handler).await?;
    let mut acc = StreamAccumulator::new();

    while let Some(evt) = rx.recv().await {
        acc.on_event(&evt);
        if matches!(evt, AnthropicStreamEvent::MessageStop) {
            break;
        }
    }

    let (summary_msg, _usage, _stop) = acc.finish();
    let raw_summary = summary_msg.get_all_text();

    if raw_summary.is_empty() {
        return Err(ClaudeError::Other("Compact summary was empty".to_string()));
    }

    let formatted_summary = format_compact_summary(&raw_summary);

    // Build the new conversation:
    //   [user: compact summary preamble] [recent tail messages]
    //
    // The summary is wrapped in <compact-summary> tags so the NEXT compaction can
    // detect it (via extract_previous_summary) and fold it forward in UPDATE mode.
    let compact_notice = Message::user(format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation (originally {} messages, \
         ~{} tokens).\n\n<compact-summary>\n{}\n</compact-summary>",
        original_count, original_token_estimate, formatted_summary
    ));

    let mut new_messages = vec![compact_notice];
    new_messages.extend_from_slice(&messages[split_at..]);

    Ok(new_messages)
}

/// Does this message carry any `tool_result` blocks?
///
/// A `tool_result` always answers the `tool_use` in the message *immediately
/// before* it, so a compaction cut must never land on such a message: doing so
/// would orphan the result from its call in the kept tail (and, symmetrically,
/// leave a dangling `tool_use` at the end of the summarised head).
fn message_has_tool_result(msg: &Message) -> bool {
    match &msg.content {
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. })),
        _ => false,
    }
}

/// Snap a raw keep-index back to a pairing-safe round boundary.
///
/// A cut at index `k` keeps `messages[k..]` verbatim. It is pairing-safe iff
/// `messages[k]` carries no `tool_result` blocks (see [`message_has_tool_result`]).
/// We walk *backwards* (keeping MORE — never less — than the raw budget asked
/// for) until we land on a safe boundary. This preserves the round-aligned,
/// tool_use↔tool_result-paired history compaction must emit, independent of the
/// separate `sanitize_history` repair pass.
fn snap_to_pairing_boundary(messages: &[Message], idx: usize) -> usize {
    let len = messages.len();
    // Keep-nothing (idx == len): the tail is empty, so there is no boundary
    // message that could be orphaned — leave it as-is.
    let mut idx = idx.min(len);
    while idx > 0 && idx < len && message_has_tool_result(&messages[idx]) {
        idx -= 1;
    }
    idx
}

/// Decide how much of the recent tail to preserve verbatim, driven by a TOKEN
/// budget rather than a fixed message count.
///
/// Returns the split index: everything before it is summarised, everything at or
/// after it is kept verbatim. Larger `keep_recent_tokens` keeps more messages;
/// smaller keeps fewer. The index is snapped to a tool_use↔tool_result-safe
/// boundary so pairing is never broken.
fn compute_keep_split_index(messages: &[Message], keep_recent_tokens: u64) -> usize {
    if messages.is_empty() {
        return 0;
    }
    let raw = calculate_messages_to_keep_index(messages, keep_recent_tokens);
    snap_to_pairing_boundary(messages, raw)
}

/// Compact `messages` in-place, replacing the head with a summary.
/// Returns the new messages vector on success.
pub async fn compact_conversation(
    client: &claurst_api::AnthropicClient,
    messages: &[Message],
    model: &str,
) -> Result<Vec<Message>, ClaudeError> {
    let total = messages.len();

    // Token-budget keep: summarise everything older than the most recent
    // ~KEEP_RECENT_TOKENS worth of messages, cut on a pairing-safe boundary.
    let split_at = compute_keep_split_index(messages, KEEP_RECENT_TOKENS);

    if split_at == 0 {
        debug!(
            total,
            keep_recent_tokens = KEEP_RECENT_TOKENS,
            "Whole conversation fits the keep-recent budget – keeping everything"
        );
        return Ok(messages.to_vec());
    }

    info!(
        total,
        split_at,
        keep_recent_tokens = KEEP_RECENT_TOKENS,
        "Compacting conversation (token-budget keep)"
    );

    // Use a generous token budget for the summary (20k mirrors TypeScript MAX_OUTPUT_TOKENS_FOR_SUMMARY)
    summarise_head(client, messages, split_at, model, 20_000).await
}

/// Auto-compact `messages` if needed.  Updates `state` in place.
/// Returns `Some(new_messages)` if compaction ran, `None` otherwise.
///
/// `context_window` is the effective window for the active provider+model
/// (resolve it via [`resolve_context_window`]); `model` is still used for the
/// summarisation API call.
pub async fn auto_compact_if_needed(
    client: &claurst_api::AnthropicClient,
    messages: &[Message],
    input_tokens: u64,
    model: &str,
    context_window: u64,
    state: &mut AutoCompactState,
) -> Option<Vec<Message>> {
    if !should_auto_compact_for_window(input_tokens, context_window, state) {
        return None;
    }

    info!(
        input_tokens,
        model,
        compaction_count = state.compaction_count,
        "Auto-compact triggered"
    );

    match compact_conversation(client, messages, model).await {
        Ok(new_msgs) => {
            state.on_success();
            info!(
                original_count = messages.len(),
                new_count = new_msgs.len(),
                "Auto-compact complete"
            );
            Some(new_msgs)
        }
        Err(e) => {
            warn!(error = %e, "Auto-compact failed");
            state.on_failure();
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Reactive Compact (T1-1) — fires on usage data, not after turn end
// ---------------------------------------------------------------------------
//
// The TypeScript source uses a `ReactiveCompact` class with GrowthBook
// feature flags and a subscription to the streaming API's token-usage
// events.  In the Rust port we model the same behaviour with plain async
// functions and an env-var feature gate (`CLAUDE_REACTIVE_COMPACT=1`).
//
// Phase overview (mirrors reactiveCompact.ts):
//   1. Check usage with `should_compact` / `should_context_collapse`.
//   2. Strip image blocks from the conversation before compacting
//      (reduces the size of the prompt sent to the summariser).
//   3. Call `summarise_head` to generate a compact summary.
//   4. Re-inject recently-modified files (up to 5) as context.
//      (In the Rust port this phase is a no-op stub — the TUI layer owns
//      file-tracking; this file intentionally avoids the filesystem.)

/// Trigger classification for reactive compact.
#[derive(Debug, Clone)]
pub enum CompactTrigger {
    /// Normal 90 %-threshold compact.
    TokenThreshold { tokens_used: u64, context_limit: u64 },
    /// Caller requested an unconditional compact.
    Forced,
}

/// Result returned by `reactive_compact` and `context_collapse`.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// The new (reduced) message list.
    pub messages: Vec<claurst_core::types::Message>,
    /// Formatted summary text injected at the head of `messages`.
    pub summary: String,
    /// Rough estimate of how many tokens were freed.
    pub tokens_freed: u64,
}

/// Return `true` when reactive compact should fire (≥ 90 % of context window).
///
/// Threshold is intentionally identical to `AUTOCOMPACT_TRIGGER_FRACTION` so
/// that exactly one of the two paths (proactive auto-compact vs reactive
/// compact) fires, chosen by the `CLAUDE_REACTIVE_COMPACT` gate.
pub fn should_compact(tokens_used: u64, context_limit: u64) -> bool {
    if context_limit == 0 {
        return false;
    }
    let threshold = (context_limit as f64 * REACTIVE_COMPACT_THRESHOLD) as u64;
    tokens_used >= threshold
}

/// Return `true` when the emergency context-collapse should fire (≥ 97 %).
///
/// Context-collapse is a last-resort measure: it produces an ultra-short
/// summary and keeps only the most recent user turn so that the next API call
/// can succeed even when the conversation is severely over-limit.
pub fn should_context_collapse(tokens_used: u64, context_limit: u64) -> bool {
    if context_limit == 0 {
        return false;
    }
    let threshold = (context_limit as f64 * CONTEXT_COLLAPSE_THRESHOLD) as u64;
    tokens_used >= threshold
}

/// Snip the middle of the conversation, keeping:
///   - the first message (usually the system/context bootstrap), and
///   - the `keep_n_newest` most-recent messages.
///
/// Returns `(new_messages, rough_tokens_freed)`.
///
/// Mirrors `snipCompact` from TypeScript (no API call required — purely local).
pub fn snip_compact(messages: Vec<claurst_core::types::Message>, keep_n_newest: usize) -> (Vec<claurst_core::types::Message>, u64) {
    let total = messages.len();
    if total <= keep_n_newest + 1 {
        // Nothing to snip.
        return (messages, 0);
    }

    // Keep: messages[0] (first/system message) + messages[total-keep_n_newest..]
    let snip_start = 1usize;
    let snip_end = total.saturating_sub(keep_n_newest);

    if snip_start >= snip_end {
        return (messages, 0);
    }

    // Estimate how many tokens the snipped range held.
    let snipped_tokens =
        estimate_tokens_for_messages(&messages[snip_start..snip_end]) as u64;

    let mut result = Vec::with_capacity(1 + keep_n_newest);
    result.push(messages[0].clone());
    result.extend_from_slice(&messages[snip_end..]);

    (result, snipped_tokens)
}

/// Compute the index into `messages` such that the tail starting at that
/// index fits within `token_budget` tokens.
///
/// Returns the cut index (0 = keep everything, messages.len() = keep nothing).
/// Iterates from the newest message backwards, accumulating token estimates
/// until the budget is exhausted.
pub fn calculate_messages_to_keep_index(messages: &[claurst_core::types::Message], token_budget: u64) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let mut accumulated: u64 = 0;
    let mut keep_from = messages.len(); // default: keep nothing (index past end)

    for (i, msg) in messages.iter().enumerate().rev() {
        let est = estimate_tokens_for_messages(std::slice::from_ref(msg)) as u64;
        if accumulated + est > token_budget {
            // This message would push us over budget — stop here.
            keep_from = i + 1;
            break;
        }
        accumulated += est;
        keep_from = i;
    }

    keep_from
}

/// Remove image blocks from a message list before compacting.
///
/// Image tokens are expensive and carry no information that a text summary
/// needs.  Mirrors the TypeScript `stripImages` helper used inside
/// `reactiveCompact.ts`.
fn strip_images(messages: Vec<claurst_core::types::Message>) -> Vec<claurst_core::types::Message> {
    use claurst_core::types::{ContentBlock, MessageContent};

    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                blocks.retain(|b| !matches!(b, ContentBlock::Image { .. }));
                // If stripping left only an empty block list, collapse to a
                // placeholder text so the conversation remains parseable.
                if blocks.is_empty() {
                    msg.content = MessageContent::Text("[image removed for compaction]".to_string());
                }
            }
            msg
        })
        .collect()
}

/// Run reactive compact: summarise the oldest messages and return a trimmed
/// conversation.
///
/// Feature gate: only call this when
/// `claurst_core::feature_gates::is_feature_enabled("reactive_compact")` is true.
///
/// The `cancel` token is checked before the API call so the user can abort
/// a long-running compact.
pub async fn reactive_compact(
    messages: Vec<claurst_core::types::Message>,
    client: &claurst_api::AnthropicClient,
    config: &crate::QueryConfig,
    cancel: tokio_util::sync::CancellationToken,
    recently_modified: &[std::path::PathBuf],
) -> Result<CompactResult, claurst_core::error::ClaudeError> {
    if cancel.is_cancelled() {
        return Err(claurst_core::error::ClaudeError::Cancelled);
    }

    let total = messages.len();
    if total == 0 {
        return Ok(CompactResult {
            messages: vec![],
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    // Phase 2: strip images before the compact API call.
    let stripped = strip_images(messages.clone());

    // Phase 1 + 3: summarise the head (everything older than the ~KEEP_RECENT_TOKENS
    // recent tail, cut on a pairing-safe boundary), then replace the old head with
    // the summary message.
    let split_at = compute_keep_split_index(&stripped, KEEP_RECENT_TOKENS);
    if split_at == 0 {
        // Too few messages; nothing to summarise.
        return Ok(CompactResult {
            messages,
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    let original_token_estimate =
        estimate_tokens_for_messages(&stripped[..split_at]) as u64;

    let mut new_messages =
        summarise_head(client, &stripped, split_at, &config.model, 20_000).await?;

    // The summary lives as the first message in new_messages.
    let summary_text = new_messages
        .first()
        .map(|m| m.get_all_text())
        .unwrap_or_default();

    // Phase 4: re-inject recently modified file context (up to 5 files, skip >50KB).
    const MAX_FILES: usize = 5;
    const MAX_FILE_BYTES: u64 = 50 * 1024;
    let mut injected = 0;
    for path in recently_modified.iter().take(MAX_FILES * 3) {
        if injected >= MAX_FILES {
            break;
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let file_name = path.display().to_string();
        let text = format!("<file path=\"{}\">\n{}\n</file>", file_name, content);
        new_messages.push(claurst_core::types::Message::user(text));
        injected += 1;
    }

    let tokens_after = estimate_tokens_for_messages(&new_messages) as u64;
    let tokens_freed = original_token_estimate.saturating_sub(tokens_after);

    Ok(CompactResult {
        messages: new_messages,
        summary: summary_text,
        tokens_freed,
    })
}

/// Emergency context collapse: produce an ultra-short summary that distils
/// the entire conversation into the minimum needed to continue, then keep
/// only the most recent user turn.
///
/// Use only when `should_context_collapse()` returns `true` — i.e. the
/// context is at ≥ 97 % capacity and a regular reactive compact is unlikely
/// to free enough space.
pub async fn context_collapse(
    messages: Vec<claurst_core::types::Message>,
    client: &claurst_api::AnthropicClient,
    config: &crate::QueryConfig,
) -> Result<CompactResult, claurst_core::error::ClaudeError> {
    use claurst_api::{AnthropicStreamEvent, ApiMessage, CreateMessageRequest, StreamAccumulator, StreamHandler, SystemPrompt};
    use serde_json::Value;
    use std::sync::Arc;

    let total = messages.len();
    if total == 0 {
        return Ok(CompactResult {
            messages: vec![],
            summary: String::new(),
            tokens_freed: 0,
        });
    }

    let original_tokens = estimate_tokens_for_messages(&messages) as u64;

    // Build a concise transcript for the collapse prompt.
    let mut transcript = String::new();
    for msg in &messages {
        let role = match msg.role {
            claurst_core::types::Role::User => "Human",
            claurst_core::types::Role::Assistant => "Assistant",
        };
        let text = msg.get_all_text();
        if !text.is_empty() {
            transcript.push_str(&format!("{}: {}\n\n", role, text));
        }
    }

    let collapse_prompt = format!(
        "EMERGENCY CONTEXT COLLAPSE — the conversation is at critical capacity.\n\
         Produce an ULTRA-SHORT (max 500 words) emergency summary that captures:\n\
         1. The user's most recent explicit request.\n\
         2. The single most important decision made so far.\n\
         3. Any file names or code snippets that are ESSENTIAL to continue.\n\
         4. What was being worked on immediately before this collapse.\n\
         Respond with plain text only — no XML tags, no tool calls.\n\n\
         <conversation>\n{}\n</conversation>",
        transcript
    );

    let api_msgs = vec![ApiMessage {
        role: "user".to_string(),
        content: Value::String(collapse_prompt),
    }];

    let request = CreateMessageRequest::builder(&config.model, 1_000)
        .messages(api_msgs)
        .system(SystemPrompt::Text(
            "You are a conversation summariser. Produce an emergency ultra-short \
             summary as instructed. Plain text only."
                .to_string(),
        ))
        .build();

    let handler: Arc<dyn StreamHandler> = Arc::new(claurst_api::streaming::NullStreamHandler);
    let mut rx = client.create_message_stream(request, handler).await?;
    let mut acc = StreamAccumulator::new();

    while let Some(evt) = rx.recv().await {
        acc.on_event(&evt);
        if matches!(evt, AnthropicStreamEvent::MessageStop) {
            break;
        }
    }

    let (summary_msg, _usage, _stop) = acc.finish();
    let summary_text = summary_msg.get_all_text();

    if summary_text.is_empty() {
        return Err(claurst_core::error::ClaudeError::Other(
            "Context-collapse summary was empty".to_string(),
        ));
    }

    // Keep only: the synthetic summary + the most recent user turn.
    let collapse_notice = claurst_core::types::Message::user(format!(
        "[EMERGENCY CONTEXT COLLAPSE — conversation condensed to stay within limits]\n\n{}",
        summary_text
    ));

    // Find the last user message in the original list.
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == claurst_core::types::Role::User)
        .cloned();

    let mut new_messages = vec![collapse_notice];
    if let Some(last) = last_user {
        new_messages.push(last);
    }

    let tokens_after = estimate_tokens_for_messages(&new_messages) as u64;
    let tokens_freed = original_tokens.saturating_sub(tokens_after);

    Ok(CompactResult {
        messages: new_messages,
        summary: summary_text,
        tokens_freed,
    })
}

// Threshold constants for reactive compact / context-collapse.
/// Reactive compact fires at 90 % of the context window.
const REACTIVE_COMPACT_THRESHOLD: f64 = 0.90;
/// Context collapse (emergency) fires at 97 % of the context window.
const CONTEXT_COLLAPSE_THRESHOLD: f64 = 0.97;

// ---------------------------------------------------------------------------
// T4-5: Collapse read/search results (mirrors src/utils/collapseReadSearch.ts)
// ---------------------------------------------------------------------------

/// Replace repeated reads of the same file with a single summary.
///
/// When the same file is read more than once in the conversation, replaces
/// all but the last read with `[Content shown N time(s); showing last occurrence only]`.
pub fn collapse_read_tool_results(messages: Vec<claurst_core::types::Message>) -> Vec<claurst_core::types::Message> {
    use claurst_core::types::{ContentBlock, MessageContent, ToolResultContent};
    use std::collections::HashMap;

    // Helper: extract a fingerprint string from ToolResultContent.
    fn fingerprint(content: &ToolResultContent) -> Option<String> {
        match content {
            ToolResultContent::Text(t) => Some(t.chars().take(120).collect()),
            ToolResultContent::Blocks(_) => None,
        }
    }

    // First pass: find all file-read tool results and count by fingerprint.
    let mut read_counts: HashMap<String, usize> = HashMap::new();
    for msg in &messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if let Some(key) = fingerprint(content) {
                        *read_counts.entry(key).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Second pass: replace intermediate (non-last) occurrences.
    let mut seen: HashMap<String, usize> = HashMap::new();
    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                for block in blocks.iter_mut() {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        if let Some(key) = fingerprint(content) {
                            let count = read_counts.get(&key).copied().unwrap_or(1);
                            if count > 1 {
                                let seen_count = seen.entry(key.clone()).or_insert(0);
                                *seen_count += 1;
                                if *seen_count < count {
                                    // Replace intermediate occurrences.
                                    *content = ToolResultContent::Text(format!(
                                        "[Content shown {} time(s); showing last occurrence only]",
                                        count
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            msg
        })
        .collect()
}

/// Deduplicate grep/glob search results that appear multiple times.
///
/// If the same search was run more than once (same query), keep only the
/// most recent result; replace earlier results with a truncation notice.
pub fn collapse_search_results(messages: Vec<claurst_core::types::Message>) -> Vec<claurst_core::types::Message> {
    use claurst_core::types::{ContentBlock, MessageContent, ToolResultContent};
    use std::collections::HashSet;

    fn fingerprint(content: &ToolResultContent) -> Option<String> {
        match content {
            ToolResultContent::Text(t) => Some(t.chars().take(200).collect()),
            ToolResultContent::Blocks(_) => None,
        }
    }

    let mut seen_results: HashSet<String> = HashSet::new();

    // Iterate in reverse to keep the latest occurrence.
    let mut result: Vec<claurst_core::types::Message> = messages
        .into_iter()
        .rev()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                for block in blocks.iter_mut() {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        if let Some(fp) = fingerprint(content) {
                            if !seen_results.insert(fp) {
                                *content = ToolResultContent::Text(
                                    "[Duplicate search result; content shown in a later turn]"
                                        .to_string(),
                                );
                            }
                        }
                    }
                }
            }
            msg
        })
        .collect();

    result.reverse();
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::types::{Message, Role};

    fn make_user(text: &str) -> Message {
        Message::user(text)
    }

    fn make_assistant(text: &str) -> Message {
        // No UUID set — relies on the no-UUID grouping path in group_messages_for_compact.
        Message::assistant(text)
    }

    // ---- TokenWarningState --------------------------------------------------

    #[test]
    fn test_warning_state_ok() {
        // 50 % of 200k = 100k tokens — should be Ok
        let state = calculate_token_warning_state(100_000, "claude-sonnet-4-6");
        assert_eq!(state, TokenWarningState::Ok);
    }

    #[test]
    fn test_warning_state_warning() {
        // 85 % of 200k = 170k tokens — should be Warning
        let state = calculate_token_warning_state(170_000, "claude-sonnet-4-6");
        assert_eq!(state, TokenWarningState::Warning);
    }

    #[test]
    fn test_warning_state_critical() {
        // 96 % of 200k = 192k tokens — should be Critical
        let state = calculate_token_warning_state(192_000, "claude-sonnet-4-6");
        assert_eq!(state, TokenWarningState::Critical);
    }

    #[test]
    fn test_warning_state_boundary_80pct() {
        // Exactly 80 % of 200k = 160k tokens — should be Warning (>= threshold)
        let state = calculate_token_warning_state(160_000, "claude-sonnet-4-6");
        assert_eq!(state, TokenWarningState::Warning);
    }

    #[test]
    fn test_warning_state_boundary_95pct() {
        // Exactly 95 % of 200k = 190k tokens — should be Critical
        let state = calculate_token_warning_state(190_000, "claude-sonnet-4-6");
        assert_eq!(state, TokenWarningState::Critical);
    }

    // ---- should_auto_compact ------------------------------------------------

    #[test]
    fn test_should_not_compact_when_disabled() {
        let mut state = AutoCompactState::default();
        state.disabled = true;
        assert!(!should_auto_compact(195_000, "claude-sonnet-4-6", &state));
    }

    #[test]
    fn test_should_compact_at_90pct() {
        let state = AutoCompactState::default();
        // 90 % of 200k = 180k — should trigger
        assert!(should_auto_compact(180_000, "claude-sonnet-4-6", &state));
    }

    #[test]
    fn test_should_not_compact_below_90pct() {
        let state = AutoCompactState::default();
        // 70 % of 200k = 140k — should NOT trigger
        assert!(!should_auto_compact(140_000, "claude-sonnet-4-6", &state));
    }

    // ---- Circuit breaker ----------------------------------------------------

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let mut state = AutoCompactState::default();
        assert!(!state.disabled);
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            state.on_failure();
        }
        assert!(state.disabled);
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let mut state = AutoCompactState::default();
        state.on_failure();
        state.on_failure();
        state.on_success();
        assert_eq!(state.consecutive_failures, 0);
        assert!(!state.disabled);
    }

    // ---- Message grouping ---------------------------------------------------

    #[test]
    fn test_group_messages_simple() {
        let messages = vec![
            make_user("Hello"),
            make_assistant("Hi there"),
            make_user("How are you?"),
            make_assistant("I'm fine"),
        ];

        let groups = group_messages_for_compact(&messages);
        // Should produce 2 groups: one per assistant turn boundary
        assert_eq!(groups.len(), 2);
        // First group: user + first assistant
        assert_eq!(groups[0].messages.len(), 2);
        // Second group: second user + second assistant
        assert_eq!(groups[1].messages.len(), 2);
    }

    #[test]
    fn test_group_empty() {
        let groups = group_messages_for_compact(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_group_only_user_messages() {
        // No assistant messages → everything in one group
        let messages = vec![make_user("A"), make_user("B"), make_user("C")];
        let groups = group_messages_for_compact(&messages);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].messages.len(), 3);
    }

    // ---- format_compact_summary --------------------------------------------

    #[test]
    fn test_format_strips_analysis() {
        let raw = "<analysis>This is scratchpad text.</analysis>\n\
                   <summary>This is the real content.</summary>";
        let formatted = format_compact_summary(raw);
        assert!(!formatted.contains("<analysis>"));
        assert!(!formatted.contains("scratchpad text"));
        assert!(formatted.contains("real content"));
    }

    #[test]
    fn test_format_replaces_summary_tags() {
        let raw = "<summary>Content here</summary>";
        let formatted = format_compact_summary(raw);
        assert!(!formatted.contains("<summary>"));
        assert!(formatted.contains("Summary:"));
        assert!(formatted.contains("Content here"));
    }

    #[test]
    fn test_format_passthrough_when_no_tags() {
        let raw = "Plain text summary without any XML tags.";
        let formatted = format_compact_summary(raw);
        assert_eq!(formatted, raw);
    }

    // ---- get_compact_prompt ------------------------------------------------

    #[test]
    fn test_compact_prompt_contains_no_tools_preamble() {
        let prompt = get_compact_prompt(None, None);
        assert!(prompt.contains("CRITICAL: Respond with TEXT ONLY"));
        assert!(prompt.contains("Do NOT call any tools"));
    }

    #[test]
    fn test_compact_prompt_contains_sections() {
        let prompt = get_compact_prompt(None, None);
        assert!(prompt.contains("Primary Request and Intent"));
        assert!(prompt.contains("Key Technical Concepts"));
        assert!(prompt.contains("Files and Code Sections"));
        assert!(prompt.contains("Errors and fixes"));
        assert!(prompt.contains("Pending Tasks"));
        assert!(prompt.contains("Current Work"));
    }

    #[test]
    fn test_compact_prompt_with_custom_instructions() {
        let prompt = get_compact_prompt(Some("Focus on Rust type system changes."), None);
        assert!(prompt.contains("Additional Instructions:"));
        assert!(prompt.contains("Focus on Rust type system changes."));
    }

    #[test]
    fn test_compact_prompt_empty_custom_instructions_ignored() {
        let prompt_none = get_compact_prompt(None, None);
        let prompt_empty = get_compact_prompt(Some("   "), None);
        assert_eq!(prompt_none, prompt_empty);
    }

    // ---- context_window_for_model ------------------------------------------

    #[test]
    fn test_context_window_sonnet4() {
        assert_eq!(context_window_for_model("claude-sonnet-4-6"), 200_000);
    }

    #[test]
    fn test_context_window_opus4() {
        assert_eq!(context_window_for_model("claude-opus-4-0"), 200_000);
    }

    #[test]
    fn test_context_window_legacy() {
        assert_eq!(context_window_for_model("claude-2"), 100_000);
    }

    // ---- resolve_context_window (#216) -------------------------------------

    /// Build an in-memory `ModelRegistry` from a models.dev-style JSON snapshot
    /// by round-tripping it through the real `load_cache` parse path.
    fn registry_from_json(json: &str) -> claurst_api::ModelRegistry {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models_dev.json");
        std::fs::write(&path, json).expect("write snapshot");
        let mut reg = claurst_api::ModelRegistry::new();
        reg.load_cache(&path);
        reg
    }

    // A fake provider with a genuine 1M window and a placeholder (no-limit)
    // model. Fake ids keep the fixture isolated from the bundled snapshot.
    const TEST_SNAPSHOT: &str = r#"{"testprov":{"id":"testprov","name":"Test Provider","env":[],"models":{"big-context-model":{"id":"big-context-model","name":"Big Context Model","limit":{"context":1000000,"output":65536}},"tiny-model":{"id":"tiny-model","name":"Tiny Model"}}}}"#;

    #[test]
    fn resolve_prefers_registry_for_large_context_model() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // Sanity: the registry really carries the 1M window.
        assert_eq!(
            reg.get("testprov", "big-context-model").unwrap().info.context_window,
            1_000_000
        );
        assert_eq!(
            resolve_context_window(Some(&reg), "testprov", "big-context-model"),
            1_000_000
        );
    }

    #[test]
    fn resolve_handles_canonical_provider_slash_model_string() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // Model string carries the provider prefix; still resolves to 1M.
        assert_eq!(
            resolve_context_window(Some(&reg), "testprov", "testprov/big-context-model"),
            1_000_000
        );
        // Provider arg is wrong but the "provider/model" string still resolves.
        assert_eq!(
            resolve_context_window(Some(&reg), "anthropic", "testprov/big-context-model"),
            1_000_000
        );
    }

    #[test]
    fn resolve_falls_back_to_heuristic_when_registry_none() {
        // No registry → heuristic. Claude-ish and legacy both come through.
        assert_eq!(
            resolve_context_window(None, "anthropic", "claude-opus-4-8"),
            context_window_for_model("claude-opus-4-8")
        );
        assert_eq!(resolve_context_window(None, "anthropic", "claude-opus-4-8"), 200_000);
        assert_eq!(resolve_context_window(None, "some-provider", "some-model"), 100_000);
    }

    #[test]
    fn resolve_falls_back_to_heuristic_when_no_registry_entry() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // Provider/model that isn't in the registry → heuristic default.
        assert_eq!(
            resolve_context_window(Some(&reg), "nope", "ghost-model"),
            context_window_for_model("ghost-model")
        );
        assert_eq!(resolve_context_window(Some(&reg), "nope", "ghost-model"), 100_000);
    }

    #[test]
    fn resolve_ignores_placeholder_4096_window() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // The registry stores the models.dev-omission placeholder (4096)...
        assert_eq!(
            reg.get("testprov", "tiny-model").unwrap().info.context_window,
            4096
        );
        // ...but resolve treats it as "unknown" and uses the heuristic instead
        // of compacting a real session at ~3.7k tokens.
        assert_eq!(
            resolve_context_window(Some(&reg), "testprov", "tiny-model"),
            context_window_for_model("tiny-model")
        );
        assert_eq!(resolve_context_window(Some(&reg), "testprov", "tiny-model"), 100_000);
    }

    // ---- estimate_tokens_for_messages --------------------------------------

    #[test]
    fn test_token_estimate_nonempty() {
        let msgs = vec![make_user("Hello, world!")];
        let est = estimate_tokens_for_messages(&msgs);
        // "Hello, world!" = 13 chars → 13/4 = 3 rough tokens → 3*4/3 = 4 padded
        assert!(est > 0);
    }
}
