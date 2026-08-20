// Goal command: durable long-running autonomous goals (`/goal`).
//
// Extracted from lib.rs (issue #232). Behavior-preserving move.

use super::{CommandContext, CommandResult, SlashCommand};
use async_trait::async_trait;

pub struct GoalCommand;

// ---- /goal ---------------------------------------------------------------

/// Parse a soft token budget from strings like "250K", "1M", "500000".
fn parse_token_budget(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('K').or_else(|| s.strip_suffix('k'))
    {
        (n, 1_000u64)
    } else if let Some(n) = s.strip_suffix('M').or_else(|| s.strip_suffix('m')) {
        (n, 1_000_000u64)
    } else {
        (s, 1u64)
    };
    num_str.trim().parse::<u64>().ok().map(|n| n * multiplier)
}

#[async_trait]
impl SlashCommand for GoalCommand {
    fn name(&self) -> &str {
        "goal"
    }
    fn description(&self) -> &str {
        "Set or manage a durable long-running goal for autonomous work"
    }
    fn help(&self) -> &str {
        "Usage:\n\
         /goal <objective>              — set a new goal and begin working autonomously\n\
         /goal set <objective>          — the same, spelled out\n\
         /goal --tokens 250K <text>     — set a goal with a soft token budget\n\
         /goal                          — show current goal status\n\
         /goal status                   — show current goal status\n\
         /goal pause                    — pause the active goal\n\
         /goal resume                   — resume a paused goal\n\
         /goal clear                    — delete the current goal\n\
         /goal complete                 — request a completion audit\n\n\
         Goals let MikMik work autonomously across turns toward a single\n\
         verifiable objective. MikMik will keep iterating until the goal is\n\
         complete, you pause it, or the 200-turn runaway guard fires.\n\n\
         Examples:\n\
         /goal Migrate the project from Express to Fastify, keeping all routes passing\n\
         /goal --tokens 500K Fix all TypeScript errors in src/ without breaking tests"
    }

    async fn execute(&self, args: &str, ctx: &mut CommandContext) -> CommandResult {
        if !mikmik_core::goals_enabled() {
            return CommandResult::Message(
                "Goals are disabled. Unset MIKMIK_GOALS=0 (or remove it) to re-enable.".to_string(),
            );
        }

        let args = args.trim();
        let session_id = &ctx.session_id;

        // Parse subcommands with no objective
        match args {
            "" | "status" => return goal_status(session_id),
            // `set` names the objective that follows it, so a bare `set` is a
            // usage error rather than an objective literally called "set".
            "set" => return goal_usage(),
            "pause" => {
                let store = match open_goal_store() {
                    Some(s) => s,
                    None => return CommandResult::Error("Could not open goal store.".to_string()),
                };
                match store.get_goal(session_id) {
                    None => return CommandResult::Message("No active goal.".to_string()),
                    Some(g) if g.status == mikmik_core::GoalStatus::Complete => {
                        return CommandResult::Message("Goal is already complete.".to_string());
                    }
                    Some(g) if g.status == mikmik_core::GoalStatus::Paused => {
                        return CommandResult::Message(
                            "Goal is already paused. Use /goal resume to continue.".to_string(),
                        );
                    }
                    _ => {}
                }
                if let Err(e) = store.set_status(session_id, mikmik_core::GoalStatus::Paused) {
                    return CommandResult::Error(format!("Failed to pause goal: {}", e));
                }
                return CommandResult::Message(
                    "Goal paused. Use /goal resume to continue.".to_string(),
                );
            }
            "resume" => {
                let store = match open_goal_store() {
                    Some(s) => s,
                    None => return CommandResult::Error("Could not open goal store.".to_string()),
                };
                match store.get_goal(session_id) {
                    None => return CommandResult::Message("No goal to resume.".to_string()),
                    Some(g) if g.status == mikmik_core::GoalStatus::Active => {
                        return CommandResult::Message("Goal is already active.".to_string());
                    }
                    Some(g) if g.status == mikmik_core::GoalStatus::Complete => {
                        return CommandResult::Message(
                            "Goal is complete. Use /goal <objective> to set a new one.".to_string(),
                        );
                    }
                    _ => {}
                }
                if let Err(e) = store.set_status(session_id, mikmik_core::GoalStatus::Active) {
                    return CommandResult::Error(format!("Failed to resume goal: {}", e));
                }
                return CommandResult::Message(
                    "Goal resumed. MikMik will continue on the next message.".to_string(),
                );
            }
            "clear" => {
                let store = match open_goal_store() {
                    Some(s) => s,
                    None => return CommandResult::Error("Could not open goal store.".to_string()),
                };
                store.clear_goal(session_id).unwrap_or_default();
                return CommandResult::Message("Goal cleared.".to_string());
            }
            "complete" => {
                // Inject a completion-audit user message.
                let store = match open_goal_store() {
                    Some(s) => s,
                    None => return CommandResult::Error("Could not open goal store.".to_string()),
                };
                match store.get_active_goal(session_id) {
                    None => {
                        return CommandResult::Message(
                            "No active goal. Set one with /goal <objective>.".to_string(),
                        );
                    }
                    Some(goal) => {
                        let audit_msg = format!(
                            "[User requested goal completion audit]\n\
                             Please review your active goal:\n\
                             <objective>\n{}\n</objective>\n\n\
                             Run through the completion audit:\n\
                             1. Restate the objective as concrete deliverables.\n\
                             2. Check that all deliverables have been achieved.\n\
                             3. Run any tests or validation commands.\n\
                             4. If fully complete, call GoalComplete with audit_summary and evidence.\n\
                             5. If not complete, describe what remains.",
                            goal.objective
                        );
                        return CommandResult::UserMessage(audit_msg);
                    }
                }
            }
            _ => {} // fall through to parse as objective (possibly with --tokens)
        }

        let (token_budget, objective) = parse_objective_args(args);

        if objective.is_empty() {
            return goal_usage();
        }

        let store = match open_goal_store() {
            Some(s) => s,
            None => return CommandResult::Error("Could not open goal store.".to_string()),
        };

        match store.set_goal(session_id, objective, token_budget) {
            Err(mikmik_core::GoalError::ObjectiveTooLong { len, max }) => CommandResult::Error(
                format!("Objective too long ({} chars). Max {} chars.", len, max),
            ),
            Err(e) => CommandResult::Error(format!("Failed to set goal: {}", e)),
            Ok(goal) => {
                // Return UserMessage so the query loop fires immediately and the
                // model begins working toward the goal without user needing to
                // send another message.
                CommandResult::UserMessage(mikmik_core::goal_kickoff_message(&goal))
            }
        }
    }
}

