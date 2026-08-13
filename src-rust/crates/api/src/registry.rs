// registry.rs — Registry of all available LLM providers.
//
// Holds an `Arc<dyn LlmProvider>` for each registered provider and exposes
// lookup, health-check, and default-provider helpers.

use std::collections::HashMap;
use std::sync::Arc;

use claurst_core::ProviderId;

use crate::client::ClientConfig;
use crate::provider::LlmProvider;
use crate::provider_types::ProviderStatus;
use crate::providers::{
    AnthropicProvider, AzureProvider, BedrockProvider, CodexProvider, CohereProvider,
    CopilotProvider, FreeEntry, FreeProvider, GoogleProvider, MinimaxProvider, OpenAiProvider,
    FREE_CATALOG,
};

fn normalize_openai_compat_base(override_base: &str) -> String {
    let trimmed = override_base.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{}/v1", trimmed)
    }
}

fn normalize_openai_base(override_base: &str) -> String {
    let trimmed = override_base.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.trim_end_matches("/v1").to_string()
    } else {
        trimmed.to_string()
    }
}

fn canonical_local_provider_id(provider_id: &str) -> &str {
    match provider_id {
        "lmstudio" => ProviderId::LM_STUDIO,
        "llamacpp" | "llama-server" => ProviderId::LLAMA_CPP,
        _ => provider_id,
    }
}

pub fn resolve_provider_api_base(
    config: &claurst_core::config::Config,
    provider_id: &str,
) -> Option<String> {
    let base = config.resolve_provider_api_base(provider_id)?;
    if provider_id == "openai" {
        Some(normalize_openai_base(&base))
    } else if crate::providers::openai_compat_providers::provider_for_id(provider_id).is_some() {
        Some(normalize_openai_compat_base(&base))
    } else {
        Some(base)
    }
}

/// Registry of all available LLM providers.
/// Holds `Arc<dyn LlmProvider>` for each registered provider.
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn LlmProvider>>,
    default_provider_id: ProviderId,
}

fn provider_from_key(provider_id: &str, key: String) -> Option<Arc<dyn LlmProvider>> {
    use crate::providers::openai_compat_providers as p;

    if let Some(provider) = p::provider_for_id(provider_id) {
        return Some(Arc::new(provider.with_api_key(key)));
    }

    match provider_id {
        "anthropic" => Some(Arc::new(AnthropicProvider::from_config(ClientConfig {
            api_key: key,
            ..Default::default()
        }))),
        "minimax" => Some(Arc::new(MinimaxProvider::new(key))),
        "openai" => Some(Arc::new(OpenAiProvider::new(key))),
        "google" => Some(Arc::new(GoogleProvider::new(key))),
        "github-copilot" => Some(Arc::new(CopilotProvider::new(key))),
        "codex" | "openai-codex" => {
            // The Codex provider is OAuth-based; the `key` field is not used.
            // Load from the stored token file instead.
            CodexProvider::from_stored().map(|p| Arc::new(p) as Arc<dyn LlmProvider>)
        }
        "cohere" => Some(Arc::new(CohereProvider::new(key))),
        "custom-openai" => Some(Arc::new(p::custom_openai().with_api_key(key))),
        // The base URL comes from settings, so the key alone cannot build it.
        "custom-anthropic" => custom_anthropic_provider(),
        // "free" needs two keys (Zen + OpenRouter) — single-key path doesn't
        // apply.  The auth-store-aware path `runtime_provider_for` handles it.
        "free" => build_free_provider(),
        _ => None,
    }
}

/// Build a [`FreeProvider`] by walking [`FREE_CATALOG`] and pulling any keys
/// the user has stored in the auth store. Each catalog entry whose upstream
/// has a key becomes one link in the fallback chain.
///
/// Returns `None` only if *no* catalog entry has a configured key — a single
/// key is enough to run, and more is better.
pub fn build_free_provider() -> Option<Arc<dyn LlmProvider>> {
    let auth_store = claurst_core::AuthStore::load();
    let mut chain: Vec<FreeEntry> = Vec::new();

    for upstream in FREE_CATALOG {
        let key = match upstream.id {
            // OpenCode Zen and Go share `OPENCODE_API_KEY`; accept either slot.
            "opencode-zen" => auth_store
                .api_key_for(claurst_core::ProviderId::OPENCODE_ZEN)
                .or_else(|| auth_store.api_key_for(claurst_core::ProviderId::OPENCODE_GO)),
            other => auth_store.api_key_for(other),
        }
        .filter(|k| !k.trim().is_empty());

        let Some(key) = key else {
            continue;
        };

        let provider: Option<Arc<dyn LlmProvider>> = match upstream.id {
            "google" => Some(Arc::new(GoogleProvider::new(key))),
            "cohere" => Some(Arc::new(CohereProvider::new(key))),
            id => crate::providers::openai_compat_providers::provider_for_id(id)
                .map(|p| Arc::new(p.with_api_key(key)) as Arc<dyn LlmProvider>),
        };

        if let Some(provider) = provider {
            chain.push(FreeEntry {
                upstream: *upstream,
                provider,
            });
        }
    }

    if chain.is_empty() {
        return None;
    }
    Some(Arc::new(FreeProvider::new(chain)) as Arc<dyn LlmProvider>)
}

