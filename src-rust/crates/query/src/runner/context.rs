//! Context-window accounting and compaction, run once per request.
//!
//! Compaction used to sit at the end of a turn inside the raw Anthropic
//! dispatch arm. That put it in the wrong place twice over: every provider
//! reached through the `LlmProvider` registry ran uncompacted and without a
//! token warning, and the user waited on a 20k-token summary call after their
//! answer had already finished streaming.
//!
//! It belongs at the request boundary instead, beside `sanitize_history`,
//! which the loop already calls "the single choke point covering BOTH the
//! legacy Anthropic path and the modern provider path". One call there reaches
//! both arms, and the work happens in front of the request that would
//! otherwise overflow rather than behind the one that just finished.

use claurst_core::types::Message;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::compact::{self, CompactBackend};
use crate::runner::apply_compact_result;
use crate::{QueryConfig, QueryEvent};

/// Whether this turn goes through the provider registry rather than the raw
/// Anthropic client.
///
/// The question is which wire format the account speaks, not what it is
/// called: an OAuth login named after its owner still belongs on the raw
/// client, which is the arm that refreshes an expired token. Anthropic itself
/// moves to the provider arm when the pre-built client has no key, which is
/// the case of a session that started without `ANTHROPIC_API_KEY` and gained
/// one through `/connect`.
///
/// The compaction pass and the dispatch arm both ask this, so a turn cannot be
/// summarised by one endpoint and answered by another.
pub fn dispatches_through_provider(
    account: &str,
    config: &claurst_core::Config,
    client: &claurst_api::AnthropicClient,
) -> bool {
    config.vendor_id_for_account(account) != claurst_core::ProviderId::ANTHROPIC
        || client.api_key_is_empty()
}

/// Resolve the provider that will serve this turn's account.
///
/// Both the dispatch arm and the compaction pass in front of it need the same
/// handle, and picking it twice by hand is how the two would come to disagree
/// about which endpoint a turn belongs to.
pub fn provider_for_turn(
    registry: &claurst_api::ProviderRegistry,
    config: &claurst_core::Config,
    account: &str,
) -> Option<std::sync::Arc<dyn claurst_api::provider::LlmProvider>> {
    let pid = claurst_core::provider_id::ProviderId::new(account);

    // Always prefer a fresh provider built from the auth_store so that keys
    // added at runtime via /connect are picked up immediately, even when the
    // provider was pre-registered at startup with a stale or missing key.
    let runtime_provider = claurst_api::registry::runtime_provider_for(account);
    let registry_provider = if runtime_provider.is_some() {
        None
    } else {
        registry.get(&pid).cloned()
    };
    let mut provider = runtime_provider.or(registry_provider);

    // Rebuild through the unified base resolver so overrides from settings,
    // env and defaults apply consistently.
    if claurst_api::registry::resolve_provider_api_base(config, account).is_some() {
        if let Some(overridden) = claurst_api::registry::provider_from_config(config, account) {
            provider = Some(overridden);
        }
    }

    provider
}

/// What one pass over the context boundary did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextPass {
    /// Message count before compaction; equal to `after` when nothing ran.
    pub before: usize,
    /// Message count afterwards.
    pub after: usize,
    /// The context size the next request will carry, in tokens.
    pub tokens_after: u64,
    /// Whether the conversation was actually replaced.
    pub compacted: bool,
}

/// Everything the pass needs about the turn it is fronting.
pub(crate) struct ContextPassInput<'a> {
    /// How the summary gets written, whichever arm dispatches this turn.
    pub backend: &'a dyn CompactBackend,
    /// The model that ran, following an agent override and a fallback switch.
    pub model: &'a str,
    /// The account the model is served through.
    pub provider: &'a str,
    /// The session, which owns the auto-compact circuit breaker.
    pub session_id: &'a str,
    /// The previous turn's `usage.total_input()`, or 0 before the first one.
    pub last_context_tokens: u64,
}

