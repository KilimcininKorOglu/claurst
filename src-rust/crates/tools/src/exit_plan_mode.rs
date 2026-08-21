// ExitPlanMode tool: leave planning mode and return to normal execution.

use crate::{
    PermissionLevel, PlanApprovalEvent, PlanChoice, PlanDecision, Tool, ToolContext, ToolResult,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

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

        let plan = params
            .summary
            .clone()
            .unwrap_or_else(|| "The model did not write out a plan.".to_string());
        // Written on every path, headless included, because the file is the
        // only lasting record of the plan: the tool input scrolls away with the
        // transcript.
        let plan_path = write_plan(&ctx.session_id, &plan);

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
        if tx
            .send(PlanApprovalEvent {
                plan: plan.clone(),
                plan_path: plan_path.clone(),
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

        let mut msg = message_for(decision.choice).to_string();
        if let Some(note) = decision
            .note
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            msg.push_str(&format!("\nThe user added: {note}"));
        }

        // The dialog can open the plan file in an editor while it waits, so the
        // plan the user approved may not be the plan the model wrote. Read it
        // back here rather than carrying the text through the dialog, so the
        // file has one owner.
        let edited = plan_path
            .as_deref()
            .and_then(|path| read_edited_plan(path, &plan));
        if let Some(edited) = &edited {
            msg.push_str(&format!(
                "\nThe user edited the plan. The plan is now:\n\n{edited}"
            ));
        }

        ToolResult::success(msg).with_metadata(json!({
            "type": "exit_plan_mode",
            "summary": edited.or(params.summary),
            "approved": decision.choice.is_approval(),
        }))
    }
}

/// What the model is told about the answer.
///
/// Each answer says what the session is now allowed to do, because the tool
/// result is all the model learns: it cannot see that the permission mode
/// moved, and it cannot see that the conversation is about to be summarised.
fn message_for(choice: PlanChoice) -> &'static str {
    match choice {
        PlanChoice::ApproveAndClearContext => {
            "The user approved the plan and asked for the conversation to be \
             cleared first. Stop here without starting the work: the \
             conversation is about to be summarised and the plan will be sent \
             to you again."
        }
        PlanChoice::Approve => {
            "The user approved the plan and the session is back in the \
             permission mode it was in before planning. Start implementing it."
        }
        PlanChoice::ApproveWithManualEdits => {
            "The user approved the plan and will approve each edit. \
             Start implementing it."
        }
        PlanChoice::KeepPlanning => {
            "The user did not approve the plan. Stay in plan mode and revise it."
        }
    }
}

/// Write the plan where the user can open it, returning where it landed.
///
/// Best-effort: a plan that cannot be written still reaches the dialog, which
/// then offers no way to edit it. The failure is logged rather than returned,
/// because it changes nothing about what the model should do next.
fn write_plan(session_id: &str, plan: &str) -> Option<PathBuf> {
    let path = match mikmik_core::session_storage::plan_path(session_id) {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, session_id, "the session id cannot name a plan file");
            return None;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            warn!(%error, dir = %parent.display(), "could not create the plans directory");
            return None;
        }
    }
    // Ends with a newline: this file is opened in an editor, and a plan that
    // does not end its last line makes anything appended to it join that line.
    let plan = if plan.ends_with('\n') {
        plan.to_string()
    } else {
        format!("{plan}\n")
    };
    if let Err(error) = std::fs::write(&path, plan) {
        warn!(%error, path = %path.display(), "could not write the plan file");
        return None;
    }
    Some(path)
}

