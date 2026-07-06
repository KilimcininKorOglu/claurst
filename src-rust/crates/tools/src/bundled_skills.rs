//! Bundled skill definitions for the Skill tool.
//!
//! Each entry in `BUNDLED_SKILLS` mirrors one of the TypeScript
//! `registerXxxSkill()` calls under `src/skills/bundled/`.  Only publicly
//! invocable, user-facing skills are included; internal or ANT-only skills
//! (stuck, remember, verify) are omitted from the user-visible list but are
//! still present as documentation stubs so callers can discover them.
//!
//! The `SkillTool` checks bundled skills *before* scanning disk directories,
//! so bundled names take precedence over same-named `.md` files.

/// A single bundled skill definition.
#[derive(Debug, Clone)]
pub struct BundledSkill {
    /// Primary name used to invoke the skill (e.g. `"simplify"`).
    pub name: &'static str,
    /// One-line description shown in `/skill list` output and to the model.
    pub description: &'static str,
    /// Additional names that map to this skill.
    pub aliases: &'static [&'static str],
    /// Optional guidance for the model about when to auto-invoke.
    pub when_to_use: Option<&'static str>,
    /// Placeholder shown next to the skill name in help text.
    pub argument_hint: Option<&'static str>,
    /// The prompt template.  `$ARGUMENTS` is replaced at call time.
    /// `$ARGUMENTS_SUFFIX` expands to `": <args>"` when args are non-empty,
    /// or `""` otherwise.
    pub prompt_template: &'static str,
    /// If `Some`, only these tool names are available during the skill run.
    pub allowed_tools: Option<&'static [&'static str]>,
    /// Whether a human user can invoke this skill via `/skill <name>`.
    pub user_invocable: bool,
}