/// Build the user-supplied Anthropic-compatible provider from settings.
///
/// Needs a base URL: without one there is nothing to point at, and falling
/// back to the real Anthropic endpoint would make this a confusing duplicate.
/// The key may be empty, because a self-hosted gateway can be unauthenticated.
pub fn custom_anthropic_provider() -> Option<Arc<dyn LlmProvider>> {
    let settings = claurst_core::config::Settings::load_sync().unwrap_or_default();
    let provider_id = ProviderId::CUSTOM_ANTHROPIC;
    let entry = settings.providers.get(provider_id);
    if entry.is_some_and(|provider| !provider.enabled) {
        return None;
    }

    let base_url = entry
        .and_then(|provider| provider.api_base.clone())
        .filter(|url| !url.trim().is_empty())
        .or_else(|| std::env::var("CUSTOM_ANTHROPIC_BASE_URL").ok())
        .filter(|url| !url.trim().is_empty())?;

    let api_key = entry
        .and_then(|provider| provider.api_key.clone())
        .filter(|key| !key.trim().is_empty())
        .or_else(|| claurst_core::AuthStore::load().api_key_for(provider_id))
        .unwrap_or_default();

    Some(Arc::new(AnthropicProvider::from_config_with_id(
        ClientConfig {
            api_key,
            api_base: base_url,
            ..Default::default()
        },
        provider_id,
    )))
}

