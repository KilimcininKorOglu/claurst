//! Recording what an account says it serves.
//!
//! Discovery asks a provider for its model list; this is where the answer is
//! written down and described. It lives here rather than in a front end
//! because every front end needs the same thing to happen, and a second copy
//! would be a second set of rules for the same file.

use mikmik_core::config::{Config, ModelOverride, Settings};

/// What a sync did, so the caller can say it rather than guess, and apply the
/// same change to the running configuration.
pub struct ModelSyncOutcome {
    pub discovered: usize,
    /// `modelOverrides` keys filled from the endpoint's own numbers.
    pub written: usize,
    /// Keys left alone because the user had already set them.
    pub kept: Vec<String>,
    /// The model ids now recorded for the account.
    pub models: Vec<String>,
    /// When the list was read.
    pub synced_at: String,
    /// The overrides that were written, ready to apply in memory.
    pub overrides: Vec<(String, ModelOverride)>,
}

/// Apply a completed sync to a live `Config`.
///
/// The settings file is not what the session reads: routing checks
/// `Config::provider_configs` and the picker reads `Config::model_overrides`,
/// so a sync that only reached disk leaves the running session refusing a
/// model the endpoint just confirmed it serves.
pub fn apply_model_sync(config: &mut Config, account_id: &str, outcome: &ModelSyncOutcome) {
    let entry = config
        .provider_configs
        .entry(account_id.to_string())
        .or_default();
    entry.models = outcome.models.clone();
    entry.models_synced_at = Some(outcome.synced_at.clone());
    for (key, value) in &outcome.overrides {
        config.model_overrides.insert(key.clone(), value.clone());
    }
}

/// Record the models an account was discovered to serve.
///
/// Written to disk rather than kept in memory so the list is visible and
/// editable: discovery seeds it, and the file is the source of truth from then
/// on.
///
/// The limits the endpoint reported go into `modelOverrides` under
/// `"<account>/<model>"`, which is the map the picker already consults. A key
/// the user has already written is never touched, because that map is
/// documented as user-supplied; `force` is the explicit way to take the
/// endpoint's numbers instead.
pub fn persist_account_models(
    account_id: &str,
    models: &[crate::ModelInfo],
    force: bool,
) -> Result<ModelSyncOutcome, String> {
    let mut settings = Settings::load_sync().unwrap_or_default();
    let ids: Vec<String> = models.iter().map(|model| model.id.to_string()).collect();
    let synced_at = chrono::Utc::now().to_rfc3339();

    let entry = settings
        .providers
        .entry(account_id.to_string())
        .or_default();
    entry.models = ids.clone();
    entry.models_synced_at = Some(synced_at.clone());

    let mut outcome = ModelSyncOutcome {
        discovered: models.len(),
        written: 0,
        kept: Vec::new(),
        models: ids,
        synced_at,
        overrides: Vec::new(),
    };

    for model in models {
        let key = format!("{account_id}/{}", model.id);
        if settings.model_overrides.contains_key(&key) && !force {
            outcome.kept.push(key);
            continue;
        }
        let value = ModelOverride {
            context_window: Some(model.context_window),
            max_output_tokens: Some(model.max_output_tokens),
            name: Some(model.name.clone()),
            ..Default::default()
        };
        settings.model_overrides.insert(key.clone(), value.clone());
        outcome.overrides.push((key, value));
        outcome.written += 1;
    }

    settings.save_sync().map_err(|err| err.to_string())?;
    Ok(outcome)
}