/// All bundled skills.
pub const BUNDLED_SKILLS: &[BundledSkill] = &[
    // -----------------------------------------------------------------------
    // simplify
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "simplify",
        description: "Review changed code for reuse, quality, and efficiency, then fix any issues found.",
        aliases: &[],
        when_to_use: Some("After writing code, when you want a quality review and cleanup pass."),
        argument_hint: None,
        prompt_template: r#"# Simplify: Code Review and Cleanup

Review all changed files for reuse, quality, and efficiency. Fix any issues found.

## Phase 1: Identify Changes

Run `git diff` (or `git diff HEAD` if there are staged changes) to see what changed.
If there are no git changes, review the most recently modified files that were
mentioned or edited earlier in this conversation.

## Phase 2: Launch Three Review Agents in Parallel

Use the Agent tool to launch all three agents concurrently in a single message.
Pass each agent the full diff so it has complete context.

### Agent 1: Code Reuse Review

For each change:
1. **Search for existing utilities and helpers** that could replace newly written code.
2. **Flag any new function that duplicates existing functionality.**
3. **Flag any inline logic that could use an existing utility** — hand-rolled string
   manipulation, manual path handling, custom environment checks, etc.

### Agent 2: Code Quality Review

Review the same changes for hacky patterns:
1. **Redundant state** that duplicates existing state.
2. **Parameter sprawl** — new parameters instead of restructuring.
3. **Copy-paste with slight variation** that should be unified.
4. **Leaky abstractions** — exposing internal details.
5. **Stringly-typed code** where constants or enums already exist.
6. **Unnecessary comments** narrating what code does (not why).

### Agent 3: Efficiency Review

Review the same changes for efficiency:
1. **Unnecessary work** — redundant computations, duplicate reads.
2. **Missed concurrency** — independent operations run sequentially.
3. **Hot-path bloat** — blocking work added to startup or per-request paths.
4. **Recurring no-op updates** — unconditional updates in polling loops.
5. **Memory** — unbounded data structures, missing cleanup.

## Phase 3: Fix Issues

Wait for all three agents to complete. Aggregate findings and fix each issue.
If a finding is a false positive, note it and move on.

When done, briefly summarize what was fixed (or confirm the code was already clean).
$ARGUMENTS_SUFFIX"#,
        allowed_tools: None,
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // remember
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "remember",
        description: "Review auto-memory entries and propose promotions to AGENTS.md, AGENTS.local.md, or shared memory.",
        aliases: &["mem", "save"],
        when_to_use: Some("When the user wants to review, organise, or promote their auto-memory entries."),
        argument_hint: Some("[additional context]"),
        prompt_template: r#"# Memory Review

## Goal
Review the user's memory landscape and produce a clear report of proposed changes,
grouped by action type. Do NOT apply changes — present proposals for user approval.

## Steps

### 1. Gather all memory layers
Read AGENTS.md and AGENTS.local.md from the project root (if they exist).
Your auto-memory content is already in your system prompt — review it there.

### 2. Classify each auto-memory entry

| Destination | What belongs there |
|---|---|
| **AGENTS.md** | Project conventions all contributors should follow |
| **AGENTS.local.md** | Personal instructions specific to this user |
| **Stay in auto-memory** | Working notes, temporary context, uncertain patterns |

### 3. Identify cleanup opportunities
- **Duplicates**: auto-memory entries already in AGENTS.md → propose removing
- **Outdated**: AGENTS.md entries contradicted by newer auto-memory → propose updating
- **Conflicts**: contradictions between layers → propose resolution

### 4. Present the report
Output a structured report grouped by: Promotions, Cleanup, Ambiguous, No action needed.

## Rules
- Present ALL proposals before making any changes
- Do NOT modify files without explicit user approval
- Ask about ambiguous entries — don't guess
$ARGUMENTS_SUFFIX"#,
        allowed_tools: Some(&["Read", "Write", "Edit", "Glob"]),
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // debug
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "debug",
        description: "Enable debug logging for this session and help diagnose issues.",
        aliases: &["diagnose"],
        when_to_use: Some("When there is an error, bug, or unexpected behaviour to investigate."),
        argument_hint: Some("[issue description or error message]"),
        prompt_template: r#"# Debug Skill

Help the user debug an issue they are encountering.

## Issue Description

$ARGUMENTS

## Systematic Debugging Approach

1. **Reproduce** — Confirm the exact error / behaviour.
2. **Locate** — Find the relevant code (read files, grep for error messages).
3. **Hypothesize** — Form 2–3 hypotheses about the root cause.
4. **Test** — Verify each hypothesis systematically.
5. **Fix** — Implement the fix for the confirmed root cause.
6. **Verify** — Confirm the fix resolves the issue.

## Settings Reference

Settings files are in:
- User:    ~/.claurst/settings.json
- Project: .claurst/settings.json
- Local:   .claurst/settings.local.json

Read the relevant files before making any changes."#,
        allowed_tools: Some(&["Read", "Grep", "Glob"]),
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // stuck
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "stuck",
        description: "Help get unstuck when you don't know how to proceed.",
        aliases: &["help-me", "unblock"],
        when_to_use: Some("When you are stuck, confused, or don't know how to proceed."),
        argument_hint: Some("[what you're trying to do]"),
        prompt_template: r#"The user is stuck$ARGUMENTS_SUFFIX. Help them get unstuck:

1. Clarify what they are trying to achieve (if unclear).
2. Identify why they might be stuck (missing context, unclear requirements, technical blocker).
3. Suggest 2–3 concrete next steps in order of likelihood of success.
4. If a technical blocker: propose specific debugging steps or workarounds.
5. Ask clarifying questions if needed.

Be direct and actionable. Focus on unblocking, not on explaining concepts."#,
        allowed_tools: None,
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // batch
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "batch",
        description: "Research and plan a large-scale change, then execute it in parallel across isolated worktree agents that each open a PR.",
        aliases: &[],
        when_to_use: Some("When the user wants to make a sweeping, mechanical change across many files that can be decomposed into independent parallel units."),
        argument_hint: Some("<instruction>"),
        prompt_template: r#"# Batch: Parallel Work Orchestration

You are orchestrating a large, parallelisable change across this codebase.

## User Instruction

$ARGUMENTS

## Phase 1: Research and Plan (Plan Mode)

Enter plan mode, then:

1. **Understand the scope.** Launch subagents to deeply research what this instruction
   touches. Find all files, patterns, and call sites that need to change.

2. **Decompose into independent units.** Break the work into 5–30 self-contained units.
   Each unit must be independently implementable in an isolated git worktree and
   mergeable on its own without depending on another unit's PR landing first.

3. **Determine the e2e test recipe.** Figure out how a worker can verify its change
   actually works end-to-end. If you cannot find a concrete path, ask the user.

4. **Write the plan.** Include: research summary, numbered work units, e2e recipe,
   and the exact worker instructions.

## Phase 2: Spawn Workers (After Plan Approval)

Spawn one background agent per work unit using the Agent tool with
`isolation: "worktree"` and `run_in_background: true`. Launch them all in a single
message block so they run in parallel. Each agent prompt must be fully self-contained.

After each agent finishes, parse the `PR: <url>` line from its result and render
a status table. When all agents have reported, print a final summary."#,
        allowed_tools: None,
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // verify
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "verify",
        description: "Verify that code or behaviour is correct.",
        aliases: &["check", "validate"],
        when_to_use: Some("After implementing something, to verify it is correct."),
        argument_hint: Some("[what to verify]"),
        prompt_template: r#"# Verify: $ARGUMENTS

## Verification Steps

1. Read the relevant code / implementation.
2. Check against requirements (if specified).
3. Look for edge cases and error conditions.
4. Run tests if available.
5. Check for common pitfalls: null handling, error propagation, type safety.
6. Report: what was verified, what passed, what failed or is uncertain."#,
        allowed_tools: None,
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // update-config
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "update-config",
        description: "Configure Claurst settings (hooks, permissions, env vars, behaviours) via settings.json.",
        aliases: &["config-update", "settings"],
        when_to_use: Some("When the user wants to configure automated behaviours, permissions, or settings."),
        argument_hint: Some("<what to configure>"),
        prompt_template: r#"# Update Config Skill

Modify Claurst configuration by updating settings.json files.

## Settings File Locations

| File | Scope | Use For |
|------|-------|---------|
| `~/.claurst/settings.json` | Global | Personal preferences for all projects |
| `.claurst/settings.json` | Project | Team-wide hooks, permissions, plugins |
| `.claurst/settings.local.json` | Project (local) | Personal overrides for this project |

Settings load in order: user → project → local (later overrides earlier).

## CRITICAL: Read Before Write

Always read the existing settings file before making changes.
Merge new settings with existing ones — never replace the entire file.

## Hook Events

PreToolUse, PostToolUse, PreCompact, PostCompact, Stop, Notification, SessionStart

## User Request

$ARGUMENTS"#,
        allowed_tools: Some(&["Read", "Write", "Edit", "Bash"]),
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // claude-api
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "claude-api",
        description: "Build apps with the Claude API or Anthropic SDK.",
        aliases: &["api", "anthropic-sdk"],
        when_to_use: Some("When the user wants to use the Claude API, Anthropic SDK, or build Claude-powered apps."),
        argument_hint: Some("[what to build]"),
        prompt_template: r#"# Build a Claude API Integration

## User Request

$ARGUMENTS

## Default Models

- Most capable: claude-opus-4-6
- Balanced:     claude-sonnet-4-6
- Fast:         claude-haiku-4-5-20251001

## SDK Quickstart

**Python**
```python
pip install anthropic
import anthropic
client = anthropic.Anthropic()
```

**TypeScript / Node**
```typescript
npm install @anthropic-ai/sdk
import Anthropic from '@anthropic-ai/sdk';
const client = new Anthropic();
```

## Key API Features

- Streaming (`stream_message`)
- Tool use / function calling
- Extended thinking
- Prompt caching
- Vision (image input)
- Files API
- Batch processing

Use async/await patterns. Follow SDK best practices."#,
        allowed_tools: Some(&["Read", "Grep", "Glob", "WebFetch"]),
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // loop
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "loop",
        description: "Run a prompt or slash command on a recurring interval.",
        aliases: &[],
        when_to_use: Some("When the user wants to run something repeatedly on a schedule."),
        argument_hint: Some("[interval] <command>"),
        prompt_template: r#"# /loop — schedule a recurring prompt

Parse the input below into `[interval] <prompt…>` and schedule it with CronCreate.

## Parsing (in priority order)

1. **Leading token**: if the first token matches `^\d+[smhd]$` (e.g. `5m`, `2h`), that
   is the interval; the rest is the prompt.
2. **Trailing "every" clause**: if the input ends with `every <N><unit>` extract that
   as the interval and strip it from the prompt.
3. **Default**: interval is `10m` and the entire input is the prompt.

If the resulting prompt is empty, show usage `/loop [interval] <prompt>` and stop.

## Interval → Cron

| Pattern | Cron | Notes |
|---------|------|-------|
| `Nm` (N ≤ 59) | `*/N * * * *` | every N minutes |
| `Nh` (N ≤ 23) | `0 */N * * *` | every N hours |
| `Nd` | `0 0 */N * *` | every N days at midnight |
| `Ns` | round up to nearest minute | cron min granularity is 1 min |

## Action

1. Call CronCreate with the parsed cron expression and prompt.
2. Confirm what was scheduled, including the cron expression and human-readable cadence.
3. **Immediately execute the parsed prompt now** — don't wait for the first cron fire.

## Input

$ARGUMENTS"#,
        allowed_tools: Some(&["CronCreate", "CronList"]),
        user_invocable: true,
    },

    // -----------------------------------------------------------------------
    // ultracode
    // -----------------------------------------------------------------------
    BundledSkill {
        name: "ultracode",
        description: "Run a disciplined ultracode workflow for serious coding work: classify the task, pick a mode (Direct / Workflow / Delegated), and when useful delegate bounded sidecar packets across claurst's native primitives (Agent subagents, TeamCreate swarms, TaskCreate background tasks), then integrate in the parent and verify.",
        aliases: &["ultra code"],
        when_to_use: Some("When the user invokes ultracode / ultra code, or wants a planned, multi-agent, delegated workflow with integration and an independent verification pass for a non-trivial coding task."),
        argument_hint: Some("[task to run in ultracode mode]"),
        prompt_template: r#"# Ultracode

You are operating in **ultracode** mode: a disciplined, supervised workflow for
serious coding work. Plan, split, delegate when it genuinely helps, integrate in
the parent session, and verify. Use the smallest workflow that can prove the
result -- do not manufacture ceremony for small tasks.

## Contract

- Ultracode is a claurst operating procedure, not a separate runtime. System,
  developer, and user rules always win over this text.
- Do NOT commit, push, publish, deploy, or delete anything unless the user
  explicitly asks. Ask one clear approval question before any destructive or
  irreversible action (mass rename/delete, force-push, migrations, production
  data, credentials/secrets/billing, broad codemods, or large agent fan-out).
  If approval is missing, continue only with safe read-only or draft work.
- Never use delegation to avoid understanding the integration path. The parent
  session owns integration, verification, and the final answer.

## 1. Classify the task

Before acting, state briefly:

- **type**: research | code change | bug fix | migration | audit | docs | design | QA | release
- **risk**: low | medium | high
- **blast radius**: single file | module | repo-wide | external system
- **verification**: none | command | tests | build | manual checklist
- **delegation**: useful | not useful (do bounded, independent packets exist?)

Then pick ONE mode.

## 2. Pick a mode

### Direct
Small, clear, tightly-coupled tasks with no useful independent packets (one file,
one command, a small function, a typo). Just do it, then run the narrowest useful
check. No artifacts.

### Workflow
Multi-phase or risky work that benefits from separated packets, but delegation is
not useful or not available. Keep a concrete plan (goal, success criteria,
constraints, risk, packets, verification), execute packets as isolated passes in
this session, integrate, then verify. Write scratch notes under
`.workflow/ultracode/<slug>/` only when they reduce risk.

### Delegated (default for non-trivial ultracode work)
When the work has bounded, non-overlapping, independent packets and delegation is
allowed, use claurst's native delegation primitives. Keep the blocking critical
path in the parent; delegate only useful sidecar work.

## 3. Delegated mode -- claurst native primitives

- **`Agent`** -- spawn a subagent for one bounded packet (read-only exploration,
  test writing, triage, or a disjoint write scope). Use `isolation: "worktree"`
  for write-heavy packets that must not collide, and `run_in_background: true` to
  fan several out at once, then integrate their final messages in the parent.
- **`TeamCreate`** (+ `TeamDelete`) -- stand up a named swarm/coordinator when
  several agents should work a shared task in parallel with restricted tool lists
  and aggregated output. Good for parallel audits or multi-track implementation.
- **`TaskCreate`** (+ `TaskGet` / `TaskUpdate` / `TaskList` / `TaskStop`) --
  track and run background work items; poll or stop them from the parent.

Rules:

- Plan first, then split into bounded, non-overlapping packets before spawning.
- Default to **2-4** subagents for useful independent work; do not exceed **~5**
  total without explicit user approval. Run at most one broad implementation wave
  and one review/verification wave unless the user approves more.
- Prefer delegation for read-heavy exploration, test writing, triage, and
  summarization. Use write-capable agents only when file ownership is disjoint.
- Tell every write-capable agent: "You are not alone in the codebase. Do not
  revert edits made by others; adapt to nearby changes." Give each a concrete
  objective, explicit ownership, and an expected-output shape.
- Only wait on an agent when its result blocks the next parent step.
- If native agents are unavailable or not useful, fall back to Workflow mode and
  say so briefly with the concrete reason.

## 4. Integrate (parent-owned)

Read each result, check claimed edits against the actual files, resolve
disagreements with source/tests/docs, and reject outputs that lack evidence.
Never paste raw agent logs as the final answer.

## 5. Verify (scaled by risk)

- **low**: inspect the diff + a targeted test.
- **medium**: targeted tests + typecheck/lint + affected build.
- **high**: full tests (if practical) + build + smoke + an independent review pass.

Mark each check pass | fail | skipped (with reason). Report skipped checks
honestly. Finish with a short summary: outcome, key files changed, verification
run, and remaining risk.

## Composing with /goal (multi-turn)

Ultracode is compatible with claurst's continuation/goal mode: a large task can
span multiple turns. For a long, autonomous objective, combine ultracode with
`/goal <objective>` -- the goal loop keeps working across turns while ultracode
governs *how* each turn plans, delegates, integrates, and verifies. Ultracode
mode itself is scoped to the current turn; re-invoke it (or run it under a goal)
for sustained multi-turn work.

## Your task

$ARGUMENTS"#,
        allowed_tools: None,
        user_invocable: true,
    },
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Find a bundled skill by name or alias (case-insensitive).
pub fn find_bundled_skill(name: &str) -> Option<&'static BundledSkill> {
    let lower = name.to_lowercase();
    BUNDLED_SKILLS.iter().find(|s| {
        s.name == lower || s.aliases.iter().any(|a| *a == lower)
    })
}

