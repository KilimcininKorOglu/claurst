//! Canonical filesystem locations for mikmik.
//!
//! Everything mikmik persists lives under a single root directory. This module
//! exposes the one resolver ([`mikmik_home`]) that the whole workspace routes
//! through, so the home-dir precedence (see [`crate::config::Settings::config_dir`])
//! is defined in exactly one place.

use std::path::PathBuf;

/// The canonical mikmik home directory — the single source of truth for where
/// mikmik keeps its data. Thin wrapper over
/// [`crate::config::Settings::config_dir`]; prefer this at call sites that only
/// need the root path.
///
/// Resolution precedence (issue #207 — XDG support):
/// 1. `$MIKMIK_HOME` if set and non-empty (verbatim).
/// 2. `$XDG_CONFIG_HOME/mikmik` (when absolute) else `~/.config/mikmik`.
pub fn mikmik_home() -> PathBuf {
    crate::config::Settings::config_dir()
}

// These tests drive the resolver through `HOME`/`XDG_CONFIG_HOME`, which only
// govern `dirs::home_dir()` on Unix — on Windows the home dir comes from the OS
// profile API and can't be pinned via env, so they'd be non-hermetic there.
#[cfg(all(test, unix))]
mod tests {
    use crate::config::Settings;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // The resolver reads process-global env (`MIKMIK_HOME`, `HOME`,
    // `XDG_CONFIG_HOME`). Serialize every test that mutates them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let keys = ["MIKMIK_HOME", "HOME", "XDG_CONFIG_HOME"];
            let saved = keys
                .iter()
                .map(|k| (*k, std::env::var_os(k)))
                .collect::<Vec<_>>();
            for k in keys {
                std::env::remove_var(k);
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn mikmik_home_env_override_wins() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let tmp = tempfile::tempdir().unwrap();
        // Set HOME and XDG too, to prove the override takes precedence over
        // every other rule and is used verbatim.
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path());
        std::env::set_var("MIKMIK_HOME", tmp.path());

        assert_eq!(Settings::config_dir(), tmp.path());
    }

    #[test]
    fn mikmik_home_empty_env_override_ignored() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("MIKMIK_HOME", "");

        // Empty override falls through to XDG (no legacy dir, no XDG_CONFIG_HOME).
        assert_eq!(
            Settings::config_dir(),
            home.path().join(".config").join("mikmik")
        );
    }

    #[test]
    fn a_directory_left_by_the_old_name_is_not_read() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let home = tempfile::tempdir().unwrap();
        // A leftover `~/.claurst` used to win over XDG. It no longer does, and
        // this asserts the clean break rather than the old precedence.
        std::fs::create_dir_all(home.path().join(".claurst")).unwrap();
        std::env::set_var("HOME", home.path());

        assert_eq!(
            Settings::config_dir(),
            home.path().join(".config").join("mikmik")
        );
    }

    #[test]
    fn mikmik_home_xdg_used_when_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", xdg.path());

        assert_eq!(Settings::config_dir(), xdg.path().join("mikmik"));
    }

    #[test]
    fn mikmik_home_xdg_default_when_no_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());

        // No MIKMIK_HOME and no XDG_CONFIG_HOME → ~/.config/mikmik.
        assert_eq!(
            Settings::config_dir(),
            home.path().join(".config").join("mikmik")
        );
    }

    #[test]
    fn mikmik_home_relative_xdg_ignored() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        // Per the XDG spec a relative $XDG_CONFIG_HOME is invalid and ignored.
        std::env::set_var("XDG_CONFIG_HOME", "relative/path");

        assert_eq!(
            Settings::config_dir(),
            home.path().join(".config").join("mikmik")
        );
    }

    #[test]
    fn mikmik_home_wrapper_matches_config_dir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::new();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("MIKMIK_HOME", tmp.path());
        assert_eq!(super::mikmik_home(), Settings::config_dir());
        assert_eq!(super::mikmik_home(), PathBuf::from(tmp.path()));
    }
}
