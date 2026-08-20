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

use mikmik_api::{
    AnthropicStreamEvent, ApiMessage, CreateMessageRequest, StreamAccumulator, StreamHandler,
    SystemPrompt,
};
use mikmik_core::config::WireModel;
use mikmik_core::error::ClaudeError;
use mikmik_core::types::{ContentBlock, Message, MessageContent, Role};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
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
const WARNING_PCT: f64 = 0.80; // 80 % full → yellow warning
const CRITICAL_PCT: f64 = 0.95; // 95 % full → red critical

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Tracks auto-compact state across a whole session.
///
/// Held in [`SESSION_COMPACT_STATE`] and keyed by session id, not by the turn
/// loop: a value scoped to one `run_query_loop` call restarts on every user
/// message, so three consecutive failures would have to land inside a single
/// prompt and the circuit breaker below could never open.
#[derive(Debug, Default, Clone)]
pub struct AutoCompactState {
    /// Total compactions performed this session.
    pub compaction_count: u32,
    /// Consecutive failures (reset on success).
    pub consecutive_failures: u32,
    /// Whether the circuit breaker is open (too many failures).
    pub disabled: bool,
    /// The prompt size the provider last reported, in tokens.
    ///
    /// Session-scoped for the same reason the breaker is: every user message
    /// starts a fresh turn loop, so a value living in that loop is always zero
    /// at the request boundary and the threshold falls back to the chars/4
    /// estimate, which does not see the system prompt, the tool schemas or the
    /// cache. Measured: a session the provider reported at 90% of its window
    /// was not compacted at all.
    pub last_context_tokens: u64,
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

/// Every live session's auto-compact state, keyed by session id.
///
/// A `parking_lot::Mutex` because the critical sections are two field reads
/// and a write. No guard may cross the summarisation `.await`, so
/// [`auto_compact_if_needed`] reads a copy, runs the call, then writes the
/// outcome back.
static SESSION_COMPACT_STATE: once_cell::sync::Lazy<
    parking_lot::Mutex<HashMap<String, AutoCompactState>>,
> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(HashMap::new()));

/// Read one session's auto-compact state.
pub fn compact_state_for(session_id: &str) -> AutoCompactState {
    SESSION_COMPACT_STATE
        .lock()
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// Record the prompt size the provider reported for a session's last turn.
pub fn record_context_tokens(session_id: &str, tokens: u64) {
    if tokens == 0 {
        return;
    }
    update_compact_state(session_id, |state| state.last_context_tokens = tokens);
}

/// Apply `f` to one session's auto-compact state, creating it if absent.
fn update_compact_state(session_id: &str, f: impl FnOnce(&mut AutoCompactState)) {
    let mut states = SESSION_COMPACT_STATE.lock();
    f(states.entry(session_id.to_string()).or_default());
}

/// Forget a session's auto-compact state once the session is over.
pub fn forget_compact_state(session_id: &str) {
    SESSION_COMPACT_STATE.lock().remove(session_id);
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

/// Estimated size of a conversation, in tokens.
///
/// For a caller with no provider-reported usage to go on, such as the footer
/// right after `/compact` replaced the history.
pub fn estimate_context_size(messages: &[Message]) -> u64 {
    estimate_tokens_for_messages(messages) as u64
}

/// Rough token estimate: sum of character lengths divided by 4, padded by 4/3.
pub(crate) fn estimate_tokens_for_messages(messages: &[Message]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            MessageContent::Blocks(blocks) => blocks.iter().map(estimate_block_chars).sum(),
        })
        .sum();
    // chars / 4 = rough tokens, then * 4/3 padding
    (chars / 4) * 4 / 3
}

fn estimate_block_chars(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        ContentBlock::ToolResult { content, .. } => match content {
            mikmik_core::types::ToolResultContent::Text(t) => t.len(),
            mikmik_core::types::ToolResultContent::Blocks(blocks) => {
                blocks.iter().map(estimate_block_chars).sum()
            }
        },
        ContentBlock::Thinking { thinking, .. } => thinking.len(),
        ContentBlock::RedactedThinking { data } => data.len(),
        _ => 200, // default for images/documents
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
const NO_TOOLS_TRAILER: &str =
    "\n\nREMINDER: Do NOT call any tools. Respond with plain text only — \
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

// ---------------------------------------------------------------------------
// Files-touched manifest (mirrors extractFileOperations / formatFileOperations)
// ---------------------------------------------------------------------------

/// Set of files the agent read / wrote / edited across a batch of history.
///
/// Sorted (`BTreeSet`) so the emitted manifest is deterministic, and unioned
/// across successive compactions so the agent never forgets what it worked on.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct FileOps {
    read: BTreeSet<String>,
    written: BTreeSet<String>,
    edited: BTreeSet<String>,
}

impl FileOps {
    fn is_empty(&self) -> bool {
        self.read.is_empty() && self.written.is_empty() && self.edited.is_empty()
    }

    /// Merge another manifest into this one (used to carry a prior manifest
    /// forward across compactions).
    fn union(&mut self, other: &FileOps) {
        self.read.extend(other.read.iter().cloned());
        self.written.extend(other.written.iter().cloned());
        self.edited.extend(other.edited.iter().cloned());
    }

    /// Compute the final `(read_only, modified)` lists: a file that was written
    /// or edited is "modified" (and dropped from the read-only list even if it
    /// was also read). Mirrors pi's `computeFileLists`.
    fn computed_lists(&self) -> (Vec<String>, Vec<String>) {
        let mut modified: BTreeSet<String> = self.edited.clone();
        modified.extend(self.written.iter().cloned());
        let read_only: Vec<String> = self
            .read
            .iter()
            .filter(|f| !modified.contains(*f))
            .cloned()
            .collect();
        (read_only, modified.into_iter().collect())
    }
}

/// Cap on how many files to list per bucket in the manifest; the overflow is
/// summarised as "(+N more)" so the manifest stays bounded across compactions.
const MAX_MANIFEST_FILES: usize = 20;

/// Header line that introduces the files-touched manifest inside a summary.
const FILES_TOUCHED_HEADER: &str = "Files touched:";

/// Delimiter between file paths in a manifest line. A ` | ` separator keeps the
/// manifest re-parseable (paths effectively never contain it).
const MANIFEST_SEP: &str = " | ";

