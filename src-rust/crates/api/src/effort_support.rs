//! Model-adaptive effort ladders.
//!
//! Different models expose different effort ranges. Reasoning models (the OpenAI
//! reasoning family, Anthropic thinking models, Gemini thinking models) get the
//! fuller ladder up through `XHigh` (and `Max` for the top-tier models);
//! non-reasoning models get a reduced set driven purely by temperature. The
//! `Ultracode` level is *always* appended — it is a workflow overlay on top of
//! whatever top reasoning the model can do, so every model can be pushed into it.
//!
//! This is the clean, public surface the /model + /effort UI redesign (#268)
//! builds on: given a provider + model (+ optional registry), it returns exactly
//! the levels that should be selectable, in ascending order, ultracode last.

use crate::ModelRegistry;
use claurst_core::effort::EffortLevel;

/// The effort levels selectable for `provider`/`model`, ascending, with
/// [`EffortLevel::Ultracode`] always last.
///
/// - **Reasoning models** get `Low, Medium, High, XHigh` and, when the model
///   supports the very top tier (Anthropic Opus, the OpenAI gpt-5 reasoning
///   family), also `Max`.
/// - **Non-reasoning models** get `Low, Medium, High` (differentiated by
///   temperature / prompt only).
/// - `Ultracode` is appended in every case.
///
/// Reasoning capability comes from the registry entry's `reasoning` flag when a
/// `registry` is supplied and the model is known; otherwise it falls back to
/// provider/model-name heuristics.
pub fn supported_efforts(
    provider: &str,
    model: &str,
    registry: Option<&ModelRegistry>,
) -> Vec<EffortLevel> {
    let mut levels = vec![EffortLevel::Low, EffortLevel::Medium, EffortLevel::High];

    if model_is_reasoning(provider, model, registry) {
        levels.push(EffortLevel::XHigh);
        if model_supports_max_tier(provider, model) {
            levels.push(EffortLevel::Max);
        }
    }

    // Ultracode is a workflow overlay on top of the model's best reasoning, so
    // it is always available regardless of the model's native ladder.
    levels.push(EffortLevel::Ultracode);
    levels
}

/// Whether `provider`/`model` is a reasoning/thinking model.
///
/// Prefers the registry `reasoning` flag when the model is known; otherwise uses
/// provider/model-name heuristics.
pub fn model_is_reasoning(provider: &str, model: &str, registry: Option<&ModelRegistry>) -> bool {
    let bare = bare_model(model);
    if let Some(reg) = registry {
        // Try both the bare id and the raw (possibly prefixed) id.
        if let Some(entry) = reg.get(provider, bare).or_else(|| reg.get(provider, model)) {
            return entry.reasoning;
        }
    }
    reasoning_heuristic(bare)
}

/// Whether the model supports the very top `Max` reasoning tier.
///
/// Mirrors the picker's historical `model_supports_max_effort`: Anthropic Opus
/// and the OpenAI gpt-5 reasoning family (which accepts Codex's `xhigh`).
fn model_supports_max_tier(_provider: &str, model: &str) -> bool {
    let m = bare_model(model).to_ascii_lowercase();
    m.starts_with("claude-opus-4") || is_gpt5_reasoning(&m)
}

/// Name-based reasoning heuristic used when the registry has no entry.
fn reasoning_heuristic(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    // Anthropic extended-thinking families.
    let anthropic_thinking = m.starts_with("claude-3-7")
        || m.starts_with("claude-opus-4")
        || m.starts_with("claude-sonnet-4");
    // OpenAI reasoning family.
    let openai_reasoning =
        is_gpt5_reasoning(&m) || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4");
    // Gemini thinking models (2.5 and 3.x).
    let gemini_thinking =
        m.contains("gemini") && (m.contains("2.5") || m.contains("gemini-3") || m.contains("-3-"));
    anthropic_thinking || openai_reasoning || gemini_thinking
}

/// Whether `id` (already lowercased) is a gpt-5 *reasoning* model — excludes the
/// non-reasoning chat / pro snapshots that ignore `reasoning_effort`.
fn is_gpt5_reasoning(id: &str) -> bool {
    id.starts_with("gpt-5") && !id.contains("-chat") && !id.contains("-pro")
}

