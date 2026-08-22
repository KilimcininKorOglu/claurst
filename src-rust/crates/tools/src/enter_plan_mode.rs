// EnterPlanMode tool: switch the session into planning (read-only) mode.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

pub struct EnterPlanModeTool;

#[derive(Debug, Deserialize)]
struct EnterPlanModeInput {
    #[serde(default)]
    reason: Option<String>,
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        mikmik_core::constants::TOOL_NAME_ENTER_PLAN_MODE
    }

    fn description(&self) -> &str {
        "Enter plan mode before starting significant work. In plan mode you can \
         read, search and think, but you cannot write files or run commands. \
         Call it when the change touches the architecture, the data model or a \
         public interface, when it spans more than two or three files, when the \
         task is a new feature, a migration or a refactor, when a bug's cause is \
         not confirmed yet, or when the request allows more than one reasonable \
         reading. Do not call it for a typo, a one-line fix, a question about \
         the code, or work the user already specified step by step. While \
         planning, read the code rather than guessing, and use AskUserQuestion \
         for anything the request leaves open. Call ExitPlanMode when the plan \
         is ready, to submit it for approval."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why you want to enter plan mode"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: EnterPlanModeInput =
            serde_json::from_value(input).unwrap_or(EnterPlanModeInput { reason: None });

        debug!(reason = ?params.reason, "Entering plan mode");

        // The tool used to report success and change nothing: the result went
        // to the model and its metadata reached no reader, so the session
        // stayed in whatever mode it was in while the model believed it was
        // planning. The switch travels a channel now, the way ExitPlanMode's
        // decision does.
        let Some(tx) = ctx.plan_mode_tx.as_ref() else {
            return ToolResult::error(
                "Plan mode is not available in this session, so nothing changed. \
                 You still have every tool you had; do not act as though writes \
                 and commands are blocked."
                    .to_string(),
            );
        };

        if tx
            .send(crate::EnterPlanModeEvent {
                reason: params.reason.clone(),
            })
            .is_err()
        {
            return ToolResult::error(
                "The session is no longer listening, so plan mode was not entered.".to_string(),
            );
        }

        let msg = if let Some(reason) = &params.reason {
            format!("Entered plan mode: {}", reason)
        } else {
            "Entered plan mode. Only read-only operations are allowed.".to_string()
        };

        ToolResult::success(msg).with_metadata(json!({
            "type": "enter_plan_mode",
            "reason": params.reason,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::allow_all_context;

    fn context() -> ToolContext {
        allow_all_context(std::env::temp_dir())
    }

    #[tokio::test]
    async fn the_request_reaches_the_session_with_its_reason() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = context();
        ctx.plan_mode_tx = Some(tx);

        let result = EnterPlanModeTool
            .execute(json!({ "reason": "the migration needs a plan" }), &ctx)
            .await;

        assert!(!result.is_error, "the tool reported a failure");
        let event = rx.try_recv().expect("no request reached the session");
        assert_eq!(event.reason.as_deref(), Some("the migration needs a plan"));
    }

    #[tokio::test]
    async fn without_a_channel_the_tool_says_the_mode_did_not_change() {
        let result = EnterPlanModeTool.execute(json!({}), &context()).await;

        assert!(
            result.is_error,
            "a session that cannot switch modes was reported as switched"
        );
        assert!(
            result.content.contains("nothing changed"),
            "the model is not told the mode stayed put: {}",
            result.content
        );
    }
}