/// Return `(name, description)` pairs for all user-invocable bundled skills.
pub fn user_invocable_skills() -> Vec<(&'static str, &'static str)> {
    BUNDLED_SKILLS
        .iter()
        .filter(|s| s.user_invocable)
        .map(|s| (s.name, s.description))
        .collect()
}

/// Expand a skill's prompt template, substituting `$ARGUMENTS` and
/// `$ARGUMENTS_SUFFIX`.
///
/// - `$ARGUMENTS`        → replaced by `args` verbatim (or `""` when empty)
/// - `$ARGUMENTS_SUFFIX` → replaced by `": <args>"` when non-empty, else `""`
pub fn expand_prompt(skill: &BundledSkill, args: &str) -> String {
    let suffix = if args.is_empty() {
        String::new()
    } else {
        format!(": {}", args)
    };

    skill
        .prompt_template
        .replace("$ARGUMENTS_SUFFIX", &suffix)
        .replace("$ARGUMENTS", args)
}

// ---------------------------------------------------------------------------
// Ultracode keyword detection + system-prompt addendum
// ---------------------------------------------------------------------------

/// Keywords that activate ultracode mode, longest-first so a spaced match wins.
pub const ULTRACODE_KEYWORDS: &[&str] = &["ultra code", "ultracode"];