/// Strip a leading `provider/` (or nested `namespace/`) prefix, returning the
/// final path segment used for name-based heuristics.
fn bare_model(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_ultracode_last(levels: &[EffortLevel]) {
        assert_eq!(
            levels.last(),
            Some(&EffortLevel::Ultracode),
            "ultracode must always be last: {levels:?}"
        );
        // ...and appear exactly once.
        assert_eq!(
            levels.iter().filter(|l| l.is_ultracode()).count(),
            1,
            "ultracode must appear exactly once"
        );
    }

    #[test]
    fn reasoning_max_model_gets_full_ladder() {
        // Anthropic Opus: reasoning + max-tier.
        let levels = supported_efforts("anthropic", "claude-opus-4-8", None);
        assert_eq!(
            levels,
            vec![
                EffortLevel::Low,
                EffortLevel::Medium,
                EffortLevel::High,
                EffortLevel::XHigh,
                EffortLevel::Max,
                EffortLevel::Ultracode,
            ]
        );
        assert_ultracode_last(&levels);
    }

    #[test]
    fn reasoning_non_max_model_stops_at_xhigh() {
        // Sonnet is a thinking model but not the top Max tier.
        let levels = supported_efforts("anthropic", "claude-sonnet-4-6", None);
        assert_eq!(
            levels,
            vec![
                EffortLevel::Low,
                EffortLevel::Medium,
                EffortLevel::High,
                EffortLevel::XHigh,
                EffortLevel::Ultracode,
            ]
        );
        assert!(!levels.contains(&EffortLevel::Max));
        assert_ultracode_last(&levels);
    }

    #[test]
    fn gpt5_reasoning_family_gets_max_but_chat_pro_do_not() {
        let g5 = supported_efforts("openai", "gpt-5.5", None);
        assert!(g5.contains(&EffortLevel::XHigh));
        assert!(g5.contains(&EffortLevel::Max));
        assert_ultracode_last(&g5);

        // Non-reasoning chat / pro snapshots fall back to the reduced ladder.
        let chat = supported_efforts("openai", "gpt-5-chat-latest", None);
        assert!(!chat.contains(&EffortLevel::XHigh));
        assert!(!chat.contains(&EffortLevel::Max));
        let pro = supported_efforts("openai", "gpt-5.5-pro", None);
        assert!(!pro.contains(&EffortLevel::XHigh));
    }

    #[test]
    fn non_reasoning_model_gets_reduced_ladder() {
        let levels = supported_efforts("openai", "gpt-4o", None);
        assert_eq!(
            levels,
            vec![
                EffortLevel::Low,
                EffortLevel::Medium,
                EffortLevel::High,
                EffortLevel::Ultracode,
            ]
        );
        assert!(!levels.contains(&EffortLevel::XHigh));
        assert_ultracode_last(&levels);
    }

    #[test]
    fn gemini_thinking_gets_xhigh() {
        let levels = supported_efforts("google", "gemini-3-flash-preview", None);
        assert!(levels.contains(&EffortLevel::XHigh));
        // Gemini isn't a Max-tier model in our mapping.
        assert!(!levels.contains(&EffortLevel::Max));
        assert_ultracode_last(&levels);
    }

    #[test]
    fn ultracode_is_always_last_across_model_shapes() {
        for (p, m) in [
            ("anthropic", "claude-opus-4-8"),
            ("anthropic", "claude-haiku-4-5"),
            ("openai", "gpt-5.5"),
            ("openai", "gpt-4o"),
            ("google", "gemini-2.5-pro"),
            ("some-self-hosted", "mystery-model-1"),
        ] {
            assert_ultracode_last(&supported_efforts(p, m, None));
        }
    }

    #[test]
    fn registry_reasoning_flag_overrides_heuristic() {
        // A model whose *name* looks non-reasoning, but whose registry entry
        // says reasoning=true, must still get the fuller ladder.
        let mut registry = ModelRegistry::new();
        let json = r#"{"acme":{"id":"acme","name":"Acme","models":{"reasoner-x":{"id":"reasoner-x","name":"Reasoner X","reasoning":true,"limit":{"context":200000,"output":64000}},"plain-y":{"id":"plain-y","name":"Plain Y","reasoning":false,"limit":{"context":128000,"output":32000}}}}}"#;
        let path = std::env::temp_dir().join(format!(
            "claurst_effort_support_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).expect("write temp catalog");
        registry.load_cache(&path);
        let _ = std::fs::remove_file(&path);

        let reasoner = supported_efforts("acme", "reasoner-x", Some(&registry));
        assert!(
            reasoner.contains(&EffortLevel::XHigh),
            "registry reasoning=true must yield XHigh: {reasoner:?}"
        );

        let plain = supported_efforts("acme", "plain-y", Some(&registry));
        assert!(
            !plain.contains(&EffortLevel::XHigh),
            "registry reasoning=false must give the reduced ladder: {plain:?}"
        );
        assert_ultracode_last(&plain);
    }
}
