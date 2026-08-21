// ExitPlanMode tool: leave planning mode and return to normal execution.

use crate::{
    PermissionLevel, PlanApprovalEvent, PlanChoice, PlanDecision, Tool, ToolContext, ToolResult,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

pub struct ExitPlanModeTool;

#[derive(Debug, Deserialize)]
struct ExitPlanModeInput {
    #[serde(default)]
    summary: Option<String>,
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        mikmik_core::constants::TOOL_NAME_EXIT_PLAN_MODE
    }

    fn description(&self) -> &str {
        "Exit plan mode and return to normal execution mode where all tools \
         are available. Optionally provide a summary of the plan."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Summary of the plan you developed"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: ExitPlanModeInput =
            serde_json::from_value(input).unwrap_or(ExitPlanModeInput { summary: None });

        debug!(summary = ?params.summary, "Exiting plan mode");

        // Without a dialog to ask through, leaving plan mode is the model's own
        // decision, exactly as it was before approval existed. This is the
        // headless and non-interactive path, where blocking on an answer nobody
        // can give would hang the run.
        let Some(tx) = ctx
            .plan_approval_tx
            .as_ref()
            .filter(|_| !ctx.non_interactive)
        else {
            let msg = match &params.summary {
                Some(summary) => format!("Exited plan mode. Plan summary: {summary}"),
                None => "Exited plan mode. All tools are now available.".to_string(),
            };
            return ToolResult::success(msg).with_metadata(json!({
                "type": "exit_plan_mode",
                "summary": params.summary,
            }));
        };

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<PlanDecision>();
        let plan = params
            .summary
            .clone()
            .unwrap_or_else(|| "The model did not write out a plan.".to_string());
        if tx
            .send(PlanApprovalEvent {
                plan: plan.clone(),
                reply_tx,
            })
            .is_err()
        {
            return ToolResult::error("Plan approval channel closed".to_string());
        }

        let Ok(decision) = reply_rx.await else {
            return ToolResult::error(
                "Plan approval channel closed before a decision was made".to_string(),
            );
        };

        // Each answer says what the session is now allowed to do, because the
        // model has no other way to learn that the permission mode moved.
        let mut msg = match decision.choice {
            PlanChoice::AutoAcceptEdits => {
                "The user approved the plan and turned on auto-accept for edits. \
                 Start implementing it."
            }
            PlanChoice::ManualApproval => {
                "The user approved the plan and will approve each edit. \
                 Start implementing it."
            }
            PlanChoice::KeepPlanning => {
                "The user did not approve the plan. Stay in plan mode and revise it."
            }
        }
        .to_string();
        if let Some(note) = decision
            .note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            msg.push_str(&format!("\nThe user added: {note}"));
        }

        ToolResult::success(msg).with_metadata(json!({
            "type": "exit_plan_mode",
            "summary": params.summary,
            "approved": decision.choice != PlanChoice::KeepPlanning,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::allow_all_context;

    /// Nothing to approve through, so the tool must not wait for an answer.
    #[tokio::test]
    async fn without_a_dialog_the_tool_returns_at_once() {
        let ctx = allow_all_context(std::env::temp_dir());
        assert!(ctx.plan_approval_tx.is_none());

        let result = ExitPlanModeTool
            .execute(json!({ "summary": "do the thing" }), &ctx)
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("do the thing"));
    }

    /// A wired-up dialog blocks the tool until the user answers, and the answer
    /// reaches the model.
    #[tokio::test]
    async fn a_decision_is_reported_back_with_its_note() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PlanApprovalEvent>();
        let mut ctx = allow_all_context(std::env::temp_dir());
        ctx.non_interactive = false;
        ctx.plan_approval_tx = Some(tx);

        let answerer = tokio::spawn(async move {
            let event = rx.recv().await?;
            let plan = event.plan.clone();
            let _ = event.reply_tx.send(PlanDecision {
                choice: PlanChoice::KeepPlanning,
                note: Some("  the migration step is missing  ".to_string()),
            });
            Some(plan)
        });

        let result = ExitPlanModeTool
            .execute(json!({ "summary": "step one" }), &ctx)
            .await;

        assert_eq!(answerer.await.ok().flatten().as_deref(), Some("step one"));
        assert!(result.content.contains("did not approve"));
        assert!(result.content.contains("the migration step is missing"));
        assert_eq!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("approved"))
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn each_choice_maps_to_one_permission_mode() {
        use mikmik_core::config::PermissionMode;

        assert_eq!(
            PlanChoice::AutoAcceptEdits.permission_mode(),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            PlanChoice::ManualApproval.permission_mode(),
            Some(PermissionMode::Default)
        );
        // Refusing a plan leaves the session in plan mode.
        assert_eq!(PlanChoice::KeepPlanning.permission_mode(), None);
    }
}