/// Extract file read/write/edit operations from the tool calls in `messages`.
///
/// Classifies by tool name (`Read` → read, `Write` → written, `Edit` /
/// `BatchEdit` / `NotebookEdit` / `ApplyPatch` → edited) and pulls the path from
/// the tool input (`file_path`, falling back to `path` / `notebook_path`, and
/// the per-edit `file_path`s inside a `BatchEdit`).
fn extract_file_operations(messages: &[Message]) -> FileOps {
    let mut ops = FileOps::default();
    for msg in messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolUse { name, input, .. } = block {
                    collect_file_op(name, input, &mut ops);
                }
            }
        }
    }
    ops
}

/// Pull the file path(s) touched by a single tool call into `ops`.
fn collect_file_op(name: &str, input: &Value, ops: &mut FileOps) {
    use mikmik_core::constants::{
        TOOL_NAME_APPLY_PATCH, TOOL_NAME_BATCH_EDIT, TOOL_NAME_FILE_EDIT, TOOL_NAME_FILE_READ,
        TOOL_NAME_FILE_WRITE, TOOL_NAME_NOTEBOOK_EDIT,
    };

    // BatchEdit carries an array of edits, each with its own file_path.
    if name == TOOL_NAME_BATCH_EDIT {
        if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
            for edit in edits {
                if let Some(p) = edit.get("file_path").and_then(|v| v.as_str()) {
                    ops.edited.insert(p.to_string());
                }
            }
        }
        return;
    }

    let path = input
        .get("file_path")
        .and_then(|v| v.as_str())
        .or_else(|| input.get("path").and_then(|v| v.as_str()))
        .or_else(|| input.get("notebook_path").and_then(|v| v.as_str()));
    let path = match path {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return,
    };

    match name {
        TOOL_NAME_FILE_READ => {
            ops.read.insert(path);
        }
        TOOL_NAME_FILE_WRITE => {
            ops.written.insert(path);
        }
        TOOL_NAME_FILE_EDIT | TOOL_NAME_NOTEBOOK_EDIT | TOOL_NAME_APPLY_PATCH => {
            ops.edited.insert(path);
        }
        _ => {}
    }
}

/// Render one bounded, capped manifest line from a sorted file list.
fn format_manifest_line(files: &[String]) -> String {
    if files.len() <= MAX_MANIFEST_FILES {
        files.join(MANIFEST_SEP)
    } else {
        let shown = files[..MAX_MANIFEST_FILES].join(MANIFEST_SEP);
        format!("{} (+{} more)", shown, files.len() - MAX_MANIFEST_FILES)
    }
}

/// Format a compact "Files touched" manifest to append to a summary, or an
/// empty string when no files were touched. Bounded via [`MAX_MANIFEST_FILES`].
fn format_files_touched(ops: &FileOps) -> String {
    let (read_only, modified) = ops.computed_lists();
    if read_only.is_empty() && modified.is_empty() {
        return String::new();
    }
    let mut out = format!("\n\n{}\n", FILES_TOUCHED_HEADER);
    if !modified.is_empty() {
        out.push_str(&format!("Modified: {}\n", format_manifest_line(&modified)));
    }
    if !read_only.is_empty() {
        out.push_str(&format!("Read: {}\n", format_manifest_line(&read_only)));
    }
    out.trim_end().to_string()
}

/// Split a manifest line's value back into paths, dropping any `(+N more)` tail.
fn split_manifest_line(rest: &str) -> impl Iterator<Item = String> + '_ {
    let core = match rest.rfind("(+") {
        Some(idx) => rest[..idx].trim_end(),
        None => rest.trim(),
    };
    core.split(MANIFEST_SEP)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse a previously-emitted "Files touched" manifest out of a summary so it
/// can be carried forward and unioned with the current batch. Only the capped
/// (visible) entries survive — that is what keeps the manifest bounded.
fn parse_files_touched(summary: &str) -> FileOps {
    let mut ops = FileOps::default();
    let mut in_section = false;
    for line in summary.lines() {
        let trimmed = line.trim();
        if trimmed == FILES_TOUCHED_HEADER {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Modified:") {
            ops.edited.extend(split_manifest_line(rest));
        } else if let Some(rest) = trimmed.strip_prefix("Read:") {
            ops.read.extend(split_manifest_line(rest));
        } else {
            // Any other line (blank or the start of a new section) ends it.
            in_section = false;
        }
    }
    ops
}

/// Drop a trailing "Files touched" manifest from a summary. Used to keep the
/// prior manifest out of the UPDATE prompt (it is re-appended deterministically
/// from the parsed + unioned `FileOps`, so echoing it would only risk drift).
fn strip_files_touched_section(summary: &str) -> String {
    match summary.find(FILES_TOUCHED_HEADER) {
        Some(idx) => summary[..idx].trim_end().to_string(),
        None => summary.to_string(),
    }
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
    if model.contains("opus-4")
        || model.contains("sonnet-4")
        || model.contains("haiku-4")
        || model.contains("claude-3-5")
        || model.contains("claude-3.5")
    {
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
    registry: &mikmik_api::ModelRegistry,
    provider: &str,
    model_id: &str,
) -> Option<u64> {
    let window = registry.get(provider, model_id)?.info.context_window as u64;
    (window >= MIN_PLAUSIBLE_REGISTRY_WINDOW).then_some(window)
}

/// Resolve the effective context window for the active provider + model.
///
/// The models.dev-backed [`mikmik_api::ModelRegistry`] is the source of truth:
/// it carries real per-model context windows for *every* provider (Gemini/GPT
/// 1M windows, 32k local models, …), so we prefer it. We fall back to the
/// Claude-only [`context_window_for_model`] heuristic only when the registry is
/// absent, has no matching entry, or only holds a placeholder value.
///
/// `model` may be either a bare model id (`"gemini-3-pro"`) or a canonical
/// `"provider/model"` string; both forms are handled.
pub fn resolve_context_window(
    registry: Option<&mikmik_api::ModelRegistry>,
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
    } else if pct >= WARNING_PCT
        || window.saturating_sub(input_tokens) <= WARNING_THRESHOLD_BUFFER_TOKENS
    {
        TokenWarningState::Warning
    } else {
        TokenWarningState::Ok
    }
}

/// Return `true` when auto-compaction should fire.
///
/// `threshold_pct` is the user's `compactThreshold`, a percentage of the
/// window. It is a parameter rather than a constant because the setting exists
/// and used to be read by nobody: the trigger sat at a hardcoded 90% while the
/// settings screen happily saved whatever the user typed.
pub fn should_auto_compact_for_window(
    input_tokens: u64,
    window: u64,
    threshold_pct: u8,
    state: &AutoCompactState,
) -> bool {
    if state.disabled || window == 0 {
        return false;
    }
    let threshold = window.saturating_mul(threshold_pct.min(100) as u64) / 100;
    input_tokens >= threshold
}