/// Find every whole-word-ish, case-insensitive occurrence of an ultracode
/// keyword in `text`, returned as non-overlapping `(start, end)` byte ranges
/// into `text`.
///
/// The keywords are ASCII, so an ASCII-lowercased copy preserves byte length
/// and offsets exactly; every match therefore maps back onto `text` at valid
/// char boundaries. "Whole-word-ish" means the byte immediately before/after a
/// match must not be ASCII alphanumeric (so `ultracoder` does not match).
pub fn ultracode_match_ranges(text: &str) -> Vec<(usize, usize)> {
    let hay = text.as_bytes().to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    'scan: while i < hay.len() {
        for kw in ULTRACODE_KEYWORDS {
            let k = kw.as_bytes();
            if i + k.len() <= hay.len() && &hay[i..i + k.len()] == k {
                let end = i + k.len();
                let left_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
                let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
                if left_ok && right_ok {
                    ranges.push((i, end));
                    i = end;
                    continue 'scan;
                }
            }
        }
        i += 1;
    }
    ranges
}

/// Whether `text` contains an ultracode keyword (whole-word-ish, case-insensitive).
pub fn text_triggers_ultracode(text: &str) -> bool {
    !ultracode_match_ranges(text).is_empty()
}

/// Build the per-turn system-prompt addendum for ultracode mode.
///
/// This is the *single source of truth* for ultracode's operating procedure:
/// it expands the bundled `ultracode` skill's `prompt_template` (the same text
/// the `/ultracode` skill runs) and wraps it with a short activation framing.
/// Returns `None` only if the skill is somehow missing from `BUNDLED_SKILLS`.
pub fn ultracode_system_prompt_addendum() -> Option<String> {
    let skill = find_bundled_skill("ultracode")?;
    let body = expand_prompt(
        skill,
        "(the task is the user's latest message in this conversation)",
    );
    Some(format!(
        "\n## Ultracode Mode (activated by keyword)\n\
         The user's message invoked **ultracode**. Operate in ultracode mode for \
         this turn using the procedure below (the same procedure as the bundled \
         `ultracode` skill).\n\n{}\n",
        body
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_skills_have_non_empty_names() {
        for s in BUNDLED_SKILLS {
            assert!(!s.name.is_empty(), "skill has empty name");
        }
    }

    #[test]
    fn all_skills_have_non_empty_descriptions() {
        for s in BUNDLED_SKILLS {
            assert!(
                !s.description.is_empty(),
                "skill '{}' has empty description",
                s.name
            );
        }
    }

    #[test]
    fn all_skills_have_non_empty_prompt_templates() {
        for s in BUNDLED_SKILLS {
            assert!(
                !s.prompt_template.is_empty(),
                "skill '{}' has empty prompt_template",
                s.name
            );
        }
    }

    #[test]
    fn skill_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in BUNDLED_SKILLS {
            assert!(
                seen.insert(s.name),
                "duplicate skill name: {}",
                s.name
            );
        }
    }

    #[test]
    fn find_by_primary_name() {
        let skill = find_bundled_skill("simplify");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "simplify");
    }

    #[test]
    fn find_by_alias() {
        let skill = find_bundled_skill("mem");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "remember");
    }

    #[test]
    fn find_case_insensitive() {
        assert!(find_bundled_skill("SIMPLIFY").is_some());
        assert!(find_bundled_skill("Debug").is_some());
    }

    #[test]
    fn find_missing_returns_none() {
        assert!(find_bundled_skill("nonexistent-skill-xyz").is_none());
    }

    #[test]
    fn expand_prompt_substitutes_arguments() {
        let skill = find_bundled_skill("debug").unwrap();
        let expanded = expand_prompt(skill, "NullPointerException in Foo.java");
        assert!(expanded.contains("NullPointerException in Foo.java"));
        assert!(!expanded.contains("$ARGUMENTS"));
    }

    #[test]
    fn expand_prompt_empty_args_no_residual_placeholder() {
        let skill = find_bundled_skill("simplify").unwrap();
        let expanded = expand_prompt(skill, "");
        assert!(!expanded.contains("$ARGUMENTS"));
        assert!(!expanded.contains("$ARGUMENTS_SUFFIX"));
    }

    #[test]
    fn expand_prompt_suffix_non_empty() {
        let skill = find_bundled_skill("stuck").unwrap();
        let expanded = expand_prompt(skill, "trying to run tests");
        // Should contain ": trying to run tests" from $ARGUMENTS_SUFFIX
        assert!(expanded.contains(": trying to run tests"));
    }

    #[test]
    fn expand_prompt_suffix_empty() {
        let skill = find_bundled_skill("stuck").unwrap();
        let expanded = expand_prompt(skill, "");
        // $ARGUMENTS_SUFFIX should expand to "" so "stuck" is not followed by ": "
        assert!(!expanded.contains("stuck: "));
        assert!(!expanded.contains("$ARGUMENTS_SUFFIX"));
    }

    #[test]
    fn user_invocable_skills_non_empty() {
        let skills = user_invocable_skills();
        assert!(!skills.is_empty());
    }

    #[test]
    fn user_invocable_skills_all_marked_true() {
        for (name, _) in user_invocable_skills() {
            let skill = find_bundled_skill(name).unwrap();
            assert!(
                skill.user_invocable,
                "skill '{}' returned by user_invocable_skills() but user_invocable=false",
                name
            );
        }
    }

    // ---- ultracode -------------------------------------------------------

    #[test]
    fn ultracode_skill_present_and_user_invocable() {
        let skill = find_bundled_skill("ultracode").expect("ultracode skill missing");
        assert_eq!(skill.name, "ultracode");
        assert!(skill.user_invocable);
        assert!(skill.allowed_tools.is_none(), "ultracode should expose the full toolset");
    }

    #[test]
    fn ultracode_resolvable_by_alias_and_case() {
        assert_eq!(find_bundled_skill("ultra code").unwrap().name, "ultracode");
        assert_eq!(find_bundled_skill("UltraCode").unwrap().name, "ultracode");
        assert_eq!(find_bundled_skill("ULTRA CODE").unwrap().name, "ultracode");
    }

    #[test]
    fn ultracode_template_names_native_primitives() {
        let skill = find_bundled_skill("ultracode").unwrap();
        for needle in ["Agent", "TeamCreate", "TaskCreate", "/goal", "Delegated"] {
            assert!(
                skill.prompt_template.contains(needle),
                "ultracode template missing `{needle}`"
            );
        }
    }

    #[test]
    fn ultracode_match_ranges_finds_keyword() {
        let text = "please ultracode this";
        let r = ultracode_match_ranges(text);
        assert_eq!(r.len(), 1);
        let (s, e) = r[0];
        assert_eq!(&text[s..e], "ultracode");
    }

    #[test]
    fn ultracode_match_ranges_case_insensitive_and_spaced() {
        assert_eq!(ultracode_match_ranges("UltraCode now").len(), 1);
        let text = "do ULTRA CODE it";
        let r = ultracode_match_ranges(text);
        assert_eq!(r.len(), 1);
        let (s, e) = r[0];
        // Offsets map onto the original bytes (ASCII), case aside.
        assert_eq!(&text[s..e].to_ascii_lowercase(), "ultra code");
    }

    #[test]
    fn ultracode_match_ranges_respects_word_boundaries() {
        assert!(ultracode_match_ranges("ultracoder").is_empty());
        assert!(ultracode_match_ranges("supraultracode1").is_empty());
        assert!(text_triggers_ultracode("(ultracode)"));
        assert!(text_triggers_ultracode("ultracode."));
        assert!(!text_triggers_ultracode("this is a normal prompt"));
    }

    #[test]
    fn ultracode_addendum_reuses_skill_body() {
        let add = ultracode_system_prompt_addendum().expect("addendum present");
        assert!(add.contains("Ultracode Mode"));
        assert!(add.contains("## Your task"));
        // Sourced from the skill template, not a duplicate literal.
        assert!(add.contains("Agent"));
        assert!(add.contains("TeamCreate"));
        assert!(!add.contains("$ARGUMENTS"), "template placeholder must be expanded");
    }
}
