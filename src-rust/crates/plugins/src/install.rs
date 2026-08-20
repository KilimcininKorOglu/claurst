//! Installing, updating and removing plugins.
//!
//! A plugin arrives from one of two places: a directory already on the
//! machine, or a git repository. The git path covers what people actually
//! publish, which is a GitHub repository holding either one plugin or a
//! marketplace of several.
//!
//! Nothing here talks to a plugin registry service. There is no such service
//! for this build, and an install that could only reach one would install
//! nothing.

use crate::loader::find_manifest;
use crate::manifest::PluginManifest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An installed plugin summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub install_path: PathBuf,
    pub description: String,
}

/// Where `/plugin install <spec>` should read the plugin from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// A directory on this machine.
    Local(PathBuf),
    /// A git repository, optionally pinned to a branch or tag.
    Git {
        url: String,
        reference: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Spec parsing
// ---------------------------------------------------------------------------

/// Decide what `input` refers to.
///
/// Accepted forms:
/// - a path to an existing directory, absolute or relative, `~` expanded
/// - `owner/repo`, optionally `owner/repo@ref`, which means GitHub
/// - `https://…`, `ssh://…`, `file://…` or `git@host:owner/repo`, used as
///   given
///
/// A path that exists wins over the `owner/repo` reading, so a local
/// directory named like a repository still installs from disk.
///
/// The remaining schemes are rejected rather than passed to git: `git://` and
/// `http://` fetch code over a connection nobody authenticated, and `ext::`
/// makes git run a command of the URL's choosing.
pub fn parse_install_source(input: &str) -> Result<InstallSource, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("No plugin given. Pass a directory, an owner/repo, or a git URL.".to_string());
    }
    // git reads a leading dash as an option, whatever the position it lands in.
    if input.starts_with('-') {
        return Err(format!("'{input}' is not a valid plugin source."));
    }

    let expanded = expand_tilde(input);
    if expanded.is_dir() {
        return Ok(InstallSource::Local(expanded));
    }

    for accepted in ["https://", "ssh://", "file://"] {
        if let Some(rest) = input.strip_prefix(accepted) {
            if rest.is_empty() {
                return Err(format!("'{input}' names no repository."));
            }
            return Ok(InstallSource::Git {
                url: input.to_string(),
                reference: None,
            });
        }
    }
    if input.starts_with("git@") && input.contains(':') {
        return Ok(InstallSource::Git {
            url: input.to_string(),
            reference: None,
        });
    }
    for rejected in ["http://", "git://", "ext::"] {
        if input.starts_with(rejected) {
            return Err(format!(
                "'{rejected}' sources are not installed. Use https://, ssh://, \
                 git@host:owner/repo, a file:// path, or a local directory."
            ));
        }
    }

    // owner/repo, optionally @ref.
    let (repo_part, reference) = match input.split_once('@') {
        Some((repo, reference)) => (repo, Some(reference)),
        None => (input, None),
    };
    let Some((owner, repo)) = repo_part.split_once('/') else {
        return Err(format!(
            "'{input}' is neither an existing directory nor an owner/repo or git URL."
        ));
    };
    if repo.contains('/') {
        return Err(format!(
            "'{input}' has more than two path segments; an owner/repo has exactly two."
        ));
    }
    if !is_safe_segment(owner) || !is_safe_segment(repo) {
        return Err(format!(
            "'{input}' is not a valid owner/repo: use letters, digits, '.', '_' and '-'."
        ));
    }
    if let Some(reference) = reference {
        if !is_safe_reference(reference) {
            return Err(format!("'{reference}' is not a valid branch or tag name."));
        }
    }

    let repo = repo.trim_end_matches(".git");
    Ok(InstallSource::Git {
        url: format!("https://github.com/{owner}/{repo}"),
        reference: reference.map(|r| r.to_string()),
    })
}

fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_safe_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('-')
        && !reference.contains("..")
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

// ---------------------------------------------------------------------------
// Marketplace manifest (a repository holding several plugins)
// ---------------------------------------------------------------------------

