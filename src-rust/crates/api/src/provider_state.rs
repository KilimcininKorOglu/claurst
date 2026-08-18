//! Throwing away what was saved about providers.
//!
//! `/refresh` starts over: the credentials, the cached tokens, the model
//! catalogs, and the account and model that were chosen. Only the discarding
//! lives here, because that part is the same wherever the command is run from;
//! rebuilding the live runtime afterwards belongs to whoever owns it.

use anyhow::Context;

/// Discard every saved credential, cached catalog, and remembered choice.
///
/// Leaves the settings file in place with its provider, model and key cleared,
/// so everything else the user configured survives.
pub async fn clear_saved_provider_state() -> anyhow::Result<()> {
    remove_file_if_exists(&claurst_core::AuthStore::path())
        .await
        .context("Failed to clear auth store")?;
    remove_file_if_exists(&claurst_core::oauth::OAuthTokens::token_file_path())
        .await
        .context("Failed to clear OAuth token cache")?;
    remove_file_if_exists(&crate::model_cache::models_cache_path())
        .await
        .context("Failed to clear model cache")?;
    remove_file_if_exists(&crate::model_cache::models_dev_cache_path())
        .await
        .context("Failed to clear legacy model cache")?;

    let mut settings = claurst_core::config::Settings::load()
        .await
        .context("Failed to load settings for /refresh")?;
    settings.provider = None;
    settings.config.provider = None;
    settings.config.model = None;
    settings.config.api_key = None;
    settings
        .save()
        .await
        .context("Failed to save refreshed settings")
}

async fn remove_file_if_exists(path: &std::path::Path) -> anyhow::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CLAURST_HOME` is process-wide, so the tests that move it run one at a
    /// time and put it back when they are done.
    static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn set() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let previous = std::env::var_os("CLAURST_HOME");
            unsafe { std::env::set_var("CLAURST_HOME", dir.path()) };
            Self {
                previous,
                _dir: dir,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var("CLAURST_HOME", v) },
                None => unsafe { std::env::remove_var("CLAURST_HOME") },
            }
        }
    }

    #[tokio::test]
    async fn the_chosen_account_and_model_are_forgotten_but_the_rest_is_kept() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::set();

        let settings = claurst_core::config::Settings {
            provider: Some("acme".to_string()),
            config: claurst_core::config::Config {
                provider: Some("acme".to_string()),
                model: Some("acme/fast".to_string()),
                api_key: Some("secret".to_string()),
                theme: claurst_core::config::Theme::Light,
                ..Default::default()
            },
            ..Default::default()
        };
        settings.save().await.expect("staged settings");

        clear_saved_provider_state()
            .await
            .expect("state can be cleared");

        let after = claurst_core::config::Settings::load()
            .await
            .expect("settings");
        assert_eq!(after.provider, None);
        assert_eq!(after.config.provider, None);
        assert_eq!(after.config.model, None);
        assert_eq!(after.config.api_key, None);
        // Everything the user configured that has nothing to do with the
        // account survives; /refresh is not a reset of the whole file.
        assert!(matches!(
            after.config.theme,
            claurst_core::config::Theme::Light
        ));
    }

    #[tokio::test]
    async fn clearing_state_that_was_never_written_is_not_an_error() {
        let _lock = HOME_LOCK.lock().await;
        let _home = HomeGuard::set();

        clear_saved_provider_state()
            .await
            .expect("a fresh home clears cleanly");
    }
}