// ---------------------------------------------------------------------------
// Summarisation backends
// ---------------------------------------------------------------------------

// Which endpoint a turn belongs to, and the handle that serves it. Defined
// beside the turn loop in `runner::context` and re-exported here, because a
// caller compacting on demand has to pick the same backend the next turn will
// dispatch through.
pub use crate::runner::context::{
    backend_for, compact_on_demand, dispatches_through_provider, provider_for_turn,
};

/// One model call, no tools, no streaming to anyone.
///
/// What a feature needs when it wants a single answer out of a model rather
/// than a turn: compaction, and session-memory extraction. Neither needs the
/// turn loop's dispatch machinery, only a way to send a prompt and read the
/// text back. Two implementations cover both dispatch arms: the raw Anthropic
/// client, and any registered [`LlmProvider`]. Without this both features were
/// welded to `AnthropicClient`, so every non-Anthropic session compacted never
/// and remembered nothing.
///
/// [`LlmProvider`]: mikmik_api::provider::LlmProvider
#[async_trait::async_trait]
pub trait CompactBackend: Send + Sync {
    /// Send one prompt and return the model's text.
    async fn summarise(
        &self,
        system: &str,
        user: &str,
        model: &WireModel,
        max_tokens: u32,
    ) -> Result<String, ClaudeError>;
}

/// Summarise through the raw Anthropic client.
pub struct AnthropicBackend<'a>(pub &'a mikmik_api::AnthropicClient);

#[async_trait::async_trait]
impl CompactBackend for AnthropicBackend<'_> {
    async fn summarise(
        &self,
        system: &str,
        user: &str,
        model: &WireModel,
        max_tokens: u32,
    ) -> Result<String, ClaudeError> {
        let request = CreateMessageRequest::builder(model, max_tokens)
            .messages(vec![ApiMessage {
                role: "user".to_string(),
                content: Value::String(user.to_string()),
            }])
            .system(SystemPrompt::Text(system.to_string()))
            .build();

        // A null handler: nobody watches a summary stream, only its result.
        let handler: Arc<dyn StreamHandler> = Arc::new(mikmik_api::streaming::NullStreamHandler);
        let mut rx = self.0.create_message_stream(request, handler).await?;
        let mut acc = StreamAccumulator::new();

        while let Some(evt) = rx.recv().await {
            acc.on_event(&evt);
            if matches!(evt, AnthropicStreamEvent::MessageStop) {
                break;
            }
        }

        let (summary_msg, _usage, _stop) = acc.finish();
        Ok(summary_msg.get_all_text())
    }
}

/// Summarise through any registered provider.
///
/// Uses the provider's non-streaming `create_message`, because a summary has
/// no partial output anyone reads.
pub struct ProviderBackend(pub Arc<dyn mikmik_api::provider::LlmProvider>);

#[async_trait::async_trait]
impl CompactBackend for ProviderBackend {
    async fn summarise(
        &self,
        system: &str,
        user: &str,
        model: &WireModel,
        max_tokens: u32,
    ) -> Result<String, ClaudeError> {
        let request = mikmik_api::ProviderRequest {
            model: model.clone(),
            messages: vec![Message::user(user)],
            system_prompt: Some(SystemPrompt::Text(system.to_string())),
            // No tools: a summariser that could call one would compact the
            // conversation by acting on it.
            tools: Vec::new(),
            max_tokens,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            thinking: None,
            provider_options: Value::Object(Default::default()),
        };

        let response = self
            .0
            .create_message(request)
            .await
            .map_err(|e| ClaudeError::Other(format!("Model call failed: {e}")))?;

        Ok(response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""))
    }
}

/// The standing instruction every summariser sends, whichever backend runs.
const COMPACT_SYSTEM_PROMPT: &str =
    "You are a helpful assistant that creates concise yet thorough conversation summaries. \
     Preserve all technical details, file names, code snippets, and decisions that would \
     be important for continuing the work. Follow the structured format exactly.";

// ---------------------------------------------------------------------------
// Core compaction logic
// ---------------------------------------------------------------------------