/// Split the objective-carrying form of `/goal` into its optional token budget
/// and the objective text.
///
/// `/goal set <objective>` is the explicit spelling of `/goal <objective>`, so
/// the verb is stripped before `--tokens` is read. Both orders of the two
/// prefixes therefore reduce to the same objective, and the transcript
/// renderer's `extract_goal_objective_from_args` strips them the same way. If
/// the two ever disagree the command stores one objective while the transcript
/// draws another.
fn parse_objective_args(args: &str) -> (Option<u64>, &str) {
    let args = match args.split_once(char::is_whitespace) {
        Some((verb, rest)) if verb.eq_ignore_ascii_case("set") => rest.trim_start(),
        _ => args,
    };
    let Some(rest) = args.strip_prefix("--tokens") else {
        return (None, args);
    };
    // Expected: --tokens <budget> <objective>
    let rest = rest.trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let budget_str = parts.next().unwrap_or("");
    let objective = parts.next().unwrap_or("").trim();
    (parse_token_budget(budget_str), objective)
}

/// The usage text shown for `/goal` forms that carry no objective.
fn goal_usage() -> CommandResult {
    CommandResult::Message(
        "Usage: /goal [set] <objective> [--tokens 250K]\n\
         Or: /goal status|pause|resume|clear|complete"
            .to_string(),
    )
}

fn open_goal_store() -> Option<mikmik_core::GoalStore> {
    mikmik_core::GoalStore::open_default()
}

fn goal_status(session_id: &str) -> CommandResult {
    let store = match open_goal_store() {
        Some(s) => s,
        None => return CommandResult::Error("Could not open goal store.".to_string()),
    };
    match store.get_goal(session_id) {
        None => {
            CommandResult::Message("No active goal. Set one with:\n  /goal <objective>".to_string())
        }
        Some(g) => {
            let budget_line = g
                .budget_display()
                .map(|b| format!("\nBudget:  {}", b))
                .unwrap_or_default();
            CommandResult::Message(format!(
                "Goal status\n\
                 ───────────\n\
                 Status:  {}\n\
                 Turns:   {}\n\
                 Elapsed: {}{}\n\
                 Objective:\n  {}",
                g.status.as_str(),
                g.turns_used,
                g.elapsed_display(),
                budget_line,
                g.objective,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_objective_carries_no_budget() {
        assert_eq!(
            parse_objective_args("Migrate to React"),
            (None, "Migrate to React")
        );
    }

    #[test]
    fn set_verb_is_not_part_of_the_objective() {
        assert_eq!(
            parse_objective_args("set Migrate to React"),
            (None, "Migrate to React")
        );
        assert_eq!(
            parse_objective_args("SET Migrate to React"),
            (None, "Migrate to React")
        );
    }

    #[test]
    fn a_word_merely_starting_with_set_stays_in_the_objective() {
        assert_eq!(
            parse_objective_args("settle the migration"),
            (None, "settle the migration")
        );
    }

    #[test]
    fn set_and_tokens_compose_in_that_order() {
        assert_eq!(
            parse_objective_args("set --tokens 250K Migrate to React"),
            (Some(250_000), "Migrate to React")
        );
        assert_eq!(
            parse_objective_args("--tokens 250K Migrate to React"),
            (Some(250_000), "Migrate to React")
        );
    }

    #[test]
    fn bare_set_leaves_no_objective_to_store() {
        // `execute` short-circuits a bare `set` before it reaches here, but an
        // objective made only of the verb must not survive this parse either.
        assert_eq!(parse_objective_args("set").1, "set");
        assert_eq!(
            parse_objective_args("set --tokens 250K"),
            (Some(250_000), "")
        );
    }

    #[test]
    fn usage_names_the_set_spelling() {
        let CommandResult::Message(text) = goal_usage() else {
            panic!("usage must be a plain message");
        };
        assert!(text.contains("/goal [set] <objective>"), "{text:?}");
    }
}