/// Size the context, warn about it, and compact when it is nearly full.
///
/// Call this immediately before `sanitize_history`: a cut can strand a
/// `tool_result` whose `tool_use` was summarised away, and the sanitiser
/// standing right behind it repairs exactly that.
pub(crate) async fn compact_before_request(
    messages: &mut Vec<Message>,
    config: &QueryConfig,
    input: ContextPassInput<'_>,
    event_tx: Option<&mpsc::UnboundedSender<QueryEvent>>,
    cancel_token: &CancellationToken,
) -> ContextPass {
    // Prefer the models.dev-backed registry value (correct for every provider:
    // 1M Gemini/GPT windows, 32k local models) and fall back to the
    // Claude-centric heuristic only when the registry has no usable entry.
    // (#216)
    let context_window = compact::resolve_context_window(
        config.model_registry.as_deref(),
        input.provider,
        input.model,
    );

    // Prefer the REAL context-token count the provider reported for the last
    // turn (input + cache-read + cache-creation = what the model saw) over the
    // chars/4 estimate. With prompt caching the bare `input_tokens` field
    // undercounts badly. Estimate only before the first response. (#231)
    let context_tokens = compact::estimate_context_tokens(
        messages,
        (input.last_context_tokens > 0).then_some(input.last_context_tokens),
    );

    let before = messages.len();
    let mut pass = ContextPass {
        before,
        after: before,
        tokens_after: context_tokens,
        compacted: false,
    };

    if context_window == 0 {
        return pass;
    }

    // The warning is not the compaction: it tells the user where they stand,
    // and it goes out even when auto-compact is switched off.
    let warning_state =
        compact::calculate_token_warning_state_for_window(context_tokens, context_window);
    if warning_state != compact::TokenWarningState::Ok {
        if let Some(tx) = event_tx {
            let _ = tx.send(QueryEvent::TokenWarning {
                state: warning_state,
                pct_used: context_tokens as f64 / context_window as f64,
            });
        }
    }

    // `autoCompact: false` means the user keeps the whole conversation and
    // accepts the consequence. They still get told how full it is.
    if !config.auto_compact {
        return pass;
    }

    // Reactive compact (T1-1) replaces the proactive path when its gate is set;
    // it fires from usage rather than from a finished turn and adds a 97%
    // emergency collapse. Off by default.
    if claurst_core::feature_gates::is_feature_enabled("reactive_compact") {
        run_reactive(
            messages,
            config,
            &input,
            context_tokens,
            context_window,
            event_tx,
            cancel_token,
            &mut pass,
        )
        .await;
        return pass;
    }

    if let Some(new_msgs) = compact::auto_compact_if_needed(
        input.backend,
        messages,
        context_tokens,
        input.model,
        context_window,
        config.compact_threshold,
        input.session_id,
    )
    .await
    {
        pass.after = new_msgs.len();
        pass.tokens_after = compact::estimate_tokens_for_messages(&new_msgs) as u64;
        pass.compacted = true;
        *messages = new_msgs;
    }

    pass
}

