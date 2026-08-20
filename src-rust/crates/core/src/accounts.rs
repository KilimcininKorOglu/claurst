//! Naming and identity helpers for accounts.
//!
//! An account is a `providers` entry in `settings.json` plus a credential in
//! `auth.json`, both keyed by the account's name. This module only decides
//! what that name may be and who it belongs to; the two stores own the data.

use std::path::PathBuf;

/// Slugify an arbitrary string into a safe account name. Lowercases, replaces
/// non-`[a-z0-9_-]` with `-`, trims dashes/underscores from edges, falls back
/// to "account" if the result is empty.
pub fn slugify_profile_id(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let mapped: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped
        .trim_matches(|c: char| c == '-' || c == '_')
        .to_string();
    if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed
    }
}

/// Slugify `base` and suffix it with -2, -3, … until `taken` says it is free.
pub fn unique_account_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = slugify_profile_id(base);
    if !taken(&base) {
        return base;
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{}-{}", base, n);
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The canonical mikmik home directory.
pub fn mikmik_dir() -> PathBuf {
    crate::config::Settings::config_dir()
}

/// Tighten permissions on a credential/session file so only the owner can
/// read or write it (mode `0o600`). Best-effort and Unix-only; a no-op on
/// other platforms (Windows ACLs are out of scope). Shared across modules
/// that persist tokens, credentials, or session transcripts (issue #212).
#[allow(unused_variables)]
pub(crate) fn set_user_only_perms(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

/// Tighten permissions on a directory that holds credentials/session data so
/// only the owner can traverse or list it (mode `0o700`). Best-effort and
/// Unix-only; a no-op on other platforms (issue #212).
#[allow(unused_variables)]
pub(crate) fn set_user_only_dir_perms(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
}

// ---------------------------------------------------------------------------
// JWT identity extraction
// ---------------------------------------------------------------------------

/// Identity fields extracted from an OpenAI/Codex id_token or access_token.
#[derive(Debug, Clone, Default)]
pub struct JwtIdentity {
    pub email: Option<String>,
    pub account_id: Option<String>,
}

/// Decode the payload of a JWT (`header.payload.signature`) and pull out the
/// fields we care about for naming an account. Tolerates malformed input by
/// returning an empty identity.
pub fn jwt_identity(token: &str) -> JwtIdentity {
    use base64::Engine;

    let mut out = JwtIdentity::default();
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    let Some(payload_b64) = parts.get(1) else {
        return out;
    };

    // JWT payloads are base64url-encoded without padding.
    let mut padded = (*payload_b64).to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = match base64::engine::general_purpose::URL_SAFE.decode(padded.as_bytes()) {
        Ok(b) => b,
        Err(_) => return out,
    };
    let json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return out,
    };

    // Direct email claim wins; otherwise look at the OpenAI custom profile claim.
    if let Some(email) = json.get("email").and_then(|v| v.as_str()) {
        out.email = Some(email.to_string());
    } else if let Some(profile) = json
        .get("https://api.openai.com/profile")
        .and_then(|v| v.as_object())
    {
        if let Some(email) = profile.get("email").and_then(|v| v.as_str()) {
            out.email = Some(email.to_string());
        }
    }

    // OpenAI puts account_id under the custom auth claim.
    if let Some(auth) = json
        .get("https://api.openai.com/auth")
        .and_then(|v| v.as_object())
    {
        if let Some(id) = auth.get("account_id").and_then(|v| v.as_str()) {
            out.account_id = Some(id.to_string());
        }
    }

    out
}

/// Derive a short, human-friendly account name from a JWT identity. Falls back
/// to "account" if nothing useful is in the token.
pub fn id_from_identity(identity: &JwtIdentity) -> String {
    if let Some(email) = &identity.email {
        // Use the local-part of the email (before @) as the slug source.
        let local = email.split('@').next().unwrap_or(email);
        return slugify_profile_id(local);
    }
    if let Some(account_id) = &identity.account_id {
        return slugify_profile_id(account_id);
    }
    "account".to_string()
}

/// A string that identifies this user the same way on every run.
///
/// Callers that derive something durable from "who is this" need an answer
/// that survives a restart. The stored account id is preferred because it
/// follows the user to another machine; when no account is stored, the
/// machine itself stands in, hashed so the hostname and home path are not
/// carried around in the open.
///
/// This is not a security boundary and must not be used as one. It is an
/// identifier, not a secret: anyone on the machine can reproduce it.
pub fn stable_identity() -> String {
    active_account_identity().unwrap_or_else(machine_identity)
}

/// The account half of [`stable_identity`].
///
/// Only an OAuth account answers: an API key names no person, so two machines
/// sharing one key would otherwise report the same identity.
fn active_account_identity() -> Option<String> {
    let settings = crate::config::Settings::load_sync().ok()?;
    let active = settings.provider.clone()?;
    let store = crate::AuthStore::load();
    for protocol in [
        crate::provider_id::ProviderId::ANTHROPIC,
        crate::provider_id::ProviderId::CODEX,
    ] {
        if store.accounts_for_protocol(protocol).contains(&active) {
            return Some(format!("{protocol}:{active}"));
        }
    }
    None
}

/// The machine half of [`stable_identity`], used when no account is stored.
fn machine_identity() -> String {
    let mut input = String::with_capacity(128);
    if let Ok(host) = hostname::get() {
        input.push_str(&host.to_string_lossy());
    }
    input.push(':');
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        input.push_str(&user);
    }
    input.push(':');
    if let Some(home) = dirs::home_dir() {
        input.push_str(&home.display().to_string());
    }

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("machine:{}", hex::encode(hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_strips_punctuation_and_lowercases() {
        assert_eq!(slugify_profile_id("Work Account!"), "work-account");
        assert_eq!(slugify_profile_id("  --weird-- "), "weird");
        assert_eq!(slugify_profile_id(""), "account");
        assert_eq!(slugify_profile_id("kuber@example.com"), "kuber-example-com");
    }

    #[test]
    fn unique_account_name_appends_a_suffix() {
        let taken = |candidate: &str| candidate == "work";
        assert_eq!(unique_account_name("work", taken), "work-2");
        assert_eq!(unique_account_name("personal", taken), "personal");
        assert_eq!(
            unique_account_name("Work Account", taken),
            "work-account",
            "the name is slugified before the collision check"
        );
    }

    #[test]
    fn jwt_identity_is_lenient_to_garbage() {
        let identity = jwt_identity("not.a.jwt");
        assert!(identity.email.is_none());
        assert!(identity.account_id.is_none());

        let empty = jwt_identity("");
        assert!(empty.email.is_none());
    }

    #[test]
    fn jwt_identity_pulls_email_and_account_id() {
        use base64::Engine;
        let payload = serde_json::json!({
            "email": "kuber@example.com",
            "https://api.openai.com/auth": {
                "account_id": "acc_abc123"
            }
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_string(&payload).unwrap());
        let token = format!("header.{}.signature", payload_b64);
        let identity = jwt_identity(&token);
        assert_eq!(identity.email.as_deref(), Some("kuber@example.com"));
        assert_eq!(identity.account_id.as_deref(), Some("acc_abc123"));

        assert_eq!(id_from_identity(&identity), "kuber");
    }

    // -----------------------------------------------------------------------
    // Issue #212: credential/session files must be owner-only (0o600) and
    // their parent dirs owner-only (0o700). Unix-only; a no-op elsewhere.
    // -----------------------------------------------------------------------

    /// The shared helper actually tightens a permissive file down to 0o600.
    #[cfg(unix)]
    #[test]
    fn set_user_only_perms_forces_file_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oauth_tokens.json");
        std::fs::write(&path, "{\"access_token\":\"secret\"}").unwrap();
        // Start deliberately world/group readable to prove we lock it down.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        set_user_only_perms(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be owner-only");
    }

    /// The dir helper tightens a permissive directory down to 0o700.
    #[cfg(unix)]
    #[test]
    fn set_user_only_dir_perms_forces_dir_to_0700() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("accounts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        set_user_only_dir_perms(&dir);

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "account dir must be owner-only");
    }

    /// End-to-end: the real codex token save path lands in an owner-only
    /// `auth.json` under an owner-only config dir. Redirects the config root
    /// via a temp `MIKMIK_HOME`, serialized against any other env-mutating
    /// test in this binary.
    #[cfg(unix)]
    #[test]
    fn real_codex_save_path_writes_an_0600_auth_store() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Mutex;
        static HOME_LOCK: Mutex<()> = Mutex::new(());
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("MIKMIK_HOME");
        std::env::set_var("MIKMIK_HOME", tmp.path());

        let tokens = crate::oauth_config::CodexTokens {
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            account_id: Some("acc_1".into()),
            expires_at: Some(0),
        };
        let save_res = crate::oauth_config::save_codex_tokens_for_account(&tokens, "work");

        let path = crate::AuthStore::path();
        let file_mode = std::fs::metadata(&path).map(|m| m.permissions().mode() & 0o777);
        let dir_mode =
            std::fs::metadata(path.parent().unwrap()).map(|m| m.permissions().mode() & 0o777);
        let stored = crate::AuthStore::load()
            .codex_tokens("work")
            .map(|t| t.access_token.clone());

        // Restore the config root before asserting so a failure can't leak the
        // override into the rest of the test binary.
        match prev_home {
            Some(v) => std::env::set_var("MIKMIK_HOME", v),
            None => std::env::remove_var("MIKMIK_HOME"),
        }

        save_res.unwrap();
        assert_eq!(stored.as_deref(), Some("access-secret"));
        assert_eq!(file_mode.unwrap(), 0o600, "auth store must be owner-only");
        assert_eq!(dir_mode.unwrap(), 0o700, "config dir must be owner-only");
    }
    #[test]
    fn the_same_machine_answers_with_the_same_identity() {
        assert_eq!(machine_identity(), machine_identity());
        assert!(machine_identity().starts_with("machine:"));
    }

    #[test]
    fn an_api_key_account_falls_through_to_the_machine() {
        // Nothing is stored under this config root, so there is no OAuth
        // account to name and the machine has to answer.
        assert!(stable_identity().starts_with("machine:"));
    }
}