pub fn provider_from_config(
    config: &claurst_core::config::Config,
    provider_id: &str,
) -> Option<Arc<dyn LlmProvider>> {
    let provider_cfg = config.provider_configs.get(provider_id);
    if provider_cfg.is_some_and(|provider| !provider.enabled) {
        return None;
    }

    let api_key = config.resolve_provider_api_key(provider_id);
    let api_base = resolve_provider_api_base(config, provider_id).filter(|base| !base.is_empty());

    use crate::providers;

    match provider_id {
        "anthropic" => None,
        // Built from settings rather than the resolved key, because it needs a
        // base URL and speaks the Anthropic wire format, not OpenAI's.
        "custom-anthropic" => custom_anthropic_provider(),
        // Composite "Free" provider — two keys are pulled internally from the
        // auth store; the `api_key` resolved above is ignored.
        "free" => build_free_provider(),
        "openai" => {
            let mut provider = OpenAiProvider::new(api_key.unwrap_or_default());
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "google" => api_key.map(|key| Arc::new(GoogleProvider::new(key)) as Arc<dyn LlmProvider>),
        "minimax" => api_key.map(|key| {
            let mut provider = MinimaxProvider::new(key);
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            if let Some(service_tier) = provider_cfg
                .and_then(|config| config.options.get("service_tier"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
            {
                provider = provider.with_service_tier(service_tier);
            }
            Arc::new(provider) as Arc<dyn LlmProvider>
        }),
        "azure" => {
            let resource_name = provider_cfg
                .and_then(|provider| provider.options.get("resource_name"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    std::env::var("AZURE_RESOURCE_NAME")
                        .ok()
                        .filter(|value| !value.is_empty())
                });

            match (resource_name, api_key) {
                (Some(resource_name), Some(key)) => {
                    Some(Arc::new(AzureProvider::new(resource_name, key)) as Arc<dyn LlmProvider>)
                }
                _ => None,
            }
        }
        "ollama" => {
            let mut provider = providers::ollama();
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "lmstudio" | "lm-studio" => {
            let mut provider = providers::lm_studio();
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "llamacpp" | "llama-cpp" | "llama-server" => {
            let mut provider = providers::llama_cpp();
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "deepseek" => {
            let mut provider = providers::deepseek();
            if let Some(key) = api_key {
                provider = provider.with_api_key(key);
            }
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "groq" => {
            let mut provider = providers::groq();
            if let Some(key) = api_key {
                provider = provider.with_api_key(key);
            }
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "xai" => {
            let mut provider = providers::xai();
            if let Some(key) = api_key {
                provider = provider.with_api_key(key);
            }
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "openrouter" => {
            let mut provider = providers::openrouter();
            if let Some(key) = api_key {
                provider = provider.with_api_key(key);
            }
            if let Some(base) = api_base {
                provider = provider.with_base_url(base);
            }
            Some(Arc::new(provider))
        }
        "cohere" => api_key.map(|key| Arc::new(CohereProvider::new(key)) as Arc<dyn LlmProvider>),
        "github-copilot" => {
            api_key.map(|key| Arc::new(CopilotProvider::new(key)) as Arc<dyn LlmProvider>)
        }
        "codex" | "openai-codex" => {
            CodexProvider::from_stored().map(|provider| Arc::new(provider) as Arc<dyn LlmProvider>)
        }
        _ => api_key.and_then(|key| provider_from_key(provider_id, key)),
    }
}

pub fn runtime_provider_for(provider_id: &str) -> Option<Arc<dyn LlmProvider>> {
    use crate::providers::openai_compat_providers as p;

    // Local providers never require an API key — build them directly so that
    // the auth-store bypass below doesn't silently drop them.
    // Accept both the hyphenated canonical IDs ("llama-cpp", "lm-studio") and
    // the non-hyphenated aliases ("llamacpp", "lmstudio") used throughout the
    // TUI / connect dialog.
    match provider_id {
        "ollama" => return Some(Arc::new(p::ollama())),
        "lmstudio" | "lm-studio" => return Some(Arc::new(p::lm_studio())),
        // "llama-server" is the binary name for the modern llama.cpp server.
        "llamacpp" | "llama-cpp" | "llama-server" => return Some(Arc::new(p::llama_cpp())),
        "codex" | "openai-codex" => {
            return CodexProvider::from_stored().map(|p| Arc::new(p) as Arc<dyn LlmProvider>);
        }
        // "free" pulls two keys (Zen + OpenRouter) from the auth store and
        // wraps them in a fallback composite — handled here so the generic
        // single-key path below doesn't short-circuit on a missing key.
        "free" => return build_free_provider(),
        _ => {}
    }

    let auth_store = claurst_core::AuthStore::load();
    let key = auth_store.api_key_for(provider_id)?;
    if key.is_empty() {
        return None;
    }
    provider_from_key(provider_id, key)
}

impl ProviderRegistry {
    /// Create an empty registry with Anthropic as the default provider ID.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            default_provider_id: ProviderId::new(ProviderId::ANTHROPIC),
        }
    }

    /// Register a provider. Returns `&mut self` for builder chaining.
    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) -> &mut Self {
        let id = provider.id().clone();
        self.providers.insert(id, provider);
        self
    }

    /// Set the default provider by ID.
    ///
    /// # Panics
    /// Panics if no provider with that ID has been registered.
    pub fn set_default(&mut self, id: ProviderId) -> &mut Self {
        let canonical_id = ProviderId::new(canonical_local_provider_id(&id));
        assert!(
            self.providers.contains_key(&canonical_id),
            "set_default: provider '{}' is not registered",
            id,
        );
        self.default_provider_id = canonical_id;
        self
    }

    /// Get a provider by ID.
    pub fn get(&self, id: &ProviderId) -> Option<&Arc<dyn LlmProvider>> {
        self.providers.get(id).or_else(|| {
            let canonical_id = canonical_local_provider_id(id);
            (canonical_id != &**id)
                .then(|| self.providers.get(&ProviderId::new(canonical_id)))
                .flatten()
        })
    }

    /// Get the default provider.
    pub fn default_provider(&self) -> Option<&Arc<dyn LlmProvider>> {
        self.providers.get(&self.default_provider_id)
    }

    /// Get the default provider ID.
    pub fn default_provider_id(&self) -> &ProviderId {
        &self.default_provider_id
    }

    /// List all registered provider IDs.
    pub fn provider_ids(&self) -> Vec<&ProviderId> {
        self.providers.keys().collect()
    }

    /// Check health of all providers sequentially.
    /// Returns `(provider_id, status)` pairs.
    pub async fn check_all_health(&self) -> Vec<(ProviderId, ProviderStatus)> {
        let mut results = Vec::new();
        for (id, provider) in &self.providers {
            let status = provider
                .health_check()
                .await
                .unwrap_or(ProviderStatus::Unavailable {
                    reason: "health check failed".to_string(),
                });
            results.push((id.clone(), status));
        }
        results
    }

    /// Convenience: build a registry with just Anthropic registered as the
    /// default provider.  Takes the same [`ClientConfig`] that
    /// [`AnthropicClient`] takes.
    ///
    /// [`AnthropicClient`]: crate::client::AnthropicClient
    pub fn with_anthropic(config: ClientConfig) -> Self {
        let mut registry = Self::new();
        let provider = Arc::new(AnthropicProvider::from_config(config));
        registry.register(provider);
        registry
    }

    pub fn from_config(
        config: &claurst_core::config::Config,
        anthropic_config: ClientConfig,
    ) -> Self {
        // Apply the user-configured request timeout (issue #175) before any
        // provider HTTP clients are built, so they all honour it. Uses the
        // active provider's resolved value (per-provider override or global).
        crate::set_request_timeout_secs(
            config.resolve_request_timeout_secs(config.selected_provider_id()),
        );
        let mut registry = Self::from_environment_with_auth_store(anthropic_config);
        let active_provider = config.selected_provider_id();

        let mut configured_provider_ids: Vec<String> =
            config.provider_configs.keys().cloned().collect();
        if configured_provider_ids
            .iter()
            .all(|id| id != active_provider)
        {
            configured_provider_ids.push(active_provider.to_string());
        }

        for provider_id in configured_provider_ids {
            if let Some(provider) = provider_from_config(config, &provider_id) {
                registry.register(provider);
            }
        }

        let default_provider_id = ProviderId::new(active_provider);
        if registry.get(&default_provider_id).is_some() {
            registry.set_default(default_provider_id);
        }

        registry
    }

    /// Register [`GoogleProvider`] if `GOOGLE_API_KEY` or
    /// `GOOGLE_GENERATIVE_AI_API_KEY` is set in the environment.
    /// Returns `&mut self` for builder chaining.
    pub fn with_google_if_key_set(&mut self) -> &mut Self {
        let key = std::env::var("GOOGLE_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_GENERATIVE_AI_API_KEY"));
        if let Ok(key) = key {
            let provider = Arc::new(GoogleProvider::new(key));
            self.register(provider);
        }
        self
    }

    /// Register [`OpenAiProvider`] if `OPENAI_API_KEY` is set in the
    /// environment.  Returns `&mut self` for builder chaining.
    pub fn with_openai_if_key_set(&mut self) -> &mut Self {
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            let provider = Arc::new(OpenAiProvider::new(key));
            self.register(provider);
        }
        self
    }

    /// Register [`AzureProvider`] if `AZURE_API_KEY` and `AZURE_RESOURCE_NAME`
    /// are set in the environment.  Returns `&mut self` for builder chaining.
    pub fn with_azure_if_configured(&mut self) -> &mut Self {
        if let Some(p) = AzureProvider::from_env() {
            self.register(Arc::new(p));
        }
        self
    }

    /// Register [`BedrockProvider`] if AWS credentials are available in the
    /// environment (`AWS_ACCESS_KEY_ID`+`AWS_SECRET_ACCESS_KEY` or
    /// `AWS_BEARER_TOKEN_BEDROCK`).  Returns `&mut self` for builder chaining.
    pub fn with_bedrock_if_configured(&mut self) -> &mut Self {
        if let Some(p) = BedrockProvider::from_env() {
            self.register(Arc::new(p));
        }
        self
    }

    /// Register [`CopilotProvider`] if `GITHUB_TOKEN` is set in the environment.
    /// Returns `&mut self` for builder chaining.
    pub fn with_copilot_if_configured(&mut self) -> &mut Self {
        if let Some(p) = CopilotProvider::from_env() {
            self.register(Arc::new(p));
        }
        self
    }

    /// Register [`CodexProvider`] if stored Codex OAuth tokens are available in
    /// `~/.claurst/codex_tokens.json`.  Returns `&mut self` for builder chaining.
    pub fn with_codex_if_configured(&mut self) -> &mut Self {
        if let Some(p) = CodexProvider::from_stored() {
            self.register(Arc::new(p));
        }
        self
    }

    /// Register [`CohereProvider`] if `COHERE_API_KEY` is set in the environment.
    /// Returns `&mut self` for builder chaining.
    pub fn with_cohere_if_key_set(&mut self) -> &mut Self {
        if let Some(p) = CohereProvider::from_env() {
            self.register(Arc::new(p));
        }
        self
    }

    /// Register the user-supplied Anthropic-compatible endpoint when one is
    /// configured.
    ///
    /// Registered under its own id so it sits alongside the real Anthropic
    /// provider; overriding `providers.anthropic.api_base` would replace it.
    pub fn with_custom_anthropic_if_configured(&mut self) -> &mut Self {
        if let Some(provider) = custom_anthropic_provider() {
            self.register(provider);
        }
        self
    }

    /// Build a registry with **all** providers that have credentials configured
    /// in the environment.  Anthropic is always the default provider.
    ///
    /// This is the recommended constructor for production use.
    pub fn from_environment(anthropic_config: ClientConfig) -> Self {
        let mut registry = Self::with_anthropic(anthropic_config);
        registry
            .with_openai_if_key_set()
            .with_google_if_key_set()
            .with_azure_if_configured()
            .with_bedrock_if_configured()
            .with_copilot_if_configured()
            .with_codex_if_configured()
            .with_cohere_if_key_set()
            .with_custom_anthropic_if_configured()
            .with_available_providers();
        registry
    }

    /// Build a registry that checks **both** environment variables and the
    /// persistent [`AuthStore`] (`~/.claurst/auth.json`) for credentials.
    ///
    /// This ensures that API keys stored via `/connect` or `claurst auth` are
    /// picked up at startup, not just env vars.  Falls back to
    /// `from_environment` for providers that only support env-var config, and
    /// adds any extra providers that have keys in the auth store.
    ///
    /// [`AuthStore`]: claurst_core::AuthStore
    pub fn from_environment_with_auth_store(anthropic_config: ClientConfig) -> Self {
        // Start with env-based registration.
        let mut registry = Self::from_environment(anthropic_config);

        // Now check the auth store for providers that weren't registered from
        // env vars.
        let auth_store = claurst_core::AuthStore::load();

        for provider_id in auth_store.credentials.keys() {
            let pid = claurst_core::ProviderId::new(provider_id.as_str());
            // Skip if already registered from env vars.
            if registry.get(&pid).is_some() {
                continue;
            }
            // Try to get a usable key from the auth store.
            if let Some(key) = auth_store.api_key_for(provider_id) {
                if key.is_empty() {
                    continue;
                }
                let provider = provider_from_key(provider_id, key);
                if let Some(p) = provider {
                    registry.register(p);
                }
            }
        }

        registry
    }

    /// Register all providers that have environment variable credentials set.
    ///
    /// Local providers (Ollama, LM Studio, llama.cpp) are always registered
    /// regardless of credentials — `health_check()` will report them as
    /// unavailable if the server is not running.
    ///
    /// Remote API-key providers are only registered when their respective
    /// environment variables are set (non-empty).
    ///
    /// Returns `&mut self` for builder chaining.
    pub fn with_available_providers(&mut self) -> &mut Self {
        use crate::providers::openai_compat_providers as p;

        // Local providers — always try to register.
        self.register(Arc::new(p::ollama()));
        self.register(Arc::new(p::lm_studio()));
        self.register(Arc::new(p::llama_cpp()));

        // Remote providers — only register when an API key is present.
        if std::env::var("DEEPSEEK_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::deepseek()));
        }
        if std::env::var("GROQ_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::groq()));
        }
        if std::env::var("XAI_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::xai()));
        }
        if std::env::var("OPENROUTER_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::openrouter()));
        }
        if std::env::var("TOGETHER_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::together_ai()));
        }
        if std::env::var("PERPLEXITY_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::perplexity()));
        }
        if std::env::var("CEREBRAS_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::cerebras()));
        }
        if std::env::var("DEEPINFRA_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::deepinfra()));
        }
        if std::env::var("VENICE_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::venice()));
        }
        if std::env::var("DASHSCOPE_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::qwen()));
        }
        if std::env::var("MISTRAL_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::mistral()));
        }
        if std::env::var("SAMBANOVA_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::sambanova()));
        }
        if std::env::var("HF_TOKEN")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::huggingface()));
        }
        if std::env::var("MINIMAX_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            let key = std::env::var("MINIMAX_API_KEY").unwrap_or_default();
            self.register(Arc::new(MinimaxProvider::new(key)));
        }
        if std::env::var("NVIDIA_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::nvidia()));
        }
        if std::env::var("SILICONFLOW_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::siliconflow()));
        }
        if std::env::var("MOONSHOT_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::moonshot()));
        }
        if std::env::var("ZHIPU_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::zhipu()));
        }
        if std::env::var("ZAI_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::zai()));
        }
        if std::env::var("NEBIUS_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::nebius()));
        }
        if std::env::var("NOVITA_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::novita()));
        }
        if std::env::var("OVHCLOUD_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::ovhcloud()));
        }
        if std::env::var("SCALEWAY_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::scaleway()));
        }
        if std::env::var("VULTR_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::vultr_ai()));
        }
        if std::env::var("BASETEN_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::baseten()));
        }
        if std::env::var("FRIENDLI_TOKEN")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::friendli()));
        }
        if std::env::var("UPSTAGE_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::upstage()));
        }
        if std::env::var("STEPFUN_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::stepfun()));
        }
        if std::env::var("FIREWORKS_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::fireworks()));
        }
        if std::env::var("OPENCODE_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            self.register(Arc::new(p::opencode_go()));
        }
        self
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Every spelling a provider is registered under, so a lookup by the id the
/// user typed still finds it. `ProviderRegistry::get` already canonicalises
/// local runtimes; this covers the hyphenation variants of hosted providers.
pub fn provider_lookup_ids(provider_id: &str) -> Vec<&str> {
    match provider_id {
        "togetherai" | "together-ai" => vec!["togetherai", "together-ai"],
        "lmstudio" | "lm-studio" => vec!["lmstudio", "lm-studio"],
        "llamacpp" | "llama-cpp" | "llama-server" => {
            vec!["llamacpp", "llama-cpp", "llama-server"]
        }
        "moonshot" | "moonshotai" => vec!["moonshot", "moonshotai"],
        "zhipu" | "zhipuai" => vec!["zhipu", "zhipuai"],
        "vultr" | "vultr-ai" => vec!["vultr", "vultr-ai"],
        "google" | "google-vertex" => vec!["google", "google-vertex"],
        _ => vec![provider_id],
    }
}