/// The gated reactive path: emergency collapse first, then a normal compact.
#[allow(clippy::too_many_arguments)]
async fn run_reactive(
    messages: &mut Vec<Message>,
    config: &QueryConfig,
    input: &ContextPassInput<'_>,
    context_tokens: u64,
    context_window: u64,
    event_tx: Option<&mpsc::UnboundedSender<QueryEvent>>,
    cancel_token: &CancellationToken,
    pass: &mut ContextPass,
) {
    // Both calls take a clone, and `apply_compact_result` only overwrites
    // `*messages` on success, so a failed compaction cannot wipe the live
    // conversation (#213).
    let (label, outcome) = if compact::should_context_collapse(context_tokens, context_window) {
        if let Some(tx) = event_tx {
            let _ = tx.send(QueryEvent::Status(
                "Compacting context... (emergency collapse)".to_string(),
            ));
        }
        (
            "Context-collapse",
            compact::context_collapse(messages.clone(), input.backend, config).await,
        )
    } else if compact::should_compact(context_tokens, context_window, config.compact_threshold) {
        if let Some(tx) = event_tx {
            let _ = tx.send(QueryEvent::Status("Compacting context...".to_string()));
        }
        (
            "Reactive compact",
            compact::reactive_compact(
                messages.clone(),
                input.backend,
                config,
                cancel_token.clone(),
                &[],
            )
            .await,
        )
    } else {
        return;
    };

    match apply_compact_result(messages, outcome) {
        Ok(tokens_freed) => {
            info!(tokens_freed, "{label} complete");
            pass.after = messages.len();
            pass.tokens_after = compact::estimate_tokens_for_messages(messages) as u64;
            pass.compacted = true;
        }
        Err(claurst_core::error::ClaudeError::Cancelled) => {
            warn!("{label} was cancelled; conversation preserved");
        }
        Err(e) => {
            warn!(error = %e, "{label} failed; conversation preserved");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claurst_core::error::ClaudeError;

    /// A summariser that answers with a fixed string and remembers the model
    /// it was asked to use.
    struct StubBackend {
        reply: Result<String, String>,
        model_seen: parking_lot::Mutex<Option<String>>,
    }

    impl StubBackend {
        fn answering(reply: &str) -> Self {
            Self {
                reply: Ok(reply.to_string()),
                model_seen: parking_lot::Mutex::new(None),
            }
        }

        fn failing() -> Self {
            Self {
                reply: Err("the summariser is down".to_string()),
                model_seen: parking_lot::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl CompactBackend for StubBackend {
        async fn summarise(
            &self,
            _system: &str,
            _user: &str,
            model: &str,
            _max_tokens: u32,
        ) -> Result<String, ClaudeError> {
            *self.model_seen.lock() = Some(model.to_string());
            self.reply
                .clone()
                .map_err(|e| ClaudeError::Other(e.to_string()))
        }
    }

    /// A conversation past the trigger threshold of a 200k window.
    fn a_full_conversation() -> Vec<Message> {
        vec![
            Message::user("x".repeat(400_000)),
            Message::assistant("y".repeat(400_000)),
            Message::user("and now the next thing"),
        ]
    }

    fn input<'a>(backend: &'a StubBackend, model: &'a str) -> ContextPassInput<'a> {
        ContextPassInput {
            backend,
            model,
            provider: "anthropic",
            session_id: "context-pass-tests",
            last_context_tokens: 0,
        }
    }

    async fn run(messages: &mut Vec<Message>, backend: &StubBackend, model: &str) -> ContextPass {
        let config = QueryConfig::default();
        compact_before_request(
            messages,
            &config,
            input(backend, model),
            None,
            &CancellationToken::new(),
        )
        .await
    }

    /// The pass acts on a full window, whichever arm supplied the backend.
    #[tokio::test]
    async fn a_full_window_is_compacted_at_the_request_boundary() {
        compact::forget_compact_state("context-pass-tests");
        let mut messages = a_full_conversation();
        let backend = StubBackend::answering("What went before, in short.");

        let pass = run(&mut messages, &backend, "claude-opus-4-5").await;

        assert!(pass.compacted, "a full window compacts");
        assert!(pass.after < pass.before, "the head was replaced");
        assert_eq!(messages.len(), pass.after);
        assert!(
            pass.tokens_after < 800_000,
            "the reported size follows the shortened conversation"
        );
        compact::forget_compact_state("context-pass-tests");
    }

    /// The summariser is asked for the model that ran, not the session model.
    #[tokio::test]
    async fn the_summariser_is_given_the_model_that_ran() {
        compact::forget_compact_state("context-pass-tests");
        let mut messages = a_full_conversation();
        let backend = StubBackend::answering("Short.");

        run(&mut messages, &backend, "some-agent-override-model").await;

        assert_eq!(
            backend.model_seen.lock().as_deref(),
            Some("some-agent-override-model")
        );
        compact::forget_compact_state("context-pass-tests");
    }

    /// A conversation nowhere near the threshold is left exactly as it was.
    #[tokio::test]
    async fn a_short_conversation_is_left_alone() {
        compact::forget_compact_state("context-pass-tests");
        let mut messages = vec![Message::user("hello"), Message::assistant("hi")];
        let backend = StubBackend::answering("never asked for");

        let pass = run(&mut messages, &backend, "claude-opus-4-5").await;

        assert!(!pass.compacted);
        assert_eq!(pass.before, pass.after);
        assert_eq!(messages.len(), 2);
        assert!(backend.model_seen.lock().is_none(), "no call was made");
        compact::forget_compact_state("context-pass-tests");
    }

    /// `autoCompact: false` keeps the conversation whole. The setting used to
    /// be written, saved and read by nobody.
    #[tokio::test]
    async fn auto_compact_off_leaves_a_full_window_alone() {
        compact::forget_compact_state("context-pass-tests");
        let mut messages = a_full_conversation();
        let backend = StubBackend::answering("never asked for");
        let config = QueryConfig {
            auto_compact: false,
            ..QueryConfig::default()
        };

        let pass = compact_before_request(
            &mut messages,
            &config,
            input(&backend, "claude-opus-4-5"),
            None,
            &CancellationToken::new(),
        )
        .await;

        assert!(!pass.compacted);
        assert_eq!(messages.len(), 3);
        assert!(backend.model_seen.lock().is_none(), "no call was made");
        compact::forget_compact_state("context-pass-tests");
    }

    /// A lower `compactThreshold` compacts a conversation the default would
    /// have left alone.
    #[tokio::test]
    async fn a_lower_threshold_compacts_sooner() {
        compact::forget_compact_state("context-pass-threshold");
        // ~50k tokens: a quarter of a 200k window, well under the default 90%.
        let mut messages = vec![
            Message::user("x".repeat(100_000)),
            Message::assistant("y".repeat(100_000)),
            Message::user("and now the next thing"),
        ];

        let at_default = compact_before_request(
            &mut messages.clone(),
            &QueryConfig::default(),
            ContextPassInput {
                backend: &StubBackend::answering("short"),
                model: "claude-opus-4-5",
                provider: "anthropic",
                session_id: "context-pass-threshold",
                last_context_tokens: 0,
            },
            None,
            &CancellationToken::new(),
        )
        .await;
        assert!(!at_default.compacted, "the default leaves this alone");

        let config = QueryConfig {
            compact_threshold: 20,
            ..QueryConfig::default()
        };
        let lowered = compact_before_request(
            &mut messages,
            &config,
            ContextPassInput {
                backend: &StubBackend::answering("short"),
                model: "claude-opus-4-5",
                provider: "anthropic",
                session_id: "context-pass-threshold",
                last_context_tokens: 0,
            },
            None,
            &CancellationToken::new(),
        )
        .await;
        assert!(lowered.compacted, "a threshold of 20 acts on the same size");
        compact::forget_compact_state("context-pass-threshold");
    }

    /// A summariser that fails leaves the conversation whole (#213).
    #[tokio::test]
    async fn a_failed_summary_leaves_the_conversation_intact() {
        compact::forget_compact_state("context-pass-failure");
        let mut messages = a_full_conversation();
        let before = messages.clone();
        let backend = StubBackend::failing();

        let pass = compact_before_request(
            &mut messages,
            &QueryConfig::default(),
            ContextPassInput {
                backend: &backend,
                model: "claude-opus-4-5",
                provider: "anthropic",
                session_id: "context-pass-failure",
                last_context_tokens: 0,
            },
            None,
            &CancellationToken::new(),
        )
        .await;

        assert!(!pass.compacted);
        assert_eq!(messages.len(), before.len());
        assert_eq!(messages[0].get_all_text(), before[0].get_all_text());
        compact::forget_compact_state("context-pass-failure");
    }
}
