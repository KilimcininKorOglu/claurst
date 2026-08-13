// AdvisorTool: ask a second model for a review before committing to a decision.
//
// Claude Code runs its advisor server-side and surfaces it as a tool-use block.
// The wire format for that is not documented in `spec/`, so this is the
// client-side equivalent: claurst calls the configured advisor model itself and
// hands the answer back as an ordinary tool result.
//
// The tool is only registered when `advisorModel` is configured, so a session
// without an advisor never pays the schema cost or sees the guideline block.

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use claurst_api::ProviderResolveError;
use claurst_core::advisor_target::{parse_advisor_model, AdvisorTarget};
use claurst_core::message_utils::text_from_blocks;
use claurst_core::types::Message;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

/// Calls allowed per turn. A second opinion is worth one or two consultations;
/// beyond that the model is deferring rather than deciding, and each call costs
/// a full round trip against the advisor model.
const MAX_CALLS_PER_TURN: u32 = 2;

/// Advisor replies are meant to be short critiques, not essays.
const ADVISOR_MAX_TOKENS: u32 = 2048;

const ADVISOR_SYSTEM_PROMPT: &str = "You are a senior engineer giving a second opinion to another \
     engineer who is mid-task. Be a critic, not a cheerleader.\n\n\
     State the single most important problem first. Name concrete failure cases, \
     missed edge cases, and wrong assumptions. If a claim in the question is \
     unsupported, say so. If the approach is sound, say that plainly in one line \
     rather than inventing objections.\n\n\
     Be specific and brief. No preamble, no summary of what you were asked.";

pub struct AdvisorTool;

#[derive(Debug, Deserialize)]
struct AdvisorInput {
    /// The specific decision or claim the caller wants reviewed.
    question: String,
    /// Optional material to review: a diff, a plan, a snippet.
    #[serde(default)]
    context: Option<String>,
}

/// Per-turn call accounting, keyed by the turn index the tool last saw.
static CALLS_THIS_TURN: std::sync::Mutex<(usize, u32)> = std::sync::Mutex::new((usize::MAX, 0));

/// Record one call against the current turn and report whether it is allowed.
/// The counter resets whenever the query loop moves to a new turn.
fn claim_call_slot(turn: usize) -> bool {
    let mut state = match CALLS_THIS_TURN.lock() {
        Ok(state) => state,
        // A poisoned lock means a previous call panicked mid-update. The count
        // is only a rate limit, so recover rather than propagate.
        Err(poisoned) => poisoned.into_inner(),
    };
    if state.0 != turn {
        *state = (turn, 0);
    }
    if state.1 >= MAX_CALLS_PER_TURN {
        return false;
    }
    state.1 += 1;
    true
}

/// Turn a failed provider lookup into advice the model can act on.
///
/// The model cannot change settings, so each message says who has to fix what
/// rather than suggesting a retry.
fn describe_resolve_error(error: &ProviderResolveError) -> String {
    match error {
        ProviderResolveError::NotAvailable { provider_id } => format!(
            "Advisor provider '{provider_id}' is not available. \
             Tell the user to check its credentials or pick another model with `/advisor <model>`."
        ),
        ProviderResolveError::ProfilesUnsupported { provider_id } => format!(
            "Advisor provider '{provider_id}' stores one credential, so naming an account for it \
             is a configuration mistake. Tell the user to drop the account from `advisorModel`."
        ),
        ProviderResolveError::ProfileNotFound {
            provider_id,
            profile_id,
            available,
        } => {
            let stored = if available.is_empty() {
                "none are stored".to_string()
            } else {
                format!("stored accounts: {}", available.join(", "))
            };
            format!(
                "There is no '{provider_id}' account named '{profile_id}' ({stored}). \
                 Tell the user to fix `advisorModel`."
            )
        }
        ProviderResolveError::ProfileCredentialsMissing {
            provider_id,
            profile_id,
        } => format!(
            "The '{provider_id}' account '{profile_id}' has no usable credentials. \
             Tell the user to log into it again."
        ),
    }
}

#[async_trait]
impl Tool for AdvisorTool {
    fn name(&self) -> &str {
        claurst_core::constants::TOOL_NAME_ADVISOR
    }

    fn description(&self) -> &str {
        "Ask a second, independent model to review a decision before you act on it. \
         Use it when a change is hard to reverse, when two designs are genuinely close, \
         or when you are not confident in your own answer. Send the specific question \
         plus the material to review; the advisor cannot see the conversation. \
         Not for routine steps."
    }