/// Build a registry from `config` and hand back the provider registered under
/// `provider_id`, or `None` when that provider has no usable credentials.
///
/// Anthropic auth is resolved first because the registry needs it to construct
/// the Anthropic client even when the caller wants a different provider.
pub async fn provider_by_id(
    config: &claurst_core::config::Config,
    provider_id: &str,
) -> Option<Arc<dyn LlmProvider>> {
    let anthropic_auth = config.resolve_anthropic_auth_async().await;
    let registry = ProviderRegistry::from_config(
        config,
        crate::client::ClientConfig {
            api_key: anthropic_auth
                .as_ref()
                .map(|(credential, _)| credential.clone())
                .unwrap_or_default(),
            api_base: config.resolve_anthropic_api_base(),
            use_bearer_auth: anthropic_auth
                .as_ref()
                .is_some_and(|(_, use_bearer)| *use_bearer),
            ..Default::default()
        },
    );

    provider_lookup_ids(provider_id)
        .into_iter()
        .find_map(|lookup_id| registry.get(&ProviderId::new(lookup_id)).cloned())
}

/// Hand back the provider selected by `config`.
pub async fn provider_for_config(
    config: &claurst_core::config::Config,
) -> Option<Arc<dyn LlmProvider>> {
    provider_by_id(config, config.selected_provider_id()).await
}

