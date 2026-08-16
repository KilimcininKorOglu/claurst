// auth_store.rs — JSON-based credential store at ~/.claurst/auth.json.
//
// Stores API keys and OAuth tokens for providers so users don't have to rely
// solely on environment variables.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A stored credential for an account.
///
/// Every credential the product holds lives here, whatever its shape, so an
/// account is one entry in one file rather than a registry entry plus a token
/// file of its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StoredCredential {
    #[serde(rename = "api")]
    ApiKey { key: String },
    /// GitHub Copilot's device-flow token.
    #[serde(rename = "oauth")]
    OAuthToken {
        access: String,
        refresh: String,
        expires: u64,
    },
    /// Anthropic's OAuth tokens, from either the claude.ai or the console flow.
    ///
    /// Carries its own fields rather than collapsing into `OAuthToken`, because
    /// the scope list decides whether the credential is a Bearer token or a
    /// minted API key, and the identity fields name the account.
    #[serde(rename = "anthropic-oauth")]
    AnthropicOAuth(crate::oauth::OAuthTokens),
    /// OpenAI Codex OAuth tokens.
    #[serde(rename = "codex-oauth")]
    CodexOAuth(crate::oauth_config::CodexTokens),
}

/// Persistent credential store backed by `~/.claurst/auth.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AuthStore {
    pub credentials: HashMap<String, StoredCredential>,
}

impl AuthStore {
    /// Path to the auth store file.
    pub fn path() -> PathBuf {
        crate::config::Settings::config_dir().join("auth.json")
    }

    /// Load the store from disk (returns default if missing or invalid).
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(s) => match serde_json::from_str(&s) {
                    Ok(store) => store,
                    Err(e) => {
                        tracing::warn!(
                            "auth store at {} is corrupt ({}); starting with an empty store. \
                             The corrupt file is left in place until the next save.",
                            path.display(),
                            e
                        );
                        Self::default()
                    }
                },
                Err(e) => {
                    tracing::warn!("failed to read auth store at {}: {}", path.display(), e);
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }

    /// Persist the store to disk (best-effort).
    ///
    /// Writes to a temp file then renames over the destination so a crash or
    /// disk-full mid-write can never truncate `auth.json` (which would
    /// silently wipe the user's stored credentials on the next load).
    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            crate::accounts::set_user_only_dir_perms(parent);
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(j) => j,
            Err(_) => return,
        };
        let tmp = path.with_file_name(format!(".auth.json.claurst-tmp-{}", std::process::id()));
        if std::fs::write(&tmp, &json).is_ok() {
            // auth.json holds API keys + OAuth tokens. Lock the temp file to
            // 0o600 *before* the rename so the live credential file is never
            // even momentarily world/group readable (issue #212).
            crate::accounts::set_user_only_perms(&tmp);
            if std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Store a credential for the given provider (persists immediately).
    pub fn set(&mut self, provider_id: &str, cred: StoredCredential) {
        self.credentials.insert(provider_id.to_string(), cred);
        self.save();
    }

    /// Get the stored credential for a provider.
    pub fn get(&self, provider_id: &str) -> Option<&StoredCredential> {
        self.credentials.get(provider_id)
    }

    /// Remove the credential for a provider (persists immediately).
    pub fn remove(&mut self, provider_id: &str) {
        self.credentials.remove(provider_id);
        self.save();
    }

    /// The Anthropic OAuth tokens stored for `account_id`, if that is what the
    /// account holds.
    pub fn anthropic_tokens(&self, account_id: &str) -> Option<&crate::oauth::OAuthTokens> {
        match self.get(account_id) {
            Some(StoredCredential::AnthropicOAuth(tokens)) => Some(tokens),
            _ => None,
        }
    }

    /// The Codex OAuth tokens stored for `account_id`, if that is what the
    /// account holds.
    pub fn codex_tokens(&self, account_id: &str) -> Option<&crate::oauth_config::CodexTokens> {
        match self.get(account_id) {
            Some(StoredCredential::CodexOAuth(tokens)) => Some(tokens),
            _ => None,
        }
    }

    /// Store Anthropic OAuth tokens for `account_id` (persists immediately).
    pub fn set_anthropic_tokens(&mut self, account_id: &str, tokens: crate::oauth::OAuthTokens) {
        self.set(account_id, StoredCredential::AnthropicOAuth(tokens));
    }

    /// Store Codex OAuth tokens for `account_id` (persists immediately).
    pub fn set_codex_tokens(&mut self, account_id: &str, tokens: crate::oauth_config::CodexTokens) {
        self.set(account_id, StoredCredential::CodexOAuth(tokens));
    }

    /// Every account holding a credential of `protocol`.
    ///
    /// Reads the credential's own shape rather than a separate registry, so an
    /// account cannot be listed without a credential or hold one without being
    /// listed.
    pub fn accounts_for_protocol(&self, protocol: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .credentials
            .iter()
            .filter(|(_, cred)| match (protocol, cred) {
                ("anthropic", StoredCredential::AnthropicOAuth(_)) => true,
                ("codex", StoredCredential::CodexOAuth(_)) => true,
                ("github-copilot", StoredCredential::OAuthToken { .. }) => true,
                _ => false,
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Move every plaintext `providers.<account>.api_key` out of
    /// `settings.json` and into this store.
    ///
    /// `settings.json` is written with the default file mode and holds no
    /// other secret, while `auth.json` is written `0o600`, so a key left in
    /// settings is readable by every other account on the machine. Returns the
    /// accounts that were moved, so a caller can tell the user where the key
    /// went.
    ///
    /// Runs at startup, which also means a key written into `settings.json` by
    /// hand is relocated on the next launch rather than staying in the clear.
    pub fn migrate_plaintext_provider_keys() -> Vec<String> {
        let Ok(mut settings) = crate::config::Settings::load_sync() else {
            return Vec::new();
        };

        let mut store = Self::load();
        let mut moved = Vec::new();
        for (account_id, provider) in settings.providers.iter_mut() {
            let Some(key) = provider.api_key.take().filter(|key| !key.is_empty()) else {
                continue;
            };
            // A credential already in the store is the newer one, because
            // nothing writes to `settings.json` any more. Drop the stale copy
            // rather than restoring it over the live credential.
            if store.get(account_id).is_none() {
                store
                    .credentials
                    .insert(account_id.clone(), StoredCredential::ApiKey { key });
            }
            moved.push(account_id.clone());
        }

        if moved.is_empty() {
            return moved;
        }

        store.save();
        if let Err(e) = settings.save_sync() {
            // The key now lives in both files. Say so instead of reporting a
            // move that only half happened.
            tracing::warn!(
                "moved {} plaintext provider key(s) into the auth store, but could not \
                 rewrite settings.json ({}); the plaintext copy is still there",
                moved.len(),
                e
            );
        }
        moved
    }

    /// Get the API key for a provider, checking stored credentials first then
    /// falling back to the relevant environment variable.
    pub fn api_key_for(&self, provider_id: &str) -> Option<String> {
        self.api_key_for_protocol(provider_id, provider_id)
    }

    /// Get the API key stored under `account_id`, reading it as a credential of
    /// `protocol`.
    ///
    /// The two differ whenever the user named the account: the credential is
    /// filed under the name they chose, while how to read it and which env var
    /// stands in for it are properties of the wire format it speaks. Passing
    /// the account name as both is what [`api_key_for`](Self::api_key_for)
    /// does, which is right for an account named after its vendor.
    pub fn api_key_for_protocol(&self, account_id: &str, protocol: &str) -> Option<String> {
        // Check stored credentials first
        if let Some(stored) = self.get(account_id) {
            match stored {
                StoredCredential::ApiKey { key } => {
                    if !key.is_empty() {
                        return Some(key.clone());
                    }
                }
                StoredCredential::OAuthToken {
                    access, refresh, ..
                } if protocol == "github-copilot" => {
                    if !refresh.is_empty() {
                        return Some(refresh.clone());
                    }
                    if !access.is_empty() {
                        return Some(access.clone());
                    }
                }
                // The claude.ai flow presents the access token as a Bearer and
                // the console flow presents the API key it minted, so the
                // credential to hand out is whichever the scopes call for.
                //
                // Expiry is not checked here: this is a synchronous read and
                // refreshing needs the network. The caller that can await goes
                // through `oauth::resolve_auth_for_account`.
                StoredCredential::AnthropicOAuth(tokens) => {
                    if let Some(credential) = tokens.effective_credential() {
                        return Some(credential.to_string());
                    }
                }
                StoredCredential::CodexOAuth(tokens) => {
                    if !tokens.access_token.is_empty() {
                        return Some(tokens.access_token.clone());
                    }
                }
                _ => {}
            }
        }
        // Fall back to environment variable.
        //
        // These mappings must match the env var each provider's adapter
        // actually reads in `crates/api/src/providers/openai_compat_providers.rs`
        // (and the bespoke adapters next to it). When they drift, keys that
        // were exported via env vars look "configured" to the dialog but
        // resolve to empty at request time. If you add a provider there,
        // mirror its env var here.
        let env_var = match protocol {
            "anthropic" => "ANTHROPIC_API_KEY",
            "openai" => "OPENAI_API_KEY",
            "google" => "GOOGLE_API_KEY",
            "groq" => "GROQ_API_KEY",
            "cerebras" => "CEREBRAS_API_KEY",
            "deepseek" => "DEEPSEEK_API_KEY",
            "mistral" => "MISTRAL_API_KEY",
            "xai" => "XAI_API_KEY",
            "openrouter" => "OPENROUTER_API_KEY",
            "togetherai" | "together-ai" => "TOGETHER_API_KEY",
            "perplexity" => "PERPLEXITY_API_KEY",
            "cohere" => "COHERE_API_KEY",
            "deepinfra" => "DEEPINFRA_API_KEY",
            "venice" => "VENICE_API_KEY",
            "github-copilot" => "GITHUB_TOKEN",
            "azure" => "AZURE_API_KEY",
            "huggingface" => "HF_TOKEN",
            "nvidia" => "NVIDIA_API_KEY",
            "zai" => "ZAI_API_KEY",
            "opencode-zen" | "opencode-go" => "OPENCODE_API_KEY",
            "crof" => "CROF_API_KEY",
            "sambanova" => "SAMBANOVA_API_KEY",
            // qwen adapter reads DASHSCOPE_API_KEY (Alibaba's DashScope is the
            // backing service), not QWEN_API_KEY.
            "qwen" | "alibaba" => "DASHSCOPE_API_KEY",
            "moonshot" | "moonshotai" => "MOONSHOT_API_KEY",
            "zhipu" | "zhipuai" => "ZHIPU_API_KEY",
            "siliconflow" => "SILICONFLOW_API_KEY",
            "nebius" => "NEBIUS_API_KEY",
            "novita" => "NOVITA_API_KEY",
            "ovhcloud" => "OVHCLOUD_API_KEY",
            "scaleway" => "SCALEWAY_API_KEY",
            "vultr" | "vultr-ai" => "VULTR_API_KEY",
            "baseten" => "BASETEN_API_KEY",
            // friendli adapter reads FRIENDLI_TOKEN (Friendli's docs use that
            // name), not FRIENDLI_API_KEY.
            "friendli" => "FRIENDLI_TOKEN",
            "upstage" => "UPSTAGE_API_KEY",
            "stepfun" => "STEPFUN_API_KEY",
            "fireworks" => "FIREWORKS_API_KEY",
            "minimax" => "MINIMAX_API_KEY",
            "synthetic" => "SYNTHETIC_API_KEY",
            "routing" => "ROUTING_API_KEY",
            "neuralwatt" => "NEURALWATT_API_KEY",
            "custom-openai" => "CUSTOM_OPENAI_API_KEY",
            "custom-anthropic" => "CUSTOM_ANTHROPIC_API_KEY",
            "ollama" | "lm-studio" | "llama-cpp" => "", // No API key required
            _ => return None,
        };
        if env_var.is_empty() {
            None
        } else {
            std::env::var(env_var).ok().filter(|k| !k.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthStore, StoredCredential};

    #[test]
    fn github_copilot_oauth_prefers_refresh_token() {
        let mut store = AuthStore::default();
        store.credentials.insert(
            "github-copilot".to_string(),
            StoredCredential::OAuthToken {
                access: "access-token".to_string(),
                refresh: "refresh-token".to_string(),
                expires: 0,
            },
        );

        assert_eq!(
            store.api_key_for("github-copilot").as_deref(),
            Some("refresh-token")
        );
    }

    fn anthropic_tokens(scopes: &[&str], api_key: Option<&str>) -> crate::oauth::OAuthTokens {
        crate::oauth::OAuthTokens {
            access_token: "access-token".to_string(),
            refresh_token: Some("refresh-token".to_string()),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            email: Some("work@example.com".to_string()),
            api_key: api_key.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn every_credential_shape_survives_a_round_trip() {
        // The store is now the only place a credential lives, so a field lost
        // in serialisation is a credential the account can never present.
        let mut store = AuthStore::default();
        store.credentials.insert(
            "gateway".to_string(),
            StoredCredential::ApiKey { key: "sk-1".into() },
        );
        store.credentials.insert(
            "kerem".to_string(),
            StoredCredential::OAuthToken {
                access: "gho-a".into(),
                refresh: "gho-r".into(),
                expires: 7,
            },
        );
        store.credentials.insert(
            "work".to_string(),
            StoredCredential::AnthropicOAuth(anthropic_tokens(&["user:inference"], None)),
        );
        store.credentials.insert(
            "chatgpt".to_string(),
            StoredCredential::CodexOAuth(crate::oauth_config::CodexTokens {
                access_token: "codex-a".into(),
                refresh_token: Some("codex-r".into()),
                account_id: Some("acct-1".into()),
                expires_at: Some(99),
            }),
        );

        let json = serde_json::to_string(&store).expect("serialise");
        let back: AuthStore = serde_json::from_str(&json).expect("deserialise");

        assert_eq!(back.api_key_for("gateway").as_deref(), Some("sk-1"));
        let tokens = back.anthropic_tokens("work").expect("anthropic account");
        assert_eq!(tokens.email.as_deref(), Some("work@example.com"));
        assert_eq!(tokens.scopes, vec!["user:inference".to_string()]);
        let codex = back.codex_tokens("chatgpt").expect("codex account");
        assert_eq!(codex.refresh_token.as_deref(), Some("codex-r"));
        assert_eq!(codex.expires_at, Some(99));
    }

    #[test]
    fn an_anthropic_account_presents_what_its_scopes_call_for() {
        let mut store = AuthStore::default();
        store.credentials.insert(
            "subscription".to_string(),
            StoredCredential::AnthropicOAuth(anthropic_tokens(&["user:inference"], None)),
        );
        store.credentials.insert(
            "console".to_string(),
            StoredCredential::AnthropicOAuth(anthropic_tokens(
                &["org:create_api_key"],
                Some("sk-ant-minted"),
            )),
        );

        assert_eq!(
            store
                .api_key_for_protocol("subscription", "anthropic")
                .as_deref(),
            Some("access-token"),
            "a claude.ai token is presented as the Bearer itself"
        );
        assert_eq!(
            store
                .api_key_for_protocol("console", "anthropic")
                .as_deref(),
            Some("sk-ant-minted"),
            "a console account presents the key it minted, not the access token"
        );
    }

    #[test]
    fn accounts_are_grouped_by_the_credential_they_hold() {
        let mut store = AuthStore::default();
        store.credentials.insert(
            "gateway".to_string(),
            StoredCredential::ApiKey { key: "sk-1".into() },
        );
        store.credentials.insert(
            "personal".to_string(),
            StoredCredential::AnthropicOAuth(anthropic_tokens(&["user:inference"], None)),
        );
        store.credentials.insert(
            "work".to_string(),
            StoredCredential::AnthropicOAuth(anthropic_tokens(&["user:inference"], None)),
        );

        assert_eq!(
            store.accounts_for_protocol("anthropic"),
            vec!["personal".to_string(), "work".to_string()]
        );
        assert!(store.accounts_for_protocol("codex").is_empty());
    }

    #[test]
    fn a_copilot_token_is_read_under_the_account_it_was_filed_under() {
        // A second Copilot login is stored under its GitHub name, so the OAuth
        // branch has to key off the protocol rather than the account name.
        let mut store = AuthStore::default();
        store.credentials.insert(
            "kerem".to_string(),
            StoredCredential::OAuthToken {
                access: "access-token".to_string(),
                refresh: "refresh-token".to_string(),
                expires: 0,
            },
        );

        assert_eq!(
            store
                .api_key_for_protocol("kerem", "github-copilot")
                .as_deref(),
            Some("refresh-token")
        );
        assert!(
            store.api_key_for("kerem").is_none(),
            "without the protocol there is nothing that says how to read it"
        );
    }

    #[test]
    fn api_key_for_regular_provider_uses_stored_key() {
        let mut store = AuthStore::default();
        store.credentials.insert(
            "openrouter".to_string(),
            StoredCredential::ApiKey {
                key: "or-key".to_string(),
            },
        );

        assert_eq!(store.api_key_for("openrouter").as_deref(), Some("or-key"));
    }
}