/// The plan as it stands on disk, when that is not what was written there.
fn read_edited_plan(path: &Path, written: &str) -> Option<String> {
    let current = std::fs::read_to_string(path).ok()?;
    (current.trim() != written.trim()).then(|| current.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{allow_all_context, HomeGuard, HOME_LOCK};

    /// Nothing to approve through, so the tool must not wait for an answer.
    /// The plan is still written, because headless is where nobody is watching
    /// the transcript.
    #[tokio::test]
    async fn without_a_dialog_the_tool_returns_at_once() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("a temp home");
        let _home = HomeGuard::pointing_at(home.path());

        let ctx = allow_all_context(std::env::temp_dir());
        assert!(ctx.plan_approval_tx.is_none());

        let result = ExitPlanModeTool
            .execute(json!({ "summary": "do the thing" }), &ctx)
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("do the thing"));

        let written = mikmik_core::session_storage::plan_path(&ctx.session_id)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok());
        // Ends its last line, or an editor appending to it joins that line.
        assert_eq!(written.as_deref(), Some("do the thing\n"));
    }

    /// A wired-up dialog blocks the tool until the user answers, and the answer
    /// reaches the model.
    #[tokio::test]
    async fn a_decision_is_reported_back_with_its_note() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("a temp home");
        let _home = HomeGuard::pointing_at(home.path());

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

    /// The dialog can open the plan in an editor while it waits, so what the
    /// user approved may not be what the model wrote.
    #[tokio::test]
    async fn a_plan_edited_while_the_dialog_waited_reaches_the_model() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("a temp home");
        let _home = HomeGuard::pointing_at(home.path());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PlanApprovalEvent>();
        let mut ctx = allow_all_context(std::env::temp_dir());
        ctx.non_interactive = false;
        ctx.plan_approval_tx = Some(tx);

        let answerer = tokio::spawn(async move {
            let event = rx.recv().await?;
            // Stands in for the editor the user opened with ctrl+g.
            let path = event.plan_path.clone()?;
            std::fs::write(&path, "step one\nstep two, which the user added\n").ok()?;
            let _ = event.reply_tx.send(PlanDecision {
                choice: PlanChoice::ApproveWithManualEdits,
                note: None,
            });
            Some(path)
        });

        let result = ExitPlanModeTool
            .execute(json!({ "summary": "step one" }), &ctx)
            .await;

        assert!(
            answerer.await.ok().flatten().is_some(),
            "the dialog was given no plan file to edit"
        );
        assert!(result.content.contains("The user edited the plan"));
        assert!(result.content.contains("step two, which the user added"));
        // The metadata carries the plan that was approved, not the one that
        // was proposed, because that is the one being implemented.
        assert_eq!(
            result
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("summary"))
                .and_then(Value::as_str),
            Some("step one\nstep two, which the user added")
        );
    }

    /// An untouched plan file must not be reported as an edit.
    #[tokio::test]
    async fn an_untouched_plan_is_not_reported_as_edited() {
        let _lock = HOME_LOCK.lock().await;
        let home = tempfile::tempdir().expect("a temp home");
        let _home = HomeGuard::pointing_at(home.path());

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PlanApprovalEvent>();
        let mut ctx = allow_all_context(std::env::temp_dir());
        ctx.non_interactive = false;
        ctx.plan_approval_tx = Some(tx);

        tokio::spawn(async move {
            let event = rx.recv().await?;
            let _ = event.reply_tx.send(PlanDecision {
                choice: PlanChoice::ApproveWithManualEdits,
                note: None,
            });
            Some(())
        });

        let result = ExitPlanModeTool
            .execute(json!({ "summary": "step one" }), &ctx)
            .await;

        assert!(
            !result.content.contains("edited the plan"),
            "{}",
            result.content
        );
    }

    /// Each answer has to say something different to the model, because the
    /// tool result is all it learns about what the user chose.
    #[test]
    fn every_answer_tells_the_model_something_different() {
        let choices = [
            PlanChoice::ApproveAndClearContext,
            PlanChoice::Approve,
            PlanChoice::ApproveWithManualEdits,
            PlanChoice::KeepPlanning,
        ];
        assert!(choices[..3].iter().all(|choice| choice.is_approval()));
        assert!(!PlanChoice::KeepPlanning.is_approval());

        // The one that clears the context must tell the model to stop, or it
        // starts work that the summary is about to throw away.
        assert!(message_for(PlanChoice::ApproveAndClearContext).contains("Stop here"));
        assert!(message_for(PlanChoice::Approve).contains("Start implementing"));
        assert!(message_for(PlanChoice::ApproveWithManualEdits).contains("approve each edit"));
        assert!(message_for(PlanChoice::KeepPlanning).contains("did not approve"));
    }
}