/// Why a provider could not be resolved.
///
/// `provider_by_id` collapses every failure into `None`. Naming an account
/// adds failures the caller has to tell apart, because "this provider has no
/// accounts" and "this account does not exist" call for different advice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResolveError {
    /// The provider is not registered, or it has no usable credentials.
    NotAvailable { provider_id: String },
    /// The provider stores a single credential, so it has no accounts to pick.
    ProfilesUnsupported { provider_id: String },
    /// No account with that id is stored for the provider.
    ProfileNotFound {
        provider_id: String,
        profile_id: String,
        available: Vec<String>,
    },
    /// The account exists in the registry but its stored tokens are missing or
    /// unusable.
    ProfileCredentialsMissing {
        provider_id: String,
        profile_id: String,
    },
}

impl std::fmt::Display for ProviderResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAvailable { provider_id } => {
                write!(f, "provider '{provider_id}' is not available")
            }
            Self::ProfilesUnsupported { provider_id } => write!(
                f,
                "provider '{provider_id}' stores one credential and has no accounts to choose from"
            ),
            Self::ProfileNotFound {
                provider_id,
                profile_id,
                available,
            } => {
                write!(f, "no '{provider_id}' account named '{profile_id}'")?;
                if available.is_empty() {
                    write!(f, "; none are stored")
                } else {
                    write!(f, "; stored accounts: {}", available.join(", "))
                }
            }
            Self::ProfileCredentialsMissing {
                provider_id,
                profile_id,
            } => write!(
                f,
                "the '{provider_id}' account '{profile_id}' has no usable credentials"
            ),
        }
    }
}