/// `.claude-plugin/marketplace.json`: one repository, several plugins.
#[derive(Debug, Clone, Deserialize)]
struct MarketplaceManifest {
    #[serde(default)]
    plugins: Vec<MarketplaceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct MarketplaceEntry {
    name: String,
    /// Where the plugin lives. A string is a path inside the repository; an
    /// object points at another repository, which this installer does not
    /// follow.
    #[serde(default)]
    source: serde_json::Value,
}

/// The directories inside `repo` that hold a plugin, and the name each entry
/// was listed under.
///
/// A repository with a manifest at its root is one plugin. Otherwise its
/// marketplace file lists them.
fn plugin_dirs_in_repo(repo: &Path) -> Result<Vec<PathBuf>, String> {
    if find_manifest(repo).is_some() {
        return Ok(vec![repo.to_path_buf()]);
    }

    let marketplace_path = repo.join(".claude-plugin").join("marketplace.json");
    if !marketplace_path.exists() {
        return Err(
            "The repository holds no plugin: no manifest at its root and no \
             .claude-plugin/marketplace.json."
                .to_string(),
        );
    }

    let bytes = std::fs::read(&marketplace_path)
        .map_err(|e| format!("Could not read {}: {e}", marketplace_path.display()))?;
    let marketplace: MarketplaceManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not valid JSON: {e}", marketplace_path.display()))?;

    if marketplace.plugins.is_empty() {
        return Err("The repository's marketplace file lists no plugins.".to_string());
    }

    let mut dirs = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for entry in marketplace.plugins {
        let Some(relative) = entry.source.as_str() else {
            skipped.push(entry.name);
            continue;
        };
        let candidate = repo.join(relative.trim_start_matches("./"));
        // Keep the resolved path inside the clone: a marketplace file is
        // remote input, and `../` in it would otherwise reach the rest of the
        // filesystem.
        if !candidate.starts_with(repo) || relative.contains("..") {
            skipped.push(entry.name);
            continue;
        }
        if find_manifest(&candidate).is_some() {
            dirs.push(candidate);
        } else {
            skipped.push(entry.name);
        }
    }

    if dirs.is_empty() {
        return Err(format!(
            "None of the {} plugins the marketplace file lists could be read from this \
             repository. An entry that names another repository is not followed.",
            skipped.len()
        ));
    }
    Ok(dirs)
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Clone `url` and install every plugin the repository holds.
///
/// The clone lands outside the plugins directory first, so a repository that
/// turns out to hold nothing installable never appears as a half-installed
/// plugin. `.git` is kept, which is what `/plugin update` needs later.
pub async fn install_from_git(
    url: &str,
    reference: Option<&str>,
) -> Result<Vec<InstalledPlugin>, String> {
    let staging = staging_dir()?;
    let clone_path = staging.join("repo");

    let result = clone_and_install(url, reference, &clone_path).await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    } else {
        // The plugins moved out; what is left is the empty staging shell.
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

async fn clone_and_install(
    url: &str,
    reference: Option<&str>,
    clone_path: &Path,
) -> Result<Vec<InstalledPlugin>, String> {
    git_clone(url, reference, clone_path).await?;

    let plugin_dirs = plugin_dirs_in_repo(clone_path)?;
    let plugins_root = plugins_root()?;
    std::fs::create_dir_all(&plugins_root)
        .map_err(|e| format!("Could not create {}: {e}", plugins_root.display()))?;

    // Read every manifest before moving anything, so a name that collides with
    // an installed plugin stops the install with nothing half-done.
    let mut staged: Vec<(PathBuf, PluginManifest)> = Vec::new();
    for dir in plugin_dirs {
        let manifest = read_manifest(&dir)?;
        let destination = plugins_root.join(&manifest.name);
        if destination.exists() {
            return Err(format!(
                "Plugin '{}' is already installed at {}. Run /plugin update {} to update it, \
                 or /plugin remove {} first.",
                manifest.name,
                destination.display(),
                manifest.name,
                manifest.name
            ));
        }
        staged.push((dir, manifest));
    }

    let mut installed = Vec::new();
    for (dir, manifest) in staged {
        let destination = plugins_root.join(&manifest.name);
        move_dir(&dir, &destination)?;
        installed.push(InstalledPlugin {
            name: manifest.name.clone(),
            version: manifest
                .version
                .clone()
                .unwrap_or_else(|| "0.0.0".to_string()),
            install_path: destination,
            description: manifest.description.clone().unwrap_or_default(),
        });
    }

    Ok(installed)
}

async fn git_clone(url: &str, reference: Option<&str>, dest: &Path) -> Result<(), String> {
    let mut command = tokio::process::Command::new("git");
    command
        // `ext::` URLs run a command of the URL's choosing. The scheme check in
        // parse_install_source already rejects them; this closes the same door
        // for a redirect or a submodule that arrives later.
        .arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--single-branch");
    if let Some(reference) = reference {
        command.arg("--branch").arg(reference);
    }
    command
        .arg("--")
        .arg(url)
        .arg(dest)
        // Without this git waits on a username prompt that nothing is there to
        // answer, and the session hangs instead of reporting the failure.
        .env("GIT_TERMINAL_PROMPT", "0");

    let output = command
        .output()
        .await
        .map_err(|e| format!("Could not run git: {e}. Is git installed?"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git clone failed: {}",
            stderr.trim().lines().next_back().unwrap_or("no output")
        ));
    }
    Ok(())
}

/// Install from a directory that is already on this machine.
pub fn install_from_local(source: &Path) -> Result<InstalledPlugin, String> {
    if !source.is_dir() {
        return Err(format!("{} is not a directory.", source.display()));
    }
    let manifest = read_manifest(source)?;
    let plugins_root = plugins_root()?;
    let destination = plugins_root.join(&manifest.name);
    if destination.exists() {
        return Err(format!(
            "Plugin '{}' is already installed at {}. Run /plugin remove {} first.",
            manifest.name,
            destination.display(),
            manifest.name
        ));
    }
    std::fs::create_dir_all(&plugins_root)
        .map_err(|e| format!("Could not create {}: {e}", plugins_root.display()))?;
    copy_dir(source, &destination)?;

    Ok(InstalledPlugin {
        name: manifest.name.clone(),
        version: manifest
            .version
            .clone()
            .unwrap_or_else(|| "0.0.0".to_string()),
        install_path: destination,
        description: manifest.description.clone().unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Update / uninstall / list
// ---------------------------------------------------------------------------

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The checkout already had the latest commit.
    AlreadyCurrent,
    /// The checkout moved, and this is what git reported.
    Updated(String),
}

/// Pull the latest commit for a plugin installed from git.
///
/// A plugin installed from a local directory has no remote to pull from, and
/// says so rather than silently doing nothing.
pub async fn update_installed(name: &str) -> Result<UpdateOutcome, String> {
    let dir = plugins_root()?.join(name);
    if !dir.is_dir() {
        return Err(format!("Plugin '{name}' is not installed."));
    }
    if !dir.join(".git").exists() {
        return Err(format!(
            "Plugin '{name}' was installed from a local directory, so there is nothing to pull. \
             Reinstall it to pick up changes."
        ));
    }

    let before = git_head(&dir).await?;
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .arg("pull")
        .arg("--ff-only")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| format!("Could not run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git pull failed: {}",
            stderr.trim().lines().next_back().unwrap_or("no output")
        ));
    }
    let after = git_head(&dir).await?;

    if before == after {
        Ok(UpdateOutcome::AlreadyCurrent)
    } else {
        Ok(UpdateOutcome::Updated(format!(
            "{} → {}",
            short(&before),
            short(&after)
        )))
    }
}

async fn git_head(dir: &Path) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .await
        .map_err(|e| format!("Could not run git: {e}"))?;
    if !output.status.success() {
        return Err("Could not read the plugin's current commit.".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn short(commit: &str) -> String {
    commit.chars().take(8).collect()
}

/// Remove an installed plugin's directory.
pub fn uninstall(name: &str) -> Result<PathBuf, String> {
    if !is_safe_segment(name) {
        return Err(format!("'{name}' is not a plugin name."));
    }
    let dir = plugins_root()?.join(name);
    if !dir.is_dir() {
        return Err(format!("Plugin '{name}' is not installed."));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("Could not remove {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Every plugin directory under the user plugins root, with what its manifest
/// says about it.
pub fn list_installed() -> Vec<InstalledPlugin> {
    let Ok(plugins_root) = plugins_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&plugins_root) else {
        return Vec::new();
    };

    let mut installed: Vec<InstalledPlugin> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let manifest = read_manifest(&path).ok()?;
            Some(InstalledPlugin {
                name: manifest.name.clone(),
                version: manifest
                    .version
                    .clone()
                    .unwrap_or_else(|| "0.0.0".to_string()),
                install_path: path,
                description: manifest.description.clone().unwrap_or_default(),
            })
        })
        .collect();
    installed.sort_by(|a, b| a.name.cmp(&b.name));
    installed
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn plugins_root() -> Result<PathBuf, String> {
    crate::loader::default_user_plugins_dir()
        .ok_or_else(|| "Could not determine the Claurst home directory.".to_string())
}

/// A directory to clone into, next to the plugins directory rather than inside
/// it, so a clone in progress is never discovered as a plugin.
fn staging_dir() -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let root = mikmik_core::config::Settings::config_dir().join(".plugin-installs");
    let unique = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = root.join(unique);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn read_manifest(dir: &Path) -> Result<PluginManifest, String> {
    let Some(path) = find_manifest(dir) else {
        return Err(format!(
            "{} holds no plugin manifest (looked for {}).",
            dir.display(),
            crate::loader::MANIFEST_LOCATIONS.join(", ")
        ));
    };
    let bytes =
        std::fs::read(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let manifest = if path.extension().map(|e| e == "toml").unwrap_or(false) {
        PluginManifest::from_toml(&bytes)
    } else {
        PluginManifest::from_json(&bytes)
    }
    .map_err(|e| format!("{} is not a valid manifest: {e}", path.display()))?;

    if !is_safe_segment(&manifest.name) {
        return Err(format!(
            "The manifest names the plugin '{}', which cannot be a directory name.",
            manifest.name
        ));
    }
    Ok(manifest)
}

/// Move `source` to `destination`, falling back to a copy when the two are on
/// different filesystems.
fn move_dir(source: &Path, destination: &Path) -> Result<(), String> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir(source, destination)?;
            let _ = std::fs::remove_dir_all(source);
            Ok(())
        }
    }
}

fn copy_dir(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
    let entries = std::fs::read_dir(source)
        .map_err(|e| format!("Could not read {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Could not read {}: {e}", source.display()))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Could not inspect {}: {e}", from.display()))?;
        if file_type.is_dir() {
            copy_dir(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("Could not copy {}: {e}", from.display()))?;
        }
        // A symlink is skipped: it would point outside the plugin directory
        // and nothing in a plugin needs one.
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_repo_means_github() {
        assert_eq!(
            parse_install_source("acme/my-plugin"),
            Ok(InstallSource::Git {
                url: "https://github.com/acme/my-plugin".to_string(),
                reference: None,
            })
        );
    }

    #[test]
    fn a_reference_pins_the_branch() {
        assert_eq!(
            parse_install_source("acme/my-plugin@v1.2.0"),
            Ok(InstallSource::Git {
                url: "https://github.com/acme/my-plugin".to_string(),
                reference: Some("v1.2.0".to_string()),
            })
        );
    }

    #[test]
    fn a_dot_git_suffix_is_dropped_from_the_shorthand() {
        assert_eq!(
            parse_install_source("acme/my-plugin.git"),
            Ok(InstallSource::Git {
                url: "https://github.com/acme/my-plugin".to_string(),
                reference: None,
            })
        );
    }

    #[test]
    fn a_full_url_is_used_as_given() {
        assert_eq!(
            parse_install_source("https://gitlab.com/acme/my-plugin.git"),
            Ok(InstallSource::Git {
                url: "https://gitlab.com/acme/my-plugin.git".to_string(),
                reference: None,
            })
        );
        assert_eq!(
            parse_install_source("git@github.com:acme/my-plugin.git"),
            Ok(InstallSource::Git {
                url: "git@github.com:acme/my-plugin.git".to_string(),
                reference: None,
            })
        );
    }

    #[test]
    fn an_existing_directory_wins_over_the_owner_repo_reading() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("acme").join("my-plugin");
        std::fs::create_dir_all(&dir).expect("dir");
        let spec = dir.to_string_lossy().into_owned();
        assert_eq!(parse_install_source(&spec), Ok(InstallSource::Local(dir)));
    }

    #[test]
    fn unauthenticated_and_command_running_schemes_are_refused() {
        for spec in [
            "http://example.com/p.git",
            "git://example.com/p.git",
            "ext::sh -c 'echo pwned'",
        ] {
            assert!(
                parse_install_source(spec).is_err(),
                "{spec} must not be installed"
            );
        }
    }

    #[test]
    fn a_file_url_clones_from_disk() {
        assert_eq!(
            parse_install_source("file:///srv/repos/my-plugin.git"),
            Ok(InstallSource::Git {
                url: "file:///srv/repos/my-plugin.git".to_string(),
                reference: None,
            })
        );
    }

    #[test]
    fn a_leading_dash_is_not_a_source() {
        assert!(parse_install_source("--upload-pack=touch /tmp/pwn").is_err());
    }

    #[test]
    fn a_traversing_owner_repo_is_refused() {
        assert!(parse_install_source("../../etc/passwd").is_err());
        assert!(parse_install_source("acme/../../etc").is_err());
        assert!(parse_install_source("acme/repo@../../evil").is_err());
    }

    #[test]
    fn a_repository_with_a_root_manifest_is_one_plugin() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(
            tmp.path().join("plugin.json"),
            r#"{"name": "solo", "version": "1.0.0"}"#,
        )
        .expect("manifest");
        assert_eq!(
            plugin_dirs_in_repo(tmp.path()),
            Ok(vec![tmp.path().to_path_buf()])
        );
    }

    #[test]
    fn a_marketplace_repository_yields_every_listed_plugin() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join(".claude-plugin")).expect("metadata dir");
        for name in ["one", "two"] {
            let dir = tmp.path().join("plugins").join(name);
            std::fs::create_dir_all(&dir).expect("plugin dir");
            std::fs::write(
                dir.join("plugin.json"),
                format!(r#"{{"name": "{name}", "version": "1.0.0"}}"#),
            )
            .expect("manifest");
        }
        std::fs::write(
            tmp.path().join(".claude-plugin").join("marketplace.json"),
            r#"{"name": "acme", "plugins": [
                 {"name": "one", "source": "./plugins/one"},
                 {"name": "two", "source": "./plugins/two"}
               ]}"#,
        )
        .expect("marketplace");

        let dirs = plugin_dirs_in_repo(tmp.path()).expect("dirs");
        assert_eq!(dirs.len(), 2);
        assert!(dirs.contains(&tmp.path().join("plugins").join("one")));
        assert!(dirs.contains(&tmp.path().join("plugins").join("two")));
    }

    #[test]
    fn a_marketplace_entry_pointing_outside_the_clone_is_skipped() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(tmp.path().join(".claude-plugin")).expect("metadata dir");
        let inside = tmp.path().join("good");
        std::fs::create_dir_all(&inside).expect("plugin dir");
        std::fs::write(
            inside.join("plugin.json"),
            r#"{"name": "good", "version": "1.0.0"}"#,
        )
        .expect("manifest");
        std::fs::write(
            tmp.path().join(".claude-plugin").join("marketplace.json"),
            r#"{"plugins": [
                 {"name": "escape", "source": "../../elsewhere"},
                 {"name": "remote", "source": {"source": "github", "repo": "a/b"}},
                 {"name": "good", "source": "./good"}
               ]}"#,
        )
        .expect("marketplace");

        let dirs = plugin_dirs_in_repo(tmp.path()).expect("dirs");
        assert_eq!(dirs, vec![inside]);
    }

    #[test]
    fn a_repository_holding_no_plugin_reports_it() {
        let tmp = tempfile::tempdir().expect("tmp");
        let err = plugin_dirs_in_repo(tmp.path()).expect_err("empty repo");
        assert!(err.contains("marketplace.json"), "{err}");
    }

    #[test]
    fn a_manifest_naming_a_path_is_refused() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(
            tmp.path().join("plugin.json"),
            r#"{"name": "../escape", "version": "1.0.0"}"#,
        )
        .expect("manifest");
        assert!(read_manifest(tmp.path()).is_err());
    }
}