    fn permission_level(&self) -> PermissionLevel {
        PermissionLevel::None
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The specific decision, claim, or trade-off to review. \
                                    State what you are about to do and what you are unsure about."
                },
                "context": {
                    "type": "string",
                    "description": "The material to review: a diff, a plan, or a code snippet. \
                                    The advisor sees nothing else, so include what it needs."
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let params: AdvisorInput = match serde_json::from_value(input) {
            Ok(params) => params,
            Err(e) => return ToolResult::error(format!("Invalid input: {}", e)),
        };

        let configured = match ctx.config.advisor_model.as_deref() {
            Some(model) if !model.trim().is_empty() => model.trim(),
            _ => {
                return ToolResult::error(
                    "No advisor model is configured. Set one with `/advisor <model>`.".to_string(),
                )
            }
        };

        let target = match parse_advisor_model(configured, ctx.config.selected_provider_id()) {
            Ok(target) => target,
            Err(e) => {
                return ToolResult::error(format!(
                    "The configured advisor model '{configured}' cannot be read: {e}. \
                     Tell the user to fix `advisorModel`."
                ))
            }
        };

        let turn = ctx.current_turn.load(std::sync::atomic::Ordering::Relaxed);
        if !claim_call_slot(turn) {
            return ToolResult::error(format!(
                "The advisor has already been consulted {MAX_CALLS_PER_TURN} times this turn. \
                 Decide with what you have, or ask the user."
            ));
        }

        let AdvisorTarget {
            provider_id,
            profile_id,
            model,
        } = target;
        debug!(
            provider = provider_id,
            profile = profile_id,
            model,
            "Consulting advisor"
        );

        let provider =
            match claurst_api::provider_by_id_for_profile(&ctx.config, provider_id, profile_id)
                .await
            {
                Ok(provider) => provider,
                Err(e) => return ToolResult::error(describe_resolve_error(&e)),
            };

        let mut prompt = params.question;
        if let Some(context) = params.context.as_deref().map(str::trim) {
            if !context.is_empty() {
                prompt.push_str("\n\n---\n\n");
                prompt.push_str(context);
            }
        }

        let request = claurst_api::ProviderRequest {
            model: model.to_string(),
            messages: vec![Message::user(prompt)],
            system_prompt: Some(claurst_api::SystemPrompt::Text(
                ADVISOR_SYSTEM_PROMPT.to_string(),
            )),
            tools: vec![],
            max_tokens: ADVISOR_MAX_TOKENS,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: vec![],
            thinking: None,
            provider_options: Value::Object(Default::default()),
        };

        let response = match provider.create_message(request).await {
            Ok(response) => response,
            Err(e) => return ToolResult::error(format!("Advisor call failed: {e}")),
        };

        // Advisor tokens are billed to the session. `CostTracker` prices every
        // token at the session model's rate, so the figure drifts when the
        // advisor model costs differently.
        ctx.cost_tracker.add_usage(
            response.usage.input_tokens,
            response.usage.output_tokens,
            response.usage.cache_creation_input_tokens,
            response.usage.cache_read_input_tokens,
        );

        let advice = text_from_blocks(&response.content);
        if advice.trim().is_empty() {
            return ToolResult::error(format!("Advisor model '{model}' returned no text."));
        }

        ToolResult::success(advice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_call_state() {
        let mut state = match CALLS_THIS_TURN.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        *state = (usize::MAX, 0);
    }

    // Model-string parsing itself is covered in `claurst_core::advisor_target`.
    // These cover what this tool adds on top: turning a failed lookup into
    // something the model can act on.

    #[test]
    fn an_unknown_account_names_the_stored_ones() {
        let message = describe_resolve_error(&ProviderResolveError::ProfileNotFound {
            provider_id: "anthropic".to_string(),
            profile_id: "missing".to_string(),
            available: vec!["personal".to_string(), "work".to_string()],
        });

        assert!(message.contains("'missing'"));
        assert!(
            message.contains("personal, work"),
            "the model should be able to quote the real ids back to the user: {message}"
        );
    }

    #[test]
    fn an_unknown_account_with_none_stored_says_so() {
        let message = describe_resolve_error(&ProviderResolveError::ProfileNotFound {
            provider_id: "codex".to_string(),
            profile_id: "work".to_string(),
            available: Vec::new(),
        });

        assert!(message.contains("none are stored"));
    }

    #[test]
    fn a_provider_without_accounts_is_reported_as_a_settings_mistake() {
        let message = describe_resolve_error(&ProviderResolveError::ProfilesUnsupported {
            provider_id: "openai".to_string(),
        });

        assert!(message.contains("advisorModel"));
    }

    #[test]
    fn every_resolve_failure_tells_the_model_to_involve_the_user() {
        let failures = [
            ProviderResolveError::NotAvailable {
                provider_id: "openai".to_string(),
            },
            ProviderResolveError::ProfilesUnsupported {
                provider_id: "openai".to_string(),
            },
            ProviderResolveError::ProfileNotFound {
                provider_id: "anthropic".to_string(),
                profile_id: "work".to_string(),
                available: Vec::new(),
            },
            ProviderResolveError::ProfileCredentialsMissing {
                provider_id: "anthropic".to_string(),
                profile_id: "work".to_string(),
            },
        ];

        for failure in failures {
            let message = describe_resolve_error(&failure);
            assert!(
                message.contains("Tell the user"),
                "the model cannot fix settings itself, so every failure must hand off: {message}"
            );
        }
    }

    #[test]
    fn the_turn_budget_caps_calls_and_resets_on_a_new_turn() {
        reset_call_state();

        for call in 1..=MAX_CALLS_PER_TURN {
            assert!(
                claim_call_slot(7),
                "call {call} of turn 7 should be allowed"
            );
        }
        assert!(
            !claim_call_slot(7),
            "the turn budget must reject the call after the cap"
        );

        assert!(
            claim_call_slot(8),
            "moving to a new turn must clear the budget"
        );

        reset_call_state();
    }
}