/// Summarise `messages[..split_at]` using the carefully crafted compaction
/// prompt from TypeScript prompt.ts.
/// Returns a new conversation: [summary user msg] + messages[split_at..].
async fn summarise_head(
    backend: &dyn CompactBackend,
    messages: &[Message],
    split_at: usize,
    model: &WireModel,
    max_summary_tokens: u32,
    custom_instructions: Option<&str>,
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
                    ContentBlock::ToolUse {
                        name, input, id, ..
                    } => {
                        transcript.push_str(&format!(
                            "[Tool Call: {} (id={})]\nInput: {}\n\n",
                            name, id, input
                        ));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let result_text = match content {
                            mikmik_core::types::ToolResultContent::Text(t) => {
                                t.as_str().to_string()
                            }
                            mikmik_core::types::ToolResultContent::Blocks(_) => {
                                "[complex content]".to_string()
                            }
                        };
                        let error_flag = if is_error.unwrap_or(false) {
                            " [ERROR]"
                        } else {
                            ""
                        };
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

    // Feed the prior summary WITHOUT its files-touched manifest: the manifest is
    // re-appended deterministically below (parsed + unioned), so echoing it in
    // the prompt would only risk the model drifting the file list.
    let previous_summary_for_prompt = previous_summary.as_deref().map(strip_files_touched_section);

    // Select the UPDATE prompt variant when a prior summary is present.
    let compact_prompt =
        get_compact_prompt(custom_instructions, previous_summary_for_prompt.as_deref());

    let user_content = if let Some(prev) = previous_summary_for_prompt.as_deref() {
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

    let raw_summary = backend
        .summarise(
            COMPACT_SYSTEM_PROMPT,
            &user_content,
            model,
            max_summary_tokens,
        )
        .await?;

    if raw_summary.is_empty() {
        return Err(ClaudeError::Other("Compact summary was empty".to_string()));
    }

    let formatted_summary = format_compact_summary(&raw_summary);

    // Files-touched manifest: files this batch read/wrote/edited, unioned with
    // any manifest carried in the prior summary so the agent doesn't forget what
    // it worked on across successive compactions. Appended deterministically
    // (bounded via MAX_MANIFEST_FILES) rather than trusting the model.
    let mut file_ops = extract_file_operations(head);
    if let Some(prev) = &previous_summary {
        file_ops.union(&parse_files_touched(prev));
    }
    let formatted_summary = if file_ops.is_empty() {
        formatted_summary
    } else {
        format!("{}{}", formatted_summary, format_files_touched(&file_ops))
    };

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
    backend: &dyn CompactBackend,
    messages: &[Message],
    model: &WireModel,
    custom_instructions: Option<&str>,
) -> Result<Vec<Message>, ClaudeError> {
    let total = messages.len();

    mikmik_plugins::run_global_hook(
        mikmik_plugins::HookEventKind::PreCompact,
        None,
        serde_json::json!({ "message_count": total, "model": model }),
    )
    .await;

    // Free what can be freed without asking a model anything. Re-reads of the
    // same file and repeated search results are the two cheapest wins in a
    // long session, and shrinking them first means the same keep-recent budget
    // holds more real conversation.
    let messages = collapse_search_results(collapse_read_tool_results(messages.to_vec()));
    let messages = messages.as_slice();

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
    let compacted = summarise_head(
        backend,
        messages,
        split_at,
        model,
        20_000,
        custom_instructions,
    )
    .await;

    mikmik_plugins::run_global_hook(
        mikmik_plugins::HookEventKind::PostCompact,
        None,
        serde_json::json!({
            "message_count_before": total,
            "message_count_after": compacted.as_ref().map(|m| m.len()).unwrap_or(total),
            "model": model,
            "ok": compacted.is_ok(),
        }),
    )
    .await;

    compacted
}

/// Whether the context is full enough to compact, and the circuit breaker is
/// not open.
///
/// Split from the attempt below so a caller can decide once and then try more
/// than one summariser. Deciding inside the attempt meant a second try
/// re-answered the threshold question against a conversation the first try had
/// not changed, and counted a second failure for the same overflow.
pub fn should_compact_now(
    input_tokens: u64,
    context_window: u64,
    threshold_pct: u8,
    session_id: &str,
) -> bool {
    let state = compact_state_for(session_id);
    should_auto_compact_for_window(input_tokens, context_window, threshold_pct, &state)
}

/// One compaction attempt, booked against the session's circuit breaker.
///
/// The error is returned rather than swallowed, because a caller with a second
/// summariser to try has to tell "the threshold was never crossed" from "the
/// model that was asked could not answer". Collapsing both into `None` is why
/// a compact model that does not work looked exactly like a conversation that
/// did not need compacting.
pub async fn attempt_compaction(
    backend: &dyn CompactBackend,
    messages: &[Message],
    model: &WireModel,
    instruction: Option<&str>,
    session_id: &str,
) -> Result<Vec<Message>, ClaudeError> {
    info!(model = %model, count = messages.len(), "Compaction attempt");

    match compact_conversation(backend, messages, model, instruction).await {
        Ok(new_msgs) => {
            update_compact_state(session_id, AutoCompactState::on_success);
            info!(
                original_count = messages.len(),
                new_count = new_msgs.len(),
                "Compaction complete"
            );
            Ok(new_msgs)
        }
        Err(e) => {
            warn!(error = %e, model = %model, "Compaction failed");
            update_compact_state(session_id, AutoCompactState::on_failure);
            Err(e)
        }
    }
}

/// A summariser that cannot be reached, so the fallback runs and says why.
///
/// An account with no usable credential is a summariser that will fail. Saying
/// so as a backend keeps one code path instead of two: the caller does not
/// have to decide separately whether to skip the attempt.
pub struct UnreachableBackend;

#[async_trait::async_trait]
impl CompactBackend for UnreachableBackend {
    async fn summarise(
        &self,
        _system: &str,
        _user: &str,
        _model: &WireModel,
        _max_tokens: u32,
    ) -> Result<String, ClaudeError> {
        Err(ClaudeError::Other("no usable credential".to_string()))
    }
}

/// Who writes a summary, and over which endpoint.
pub struct Summariser<'a> {
    pub backend: &'a dyn CompactBackend,
    pub route: &'a mikmik_core::config::Route,
}

/// What one compaction came to, and what to tell the user about it.
pub struct CompactionRun {
    pub result: Result<Vec<Message>, ClaudeError>,
    /// Set when the chosen summariser could not be used and the fallback
    /// wrote the summary instead.
    pub note: Option<String>,
}

/// Summarise on `chosen`, falling back once to `fallback` and saying so.
///
/// One place rather than three, because every surface that compacts has the
/// same decision to make. A compact model that cannot answer must not mean no
/// compaction at all: the context still has to come down, and a session that
/// stops compacting over a mistyped setting fails in a way nobody sees until
/// the provider refuses the prompt. Equally, honouring a setting that does not
/// work is not honouring it, so the substitution is reported.
///
/// `fallback` is `None` when `chosen` is already the turn's own model, which
/// leaves nothing to fall back to.
pub async fn compact_with_fallback(
    chosen: Summariser<'_>,
    fallback: Option<Summariser<'_>>,
    messages: &[Message],
    instruction: Option<&str>,
    session_id: &str,
) -> CompactionRun {
    let first = attempt_compaction(
        chosen.backend,
        messages,
        &chosen.route.model,
        instruction,
        session_id,
    )
    .await;

    let (Err(error), Some(fallback)) = (&first, fallback) else {
        return CompactionRun {
            result: first,
            note: None,
        };
    };

    let note = format!(
        "Compact model '{}' on account '{}' is unavailable ({error}); \
         summarised with '{}' instead.",
        chosen.route.model, chosen.route.account, fallback.route.model
    );

    CompactionRun {
        result: attempt_compaction(
            fallback.backend,
            messages,
            &fallback.route.model,
            instruction,
            session_id,
        )
        .await,
        note: Some(note),
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
    TokenThreshold {
        tokens_used: u64,
        context_limit: u64,
    },
    /// Caller requested an unconditional compact.
    Forced,
}

/// Result returned by `reactive_compact` and `context_collapse`.
#[derive(Debug, Clone)]
pub struct CompactResult {
    /// The new (reduced) message list.
    pub messages: Vec<mikmik_core::types::Message>,
    /// Formatted summary text injected at the head of `messages`.
    pub summary: String,
    /// Rough estimate of how many tokens were freed.
    pub tokens_freed: u64,
}

/// Return `true` when reactive compact should fire.
///
/// Takes the same `threshold_pct` as [`should_auto_compact_for_window`], so
/// the user's setting means the same thing whichever of the two paths the
/// `CLAUDE_REACTIVE_COMPACT` gate selects.
pub fn should_compact(tokens_used: u64, context_limit: u64, threshold_pct: u8) -> bool {
    if context_limit == 0 {
        return false;
    }
    let threshold = context_limit.saturating_mul(threshold_pct.min(100) as u64) / 100;
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

/// Compute the index into `messages` such that the tail starting at that
/// index fits within `token_budget` tokens.
///
/// Returns the cut index (0 = keep everything, messages.len() = keep nothing).
/// Iterates from the newest message backwards, accumulating token estimates
/// until the budget is exhausted.
pub fn calculate_messages_to_keep_index(
    messages: &[mikmik_core::types::Message],
    token_budget: u64,
) -> usize {
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
fn strip_images(messages: Vec<mikmik_core::types::Message>) -> Vec<mikmik_core::types::Message> {
    use mikmik_core::types::{ContentBlock, MessageContent};

    messages
        .into_iter()
        .map(|mut msg| {
            if let MessageContent::Blocks(ref mut blocks) = msg.content {
                blocks.retain(|b| !matches!(b, ContentBlock::Image { .. }));
                // If stripping left only an empty block list, collapse to a
                // placeholder text so the conversation remains parseable.
                if blocks.is_empty() {
                    msg.content =
                        MessageContent::Text("[image removed for compaction]".to_string());
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
/// `mikmik_core::feature_gates::is_feature_enabled("reactive_compact")` is true.
///
/// The `cancel` token is checked before the API call so the user can abort
/// a long-running compact.
pub async fn reactive_compact(
    messages: Vec<mikmik_core::types::Message>,
    backend: &dyn CompactBackend,
    model: &WireModel,
    cancel: tokio_util::sync::CancellationToken,
    recently_modified: &[std::path::PathBuf],
) -> Result<CompactResult, mikmik_core::error::ClaudeError> {
    if cancel.is_cancelled() {
        return Err(mikmik_core::error::ClaudeError::Cancelled);
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

    let original_token_estimate = estimate_tokens_for_messages(&stripped[..split_at]) as u64;

    let mut new_messages =
        summarise_head(backend, &stripped, split_at, model, 20_000, None).await?;

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
        new_messages.push(mikmik_core::types::Message::user(text));
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
    messages: Vec<mikmik_core::types::Message>,
    backend: &dyn CompactBackend,
    model: &WireModel,
) -> Result<CompactResult, mikmik_core::error::ClaudeError> {
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
            mikmik_core::types::Role::User => "Human",
            mikmik_core::types::Role::Assistant => "Assistant",
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

    let summary_text = backend
        .summarise(
            "You are a conversation summariser. Produce an emergency ultra-short \
             summary as instructed. Plain text only.",
            &collapse_prompt,
            model,
            1_000,
        )
        .await?;

    if summary_text.is_empty() {
        return Err(mikmik_core::error::ClaudeError::Other(
            "Context-collapse summary was empty".to_string(),
        ));
    }

    // Keep only: the synthetic summary + the most recent user turn.
    let collapse_notice = mikmik_core::types::Message::user(format!(
        "[EMERGENCY CONTEXT COLLAPSE — conversation condensed to stay within limits]\n\n{}",
        summary_text
    ));

    // Find the last user message in the original list.
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == mikmik_core::types::Role::User)
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

/// Context collapse (emergency) fires at 97 % of the context window.
///
/// A constant and not the user's `compactThreshold`: this is the last thing
/// standing between the session and a prompt the provider refuses, so it is
/// not the user's to move.
const CONTEXT_COLLAPSE_THRESHOLD: f64 = 0.97;

// ---------------------------------------------------------------------------
// T4-5: Collapse read/search results (mirrors src/utils/collapseReadSearch.ts)
// ---------------------------------------------------------------------------

/// Replace repeated reads of the same file with a single summary.
///
/// When the same file is read more than once in the conversation, replaces
/// all but the last read with `[Content shown N time(s); showing last occurrence only]`.
pub fn collapse_read_tool_results(
    messages: Vec<mikmik_core::types::Message>,
) -> Vec<mikmik_core::types::Message> {
    use mikmik_core::types::{ContentBlock, MessageContent, ToolResultContent};
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
pub fn collapse_search_results(
    messages: Vec<mikmik_core::types::Message>,
) -> Vec<mikmik_core::types::Message> {
    use mikmik_core::types::{ContentBlock, MessageContent, ToolResultContent};
    use std::collections::HashSet;

    fn fingerprint(content: &ToolResultContent) -> Option<String> {
        match content {
            ToolResultContent::Text(t) => Some(t.chars().take(200).collect()),
            ToolResultContent::Blocks(_) => None,
        }
    }

    let mut seen_results: HashSet<String> = HashSet::new();

    // Iterate in reverse to keep the latest occurrence.
    let mut result: Vec<mikmik_core::types::Message> = messages
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
    use mikmik_core::types::{Message, ToolResultContent};

    fn make_user(text: &str) -> Message {
        Message::user(text)
    }

    fn make_assistant(text: &str) -> Message {
        // No UUID set — relies on the no-UUID grouping path in group_messages_for_compact.
        Message::assistant(text)
    }

    /// `n` bytes of filler text (≈ `n/4 * 4/3` tokens under the chars/4 estimate).
    fn filler(n: usize) -> String {
        "x".repeat(n)
    }

    // ---- Summarisation backends ---------------------------------------------

    /// A backend that answers with a fixed summary and records what it was
    /// asked, so a test can read the prompt without reaching the network.
    struct RecordingBackend {
        reply: String,
        seen: parking_lot::Mutex<Option<(String, String, String, u32)>>,
    }

    impl RecordingBackend {
        fn new(reply: &str) -> Self {
            Self {
                reply: reply.to_string(),
                seen: parking_lot::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl CompactBackend for RecordingBackend {
        async fn summarise(
            &self,
            system: &str,
            user: &str,
            model: &WireModel,
            max_tokens: u32,
        ) -> Result<String, ClaudeError> {
            *self.seen.lock() = Some((
                system.to_string(),
                user.to_string(),
                model.to_string(),
                max_tokens,
            ));
            Ok(self.reply.clone())
        }
    }

    /// Compaction asks the backend for one summary and rebuilds the
    /// conversation around it, whichever backend that is.
    #[tokio::test]
    async fn compaction_summarises_the_head_through_the_backend() {
        let mut messages = vec![make_user(&filler(80_000)), make_assistant(&filler(80_000))];
        messages.push(make_user("what next"));

        let backend = RecordingBackend::new("Summary of the earlier work.");
        let out =
            compact_conversation(&backend, &messages, &WireModel::literal("some-model"), None)
                .await
                .expect("compaction succeeds");

        let (system, user, model, max_tokens) =
            backend.seen.lock().clone().expect("the backend was called");
        assert_eq!(model, "some-model");
        assert_eq!(max_tokens, 20_000);
        assert!(system.contains("conversation summaries"));
        assert!(user.contains("<conversation_to_summarize"));

        assert!(out.len() < messages.len(), "the head was replaced");
        assert!(
            out[0]
                .get_all_text()
                .contains("Summary of the earlier work."),
            "the summary leads the new conversation"
        );
    }

    /// Three reads of one file, sized so the raw conversation is over the
    /// keep-recent budget and the collapsed one is under it.
    fn a_file_read_three_times() -> Vec<Message> {
        let body = filler(30_000);
        let mut messages = vec![make_user("read that file a few times")];
        for id in ["t1", "t2", "t3"] {
            messages.push(assistant_tool_use(
                id,
                "Read",
                serde_json::json!({ "file_path": "/a.rs" }),
            ));
            messages.push(Message::user_blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: ToolResultContent::Text(body.clone()),
                is_error: Some(false),
            }]));
        }
        messages.push(make_user("now carry on"));
        messages
    }

    /// Re-reads of the same file are collapsed before anything reaches a
    /// model. It costs no API call, and the same keep-recent budget then holds
    /// more real conversation.
    ///
    /// Sized so the point is unambiguous: uncollapsed the conversation is over
    /// the budget and would be summarised, collapsed it fits and no summary is
    /// asked for at all.
    #[tokio::test]
    async fn repeated_reads_are_collapsed_before_summarising() {
        let messages = a_file_read_three_times();

        // Without the collapse the head would have to be summarised.
        assert!(
            compute_keep_split_index(&messages, KEEP_RECENT_TOKENS) > 0,
            "the raw conversation is over the keep-recent budget"
        );

        let backend = RecordingBackend::new("never asked for");
        let out =
            compact_conversation(&backend, &messages, &WireModel::literal("some-model"), None)
                .await
                .expect("compaction succeeds");

        assert!(
            backend.seen.lock().is_none(),
            "collapsing alone brought it under budget, so no summary was needed"
        );
        assert_eq!(out.len(), messages.len(), "every turn is still there");

        let kept: String = out
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::Blocks(blocks) => Some(
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolResult {
                                content: ToolResultContent::Text(t),
                                ..
                            } => Some(t.clone()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect();
        assert!(
            kept.contains("[Content shown"),
            "the earlier reads were replaced by a marker"
        );
        assert!(
            estimate_tokens_for_messages(&out) < estimate_tokens_for_messages(&messages),
            "the conversation got smaller"
        );
    }

    /// `/compact <instruction>` reaches the summariser's prompt.
    #[tokio::test]
    async fn a_custom_instruction_reaches_the_prompt() {
        let messages = vec![
            make_user(&filler(80_000)),
            make_assistant(&filler(80_000)),
            make_user("what next"),
        ];

        let backend = RecordingBackend::new("Short.");
        compact_conversation(
            &backend,
            &messages,
            &WireModel::literal("some-model"),
            Some("keep every file path"),
        )
        .await
        .expect("compaction succeeds");

        let (_system, user, _model, _max) = backend.seen.lock().clone().expect("called");
        assert!(
            user.contains("keep every file path"),
            "the instruction is in the prompt"
        );
    }

    /// An empty answer is a failed compaction, not a conversation with the
    /// head silently thrown away.
    #[tokio::test]
    async fn an_empty_summary_fails_instead_of_dropping_the_head() {
        let messages = vec![
            make_user(&filler(80_000)),
            make_assistant(&filler(80_000)),
            make_user("what next"),
        ];

        let backend = RecordingBackend::new("");
        let err =
            compact_conversation(&backend, &messages, &WireModel::literal("some-model"), None)
                .await
                .expect_err("an empty summary is an error");
        assert!(err.to_string().contains("empty"));
    }

    /// An assistant message carrying a single `tool_use` block.
    fn assistant_tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message::assistant_blocks(vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
            thought_signature: None,
        }])
    }

    /// A user message carrying a single `tool_result` block answering `id`.
    fn user_tool_result(id: &str, text: &str) -> Message {
        Message::user_blocks(vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: ToolResultContent::Text(text.to_string()),
            is_error: None,
        }])
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

    // ---- should_auto_compact_for_window ------------------------------------

    /// The default threshold, as `Config::effective_compact_threshold` gives it.
    const DEFAULT_PCT: u8 = mikmik_core::constants::DEFAULT_COMPACT_THRESHOLD;

    #[test]
    fn test_should_not_compact_when_disabled() {
        let state = AutoCompactState {
            disabled: true,
            ..Default::default()
        };
        assert!(!should_auto_compact_for_window(
            195_000,
            200_000,
            DEFAULT_PCT,
            &state
        ));
    }

    #[test]
    fn test_should_compact_at_90pct() {
        let state = AutoCompactState::default();
        // 90 % of 200k = 180k — should trigger
        assert!(should_auto_compact_for_window(
            180_000,
            200_000,
            DEFAULT_PCT,
            &state
        ));
    }

    #[test]
    fn test_should_not_compact_below_90pct() {
        let state = AutoCompactState::default();
        // 70 % of 200k = 140k — should NOT trigger
        assert!(!should_auto_compact_for_window(
            140_000,
            200_000,
            DEFAULT_PCT,
            &state
        ));
    }

    /// The user's `compactThreshold` decides when, which is the whole point of
    /// making it a parameter: at 50 the same 140k prompt now compacts.
    #[test]
    fn the_users_threshold_moves_the_trigger() {
        let state = AutoCompactState::default();
        assert!(should_auto_compact_for_window(140_000, 200_000, 50, &state));
        assert!(!should_auto_compact_for_window(90_000, 200_000, 50, &state));
    }

    /// A threshold above 100 would mean the context must overflow first.
    #[test]
    fn a_threshold_over_a_hundred_is_clamped() {
        let state = AutoCompactState::default();
        assert!(should_auto_compact_for_window(
            200_000, 200_000, 250, &state
        ));
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

    /// Failures accumulate across separate `run_query_loop` calls, because the
    /// state is keyed by session and not scoped to one prompt. Three failures
    /// spread over three user messages open the breaker, which is the case the
    /// loop-local value could never reach.
    #[test]
    fn the_breaker_counts_failures_across_prompts() {
        let session = "breaker-across-prompts";
        forget_compact_state(session);

        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            assert!(!compact_state_for(session).disabled);
            update_compact_state(session, AutoCompactState::on_failure);
        }

        assert!(compact_state_for(session).disabled);
        forget_compact_state(session);
    }

    /// One session's failures never reach another's.
    #[test]
    fn a_breaker_stays_inside_its_own_session() {
        let failing = "breaker-isolation-failing";
        let healthy = "breaker-isolation-healthy";
        forget_compact_state(failing);
        forget_compact_state(healthy);

        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            update_compact_state(failing, AutoCompactState::on_failure);
        }

        assert!(compact_state_for(failing).disabled);
        assert!(!compact_state_for(healthy).disabled);

        forget_compact_state(failing);
        forget_compact_state(healthy);
    }

    /// A session that ends leaves nothing behind, so a reused id starts clean.
    #[test]
    fn forgetting_a_session_clears_its_breaker() {
        let session = "breaker-forgotten";
        for _ in 0..MAX_CONSECUTIVE_FAILURES {
            update_compact_state(session, AutoCompactState::on_failure);
        }
        assert!(compact_state_for(session).disabled);

        forget_compact_state(session);
        assert!(!compact_state_for(session).disabled);
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
    fn registry_from_json(json: &str) -> mikmik_api::ModelRegistry {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models_dev.json");
        std::fs::write(&path, json).expect("write snapshot");
        let mut reg = mikmik_api::ModelRegistry::new();
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
            reg.get("testprov", "big-context-model")
                .unwrap()
                .info
                .context_window,
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
        assert_eq!(
            resolve_context_window(None, "anthropic", "claude-opus-4-8"),
            200_000
        );
        assert_eq!(
            resolve_context_window(None, "some-provider", "some-model"),
            100_000
        );
    }

    #[test]
    fn resolve_falls_back_to_heuristic_when_no_registry_entry() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // Provider/model that isn't in the registry → heuristic default.
        assert_eq!(
            resolve_context_window(Some(&reg), "nope", "ghost-model"),
            context_window_for_model("ghost-model")
        );
        assert_eq!(
            resolve_context_window(Some(&reg), "nope", "ghost-model"),
            100_000
        );
    }

    #[test]
    fn resolve_ignores_placeholder_4096_window() {
        let reg = registry_from_json(TEST_SNAPSHOT);
        // The registry stores the models.dev-omission placeholder (4096)...
        assert_eq!(
            reg.get("testprov", "tiny-model")
                .unwrap()
                .info
                .context_window,
            4096
        );
        // ...but resolve treats it as "unknown" and uses the heuristic instead
        // of compacting a real session at ~3.7k tokens.
        assert_eq!(
            resolve_context_window(Some(&reg), "testprov", "tiny-model"),
            context_window_for_model("tiny-model")
        );
        assert_eq!(
            resolve_context_window(Some(&reg), "testprov", "tiny-model"),
            100_000
        );
    }

    // ---- estimate_tokens_for_messages --------------------------------------

    #[test]
    fn test_token_estimate_nonempty() {
        let msgs = vec![make_user("Hello, world!")];
        let est = estimate_tokens_for_messages(&msgs);
        // "Hello, world!" = 13 chars → 13/4 = 3 rough tokens → 3*4/3 = 4 padded
        assert!(est > 0);
    }

    // ---- (1) token-budget keep (#231) --------------------------------------

    fn plain_convo(n: usize, size: usize) -> Vec<Message> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(filler(size))
                } else {
                    Message::assistant(filler(size))
                }
            })
            .collect()
    }

    #[test]
    fn keep_split_keeps_more_as_budget_grows() {
        // Eight plain messages, each filler(4000) ≈ 1333 tokens.
        let msgs = plain_convo(8, 4000);

        // Small budget keeps ~1 message; larger budget keeps ~3.
        let split_small = compute_keep_split_index(&msgs, 2000);
        let split_large = compute_keep_split_index(&msgs, 5000);

        // A larger budget keeps MORE messages ⇒ a smaller split index.
        assert!(
            split_large < split_small,
            "bigger budget must keep more (split_large={split_large}, split_small={split_small})"
        );
        assert_eq!(msgs.len() - split_small, 1, "2k budget keeps 1 message");
        assert_eq!(msgs.len() - split_large, 3, "5k budget keeps 3 messages");

        // Neither cut lands on a tool_result (trivially true for plain text).
        assert!(!message_has_tool_result(&msgs[split_small]));
        assert!(!message_has_tool_result(&msgs[split_large]));
    }

    #[test]
    fn keep_split_snaps_off_tool_result_boundary() {
        // A round whose tool_use is huge, so the raw budget cut lands right on
        // the tool_result — which would orphan it from its call.
        let msgs = vec![
            Message::user(filler(400)), // 0
            assistant_tool_use("t1", "Bash", serde_json::json!({ "command": filler(8000) })), // 1 (BIG)
            user_tool_result("t1", &filler(400)), // 2 (tool_result)
            Message::assistant(filler(400)),      // 3
        ];

        // Raw token-budget index lands ON the tool_result (index 2).
        let raw = calculate_messages_to_keep_index(&msgs, 500);
        assert_eq!(raw, 2, "raw cut should land on the tool_result message");
        assert!(message_has_tool_result(&msgs[raw]));

        // The pairing-safe keep snaps back to the assistant tool_use boundary,
        // so the kept tail contains BOTH the tool_use and its result.
        let split = compute_keep_split_index(&msgs, 500);
        assert_eq!(split, 1);
        assert!(!message_has_tool_result(&msgs[split]));
        // tail = msgs[1..] carries tool_use(t1) AND its tool_result(t1).
        let tail = &msgs[split..];
        assert!(tail.iter().any(|m| !m.get_tool_use_blocks().is_empty()));
        assert!(tail.iter().any(message_has_tool_result));
    }

    #[test]
    fn snap_to_pairing_boundary_handles_keep_nothing() {
        let msgs = plain_convo(3, 100);
        // idx == len (keep nothing) is left as-is — the tail is empty, nothing to orphan.
        assert_eq!(snap_to_pairing_boundary(&msgs, msgs.len()), msgs.len());
    }

    // ---- (2) real-usage trigger (#231) -------------------------------------

    #[test]
    fn context_tokens_prefer_real_usage_over_estimate() {
        let msgs = vec![Message::user(filler(4000))]; // ≈ 1333 estimated tokens
        let estimate = estimate_tokens_for_messages(&msgs) as u64;

        // Real usage present ⇒ used verbatim, ignoring the (much smaller) estimate.
        assert_eq!(estimate_context_tokens(&msgs, Some(150_000)), 150_000);
        assert_ne!(estimate_context_tokens(&msgs, Some(150_000)), estimate);

        // No usage / zero usage ⇒ fall back to the chars/4 estimate.
        assert_eq!(estimate_context_tokens(&msgs, None), estimate);
        assert_eq!(estimate_context_tokens(&msgs, Some(0)), estimate);
    }

    // ---- (3) iterative UPDATE prompt (#231) --------------------------------

    #[test]
    fn extract_previous_summary_finds_compact_summary_block() {
        let notice = Message::user(
            "This session is being continued from a previous conversation.\n\n\
             <compact-summary>\nSummary:\n1. Primary Request: build X\n</compact-summary>",
        );
        let msgs = vec![notice, make_assistant("ok"), make_user("next")];
        let prev = extract_previous_summary(&msgs).expect("should detect prior summary");
        assert!(prev.contains("Primary Request: build X"));
        assert!(!prev.contains("<compact-summary>"));

        // No summary block ⇒ None.
        assert!(extract_previous_summary(&[make_user("hello"), make_assistant("hi")]).is_none());
    }

    #[test]
    fn update_prompt_selected_only_with_previous_summary() {
        let base = get_compact_prompt(None, None);
        let update = get_compact_prompt(None, Some("Summary:\n1. Primary Request: build X"));

        // UPDATE variant is distinct and references the previous summary.
        assert!(update.contains("UPDATE an existing conversation summary"));
        assert!(update.contains("<previous-summary>"));
        assert!(!base.contains("UPDATE an existing conversation summary"));

        // A blank previous summary is treated as "no previous summary".
        let blank = get_compact_prompt(None, Some("   "));
        assert_eq!(blank, base);

        // Both variants preserve the structured sections.
        for p in [&base, &update] {
            assert!(p.contains("Primary Request and Intent"));
            assert!(p.contains("Files and Code Sections"));
            assert!(p.contains("Pending Tasks"));
            assert!(p.contains("Optional Next Step"));
        }
    }

    // ---- (4) files-touched manifest (#231) ---------------------------------

    fn read_use(id: &str, path: &str) -> Message {
        assistant_tool_use(id, "Read", serde_json::json!({ "file_path": path }))
    }
    fn edit_use(id: &str, path: &str) -> Message {
        assistant_tool_use(id, "Edit", serde_json::json!({ "file_path": path }))
    }
    fn write_use(id: &str, path: &str) -> Message {
        assistant_tool_use(id, "Write", serde_json::json!({ "file_path": path }))
    }

    #[test]
    fn manifest_lists_read_write_edit_files() {
        let msgs = vec![
            read_use("1", "/repo/a.rs"),
            edit_use("2", "/repo/b.rs"),
            write_use("3", "/repo/c.rs"),
            read_use("4", "/repo/a.rs"), // duplicate read — deduped
        ];
        let ops = extract_file_operations(&msgs);
        let manifest = format_files_touched(&ops);

        assert!(manifest.contains(FILES_TOUCHED_HEADER));
        // b.rs (edit) and c.rs (write) are "Modified"; a.rs is "Read".
        assert!(manifest.contains("Modified:"));
        assert!(manifest.contains("/repo/b.rs"));
        assert!(manifest.contains("/repo/c.rs"));
        assert!(manifest.contains("Read:"));
        assert!(manifest.contains("/repo/a.rs"));
    }

    #[test]
    fn manifest_edit_wins_over_read_for_same_file() {
        // A file that was both read and edited is reported only as Modified.
        let msgs = vec![read_use("1", "/repo/x.rs"), edit_use("2", "/repo/x.rs")];
        let (read_only, modified) = extract_file_operations(&msgs).computed_lists();
        assert_eq!(modified, vec!["/repo/x.rs".to_string()]);
        assert!(read_only.is_empty());
    }

    #[test]
    fn manifest_unions_with_prior_and_roundtrips() {
        // New batch touches new.rs (edit) and read_new.rs (read).
        let msgs = vec![
            edit_use("1", "/repo/new.rs"),
            read_use("2", "/repo/read_new.rs"),
        ];
        let mut ops = extract_file_operations(&msgs);

        // Prior manifest, as it would appear inside a previous <compact-summary>.
        let prior = "Summary:\n1. Primary Request: ...\n\nFiles touched:\n\
                     Modified: /repo/old.rs\nRead: /repo/read_old.rs";
        let parsed = parse_files_touched(prior);
        assert!(parsed.edited.contains("/repo/old.rs"));
        assert!(parsed.read.contains("/repo/read_old.rs"));

        ops.union(&parsed);
        let manifest = format_files_touched(&ops);

        // Both prior and new files survive the carry-forward.
        for f in [
            "/repo/new.rs",
            "/repo/old.rs",
            "/repo/read_new.rs",
            "/repo/read_old.rs",
        ] {
            assert!(manifest.contains(f), "manifest missing {f}:\n{manifest}");
        }

        // strip_files_touched_section removes the manifest for the UPDATE prompt.
        let stripped = strip_files_touched_section(prior);
        assert!(!stripped.contains(FILES_TOUCHED_HEADER));
        assert!(stripped.contains("Primary Request"));
    }

    #[test]
    fn manifest_is_bounded_with_overflow_marker() {
        // 25 edited files ⇒ list is capped at MAX_MANIFEST_FILES (20) with "+5 more".
        let msgs: Vec<Message> = (0..(MAX_MANIFEST_FILES + 5))
            .map(|i| edit_use(&format!("id{i}"), &format!("/repo/file_{i:02}.rs")))
            .collect();
        let manifest = format_files_touched(&extract_file_operations(&msgs));

        assert!(
            manifest.contains("(+5 more)"),
            "expected overflow marker:\n{manifest}"
        );
        // Exactly MAX_MANIFEST_FILES paths are shown before the marker.
        let modified_line = manifest
            .lines()
            .find(|l| l.starts_with("Modified:"))
            .expect("Modified line");
        let listed = modified_line
            .trim_start_matches("Modified:")
            .split(" (+")
            .next()
            .unwrap()
            .matches(MANIFEST_SEP)
            .count()
            + 1;
        assert_eq!(listed, MAX_MANIFEST_FILES);
    }
}
