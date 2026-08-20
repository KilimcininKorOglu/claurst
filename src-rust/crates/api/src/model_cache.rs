//! Where the models catalog is cached on disk, and how a `ModelRegistry` is
//! built from that cache.
//!
//! Every surface that lists models reads the same cache: the CLI's `models`
//! subcommand, the TUI's picker, and the ACP server's session configuration.

use std::path::PathBuf;
use std::sync::Arc;

use mikmik_core::config::Config;

use crate::ModelRegistry;

/// Directory holding the cached models catalog.
pub fn model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mikmik")
}

/// Resolve the models.dev source URL, honoring env-var overrides.
pub fn models_source_url() -> String {
    std::env::var("MIKMIK_MODELS_URL")
        .or_else(|_| std::env::var("MODELS_DEV_URL"))
        .unwrap_or_else(|_| "https://models.dev/api.json".to_string())
}

/// Default cache filename — derived from the source URL so a custom
/// `MIKMIK_MODELS_URL` doesn't stomp the canonical models.dev cache.
pub fn models_cache_path() -> PathBuf {
    let url = models_source_url();
    let filename = if url == "https://models.dev/api.json" {
        "models.json".to_string()
    } else {
        // Hash the source URL into the filename so two different mirrors
        // each get their own cache file.
        let h = xxhash_rust::xxh64::xxh64(url.as_bytes(), 0);
        format!("models-{:016x}.json", h)
    };
    model_cache_dir().join(filename)
}

/// Legacy cache file location — kept so old installs don't lose their
/// previously-fetched data on first run with the new layout.
pub fn models_dev_cache_path() -> PathBuf {
    model_cache_dir().join("models_dev.json")
}

/// Build a registry from whatever is already on disk, without touching the
/// network, and layer the user's metadata overrides on top.
pub fn load_cached_model_registry(config: &Config) -> Arc<ModelRegistry> {
    let mut reg = ModelRegistry::new();
    // MIKMIK_MODELS_PATH wins outright — useful for offline dev where you
    // pin a known-good api.json on disk.
    if let Ok(custom) = std::env::var("MIKMIK_MODELS_PATH") {
        reg.load_cache(&PathBuf::from(custom));
    } else {
        reg.load_cache(&models_cache_path());
        // Migration nicety: if the new cache file is missing but the old
        // one exists, ingest it once.
        if !models_cache_path().exists() {
            reg.load_cache(&models_dev_cache_path());
        }
    }
    // Layer user metadata overrides on top of the catalog (issue #309). Stored
    // in the registry, so any later cache reload re-asserts them automatically.
    reg.apply_model_overrides(&config.model_overrides);
    Arc::new(reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_mirrors_do_not_share_one_cache_file() {
        // The filename carries a hash of the source URL, so a user pointed at
        // a mirror cannot end up reading the canonical catalog's cache.
        let canonical = model_cache_dir().join("models.json");
        let hashed = {
            let h = xxhash_rust::xxh64::xxh64(b"https://example.invalid/api.json", 0);
            model_cache_dir().join(format!("models-{h:016x}.json"))
        };
        assert_ne!(canonical, hashed);
    }
}