/// One line describing what a sync changed.
///
/// Names the kept overrides rather than counting them, because a user who
/// edited a context window wants to know their edit survived and which switch
/// replaces it.
pub fn describe_model_sync(account_id: &str, outcome: &ModelSyncOutcome) -> String {
    let plural = if outcome.discovered == 1 { "" } else { "s" };
    let mut line = format!(
        "{account_id}: {} model{plural} discovered",
        outcome.discovered
    );
    if outcome.written > 0 {
        line.push_str(&format!(", {} limit(s) recorded", outcome.written));
    }
    if !outcome.kept.is_empty() {
        line.push_str(&format!(
            ". Kept your own limits for {} (use /providers sync --force to replace them)",
            outcome.kept.join(", ")
        ));
    } else {
        line.push('.');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelInfo;
    use mikmik_core::provider_id::{ModelId, ProviderId};

    /// `CLAURST_HOME` is process-wide, so the tests that move it run one at a
    /// time and put it back when they are done.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    fn model(id: &str, context: u32) -> ModelInfo {
        ModelInfo {
            id: ModelId::new(id),
            provider_id: ProviderId::new("acme"),
            name: format!("Acme {id}"),
            context_window: context,
            max_output_tokens: 4096,
            ..Default::default()
        }
    }

    #[test]
    fn a_sync_records_what_the_account_serves() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let _home = HomeGuard::set();

        let outcome = persist_account_models("acme", &[model("fast", 8_000)], false)
            .expect("the settings file can be written");

        assert_eq!(outcome.discovered, 1);
        assert_eq!(outcome.models, vec!["fast".to_string()]);
        assert_eq!(outcome.written, 1);

        let settings = Settings::load_sync().expect("the file that was just written");
        assert_eq!(
            settings.providers.get("acme").map(|p| p.models.clone()),
            Some(vec!["fast".to_string()])
        );
        assert!(settings.model_overrides.contains_key("acme/fast"));
    }

    #[test]
    fn a_limit_the_user_wrote_survives_a_sync() {
        // `modelOverrides` is documented as user-supplied, so discovery must
        // not quietly replace a number somebody set by hand.
        let _lock = HOME_LOCK.lock().expect("home lock");
        let _home = HomeGuard::set();

        let mut settings = Settings::default();
        settings.model_overrides.insert(
            "acme/fast".to_string(),
            ModelOverride {
                context_window: Some(123),
                ..Default::default()
            },
        );
        settings.save_sync().expect("staged settings");

        let outcome = persist_account_models("acme", &[model("fast", 8_000)], false)
            .expect("the settings file can be written");

        assert_eq!(outcome.kept, vec!["acme/fast".to_string()]);
        assert_eq!(outcome.written, 0);
        let after = Settings::load_sync().expect("settings");
        assert_eq!(
            after.model_overrides["acme/fast"].context_window,
            Some(123),
            "the user's own limit was replaced"
        );
    }

    #[test]
    fn forcing_a_sync_takes_the_endpoints_numbers() {
        let _lock = HOME_LOCK.lock().expect("home lock");
        let _home = HomeGuard::set();

        let mut settings = Settings::default();
        settings.model_overrides.insert(
            "acme/fast".to_string(),
            ModelOverride {
                context_window: Some(123),
                ..Default::default()
            },
        );
        settings.save_sync().expect("staged settings");

        let outcome = persist_account_models("acme", &[model("fast", 8_000)], true)
            .expect("the settings file can be written");

        assert!(outcome.kept.is_empty());
        let after = Settings::load_sync().expect("settings");
        assert_eq!(
            after.model_overrides["acme/fast"].context_window,
            Some(8_000)
        );
    }

    #[test]
    fn a_sync_reaches_the_running_configuration_too() {
        // The session routes on `Config`, not on the file, so a sync that only
        // landed on disk would leave it refusing a model just confirmed.
        let outcome = ModelSyncOutcome {
            discovered: 1,
            written: 1,
            kept: Vec::new(),
            models: vec!["fast".to_string()],
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            overrides: vec![(
                "acme/fast".to_string(),
                ModelOverride {
                    context_window: Some(8_000),
                    ..Default::default()
                },
            )],
        };
        let mut config = Config::default();

        apply_model_sync(&mut config, "acme", &outcome);

        assert_eq!(config.provider_configs["acme"].models, vec!["fast"]);
        assert!(config.model_overrides.contains_key("acme/fast"));
    }

    #[test]
    fn the_summary_names_the_limits_it_left_alone() {
        // Counting them would tell the user something was kept without saying
        // which of their edits it was.
        let outcome = ModelSyncOutcome {
            discovered: 2,
            written: 1,
            kept: vec!["acme/fast".to_string()],
            models: vec!["fast".to_string(), "slow".to_string()],
            synced_at: String::new(),
            overrides: Vec::new(),
        };

        let line = describe_model_sync("acme", &outcome);

        assert!(line.contains("2 models discovered"), "{line}");
        assert!(line.contains("acme/fast"), "{line}");
        assert!(line.contains("--force"), "{line}");
    }
}