impl std::error::Error for ProviderResolveError {}

/// Hand back a provider, optionally authenticated as a named account profile.
///
/// With no `profile_id` this is `provider_by_id` with a typed error. With one,
/// the provider is built directly from that account's stored credentials
/// rather than through the registry, because the registry always resolves the
/// *active* account.
pub async fn provider_by_id_for_profile(
    config: &claurst_core::config::Config,
    provider_id: &str,
    profile_id: Option<&str>,
) -> Result<Arc<dyn LlmProvider>, ProviderResolveError> {
    let Some(profile_id) = profile_id else {
        return provider_by_id(config, provider_id).await.ok_or_else(|| {
            ProviderResolveError::NotAvailable {
                provider_id: provider_id.to_string(),
            }
        });
    };

    if !claurst_core::accounts::provider_supports_profiles(provider_id) {
        return Err(ProviderResolveError::ProfilesUnsupported {
            provider_id: provider_id.to_string(),
        });
    }

    let registry = claurst_core::accounts::AccountRegistry::load();
    if registry.get(provider_id, profile_id).is_none() {
        return Err(ProviderResolveError::ProfileNotFound {
            provider_id: provider_id.to_string(),
            profile_id: profile_id.to_string(),
            available: registry
                .list(provider_id)
                .into_iter()
                .map(|profile| profile.id)
                .collect(),
        });
    }

    let missing = || ProviderResolveError::ProfileCredentialsMissing {
        provider_id: provider_id.to_string(),
        profile_id: profile_id.to_string(),
    };

    match provider_id {
        claurst_core::accounts::PROVIDER_ANTHROPIC => {
            let (credential, use_bearer_auth) =
                claurst_core::oauth::resolve_auth_for_profile(profile_id)
                    .await
                    .ok_or_else(missing)?;
            // Built directly rather than through `ProviderRegistry`, which
            // would resolve the active account instead of this one.
            Ok(Arc::new(AnthropicProvider::from_config(
                crate::client::ClientConfig {
                    api_key: credential,
                    api_base: config.resolve_anthropic_api_base(),
                    use_bearer_auth,
                    ..Default::default()
                },
            )))
        }
        claurst_core::accounts::PROVIDER_CODEX => CodexProvider::from_profile(profile_id)
            .map(|provider| Arc::new(provider) as Arc<dyn LlmProvider>)
            .ok_or_else(missing),
        // `provider_supports_profiles` already rejected everything else.
        _ => Err(ProviderResolveError::ProfilesUnsupported {
            provider_id: provider_id.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers;

    #[test]
    fn local_provider_aliases_resolve_to_canonical_registrations() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(providers::lm_studio()));
        registry.register(Arc::new(providers::llama_cpp()));

        let lm_studio = registry
            .get(&ProviderId::new("lmstudio"))
            .expect("lmstudio alias should resolve");
        let llama_cpp = registry
            .get(&ProviderId::new("llamacpp"))
            .expect("llamacpp alias should resolve");

        assert_eq!(&**lm_studio.id(), ProviderId::LM_STUDIO);
        assert_eq!(&**llama_cpp.id(), ProviderId::LLAMA_CPP);
    }

    #[test]
    fn alias_can_select_canonical_default_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(providers::lm_studio()));
        registry.set_default(ProviderId::new("lmstudio"));

        assert_eq!(&**registry.default_provider_id(), ProviderId::LM_STUDIO);
    }
}

#[cfg(test)]
mod profile_resolution_tests {
    //! Naming an account must fail with a reason the caller can act on, and it
    //! must never fall back to the active account.
    use super::*;
    use claurst_core::accounts::{AccountProfile, AccountRegistry, PROVIDER_ANTHROPIC};
    // Held across awaits, so it has to be the async-aware lock.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        saved: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let saved = std::env::var_os("CLAURST_HOME");
            std::env::set_var("CLAURST_HOME", dir.path());
            Self { saved, _dir: dir }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("CLAURST_HOME", value),
                None => std::env::remove_var("CLAURST_HOME"),
            }
        }
    }

    fn register(id: &str) {
        let mut registry = AccountRegistry::load();
        registry
            .upsert(
                PROVIDER_ANTHROPIC,
                AccountProfile {
                    id: id.to_string(),
                    ..Default::default()
                },
                false,
            )
            .expect("upsert");
    }

    #[tokio::test]
    async fn a_provider_without_accounts_rejects_a_profile() {
        let _lock = ENV_LOCK.lock().await;
        let _home = HomeGuard::new();
        let config = claurst_core::config::Config::default();

        let error = provider_by_id_for_profile(&config, "openai", Some("personal"))
            .await
            .err()
            .expect("openai stores one credential");

        assert_eq!(
            error,
            ProviderResolveError::ProfilesUnsupported {
                provider_id: "openai".to_string()
            }
        );
    }

    #[tokio::test]
    async fn an_unknown_account_lists_the_stored_ones() {
        let _lock = ENV_LOCK.lock().await;
        let _home = HomeGuard::new();
        register("work");
        register("personal");
        let config = claurst_core::config::Config::default();

        let error = provider_by_id_for_profile(&config, PROVIDER_ANTHROPIC, Some("missing"))
            .await
            .err()
            .expect("no such account");

        match error {
            ProviderResolveError::ProfileNotFound { available, .. } => {
                assert_eq!(available, vec!["personal".to_string(), "work".to_string()]);
            }
            other => panic!("expected ProfileNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_registered_account_without_tokens_is_reported_as_such() {
        let _lock = ENV_LOCK.lock().await;
        let _home = HomeGuard::new();
        // Registered in the registry, but no oauth_tokens.json was written.
        register("work");
        let config = claurst_core::config::Config::default();

        let error = provider_by_id_for_profile(&config, PROVIDER_ANTHROPIC, Some("work"))
            .await
            .err()
            .expect("no credentials on disk");

        assert_eq!(
            error,
            ProviderResolveError::ProfileCredentialsMissing {
                provider_id: PROVIDER_ANTHROPIC.to_string(),
                profile_id: "work".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn an_account_resolves_to_a_provider_carrying_its_own_credential() {
        let _lock = ENV_LOCK.lock().await;
        let _home = HomeGuard::new();
        register("personal");
        claurst_core::oauth::OAuthTokens {
            access_token: "personal-token".to_string(),
            scopes: vec![claurst_core::oauth::CLAUDE_AI_INFERENCE_SCOPE.to_string()],
            ..Default::default()
        }
        .persist(Some("personal"))
        .await
        .expect("persist");
        let config = claurst_core::config::Config::default();

        let provider = provider_by_id_for_profile(&config, PROVIDER_ANTHROPIC, Some("personal"))
            .await
            .expect("resolves");

        assert_eq!(provider.id(), &ProviderId::new(PROVIDER_ANTHROPIC));
    }

    #[tokio::test]
    async fn no_profile_keeps_the_existing_lookup() {
        let _lock = ENV_LOCK.lock().await;
        let _home = HomeGuard::new();
        let config = claurst_core::config::Config::default();

        // No credentials anywhere, so an unknown provider reports NotAvailable
        // rather than an account-shaped error.
        let error = provider_by_id_for_profile(&config, "not-a-provider", None)
            .await
            .err()
            .expect("unknown provider");

        assert_eq!(
            error,
            ProviderResolveError::NotAvailable {
                provider_id: "not-a-provider".to_string()
            }
        );
    }
}

#[cfg(test)]
mod custom_anthropic_tests {
    //! `custom-anthropic` is a second Anthropic-format endpoint. It has to sit
    //! next to the real provider rather than replace it, which is what
    //! overriding `providers.anthropic.api_base` would do.
    use super::*;
    use std::sync::Mutex as StdMutex;

    static ENV_LOCK: StdMutex<()> = StdMutex::new(());

    struct HomeGuard {
        saved_home: Option<std::ffi::OsString>,
        saved_base: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let saved_home = std::env::var_os("CLAURST_HOME");
            let saved_base = std::env::var_os("CUSTOM_ANTHROPIC_BASE_URL");
            std::env::set_var("CLAURST_HOME", dir.path());
            std::env::remove_var("CUSTOM_ANTHROPIC_BASE_URL");
            Self {
                saved_home,
                saved_base,
                _dir: dir,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved_home {
                Some(value) => std::env::set_var("CLAURST_HOME", value),
                None => std::env::remove_var("CLAURST_HOME"),
            }
            match &self.saved_base {
                Some(value) => std::env::set_var("CUSTOM_ANTHROPIC_BASE_URL", value),
                None => std::env::remove_var("CUSTOM_ANTHROPIC_BASE_URL"),
            }
        }
    }

    fn write_settings(body: &str) {
        let path = claurst_core::config::Settings::global_settings_path();
        std::fs::create_dir_all(path.parent().expect("settings dir")).expect("mkdir");
        std::fs::write(&path, body).expect("write settings");
    }

    #[test]
    fn without_a_base_url_there_is_nothing_to_register() {
        let _lock = ENV_LOCK.lock().expect("lock");
        let _home = HomeGuard::new();

        assert!(
            custom_anthropic_provider().is_none(),
            "falling back to the real Anthropic endpoint would be a confusing duplicate"
        );
    }

    #[test]
    fn a_base_url_in_settings_builds_the_provider() {
        let _lock = ENV_LOCK.lock().expect("lock");
        let _home = HomeGuard::new();
        write_settings(
            r#"{"providers":{"custom-anthropic":{"api_base":"https://gateway.example/v1"}}}"#,
        );

        let provider = custom_anthropic_provider().expect("configured");

        assert_eq!(
            provider.id(),
            &ProviderId::new(ProviderId::CUSTOM_ANTHROPIC),
            "registering under the anthropic id would replace the real provider"
        );
    }

    #[test]
    fn an_env_base_url_is_accepted_without_settings() {
        let _lock = ENV_LOCK.lock().expect("lock");
        let _home = HomeGuard::new();
        std::env::set_var("CUSTOM_ANTHROPIC_BASE_URL", "https://gateway.example/v1");

        assert!(custom_anthropic_provider().is_some());
    }

    #[test]
    fn a_disabled_entry_is_skipped() {
        let _lock = ENV_LOCK.lock().expect("lock");
        let _home = HomeGuard::new();
        write_settings(
            r#"{"providers":{"custom-anthropic":{"api_base":"https://gateway.example/v1","enabled":false}}}"#,
        );

        assert!(custom_anthropic_provider().is_none());
    }

    #[test]
    fn it_registers_alongside_the_real_anthropic_provider() {
        let _lock = ENV_LOCK.lock().expect("lock");
        let _home = HomeGuard::new();
        write_settings(
            r#"{"providers":{"custom-anthropic":{"api_base":"https://gateway.example/v1"}}}"#,
        );

        let mut registry = ProviderRegistry::with_anthropic(ClientConfig::default());
        registry.with_custom_anthropic_if_configured();

        assert!(registry
            .get(&ProviderId::new(ProviderId::ANTHROPIC))
            .is_some());
        assert!(registry
            .get(&ProviderId::new(ProviderId::CUSTOM_ANTHROPIC))
            .is_some());
    }
}
