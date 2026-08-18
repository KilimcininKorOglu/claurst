//! Named workspace roots.
//!
//! A session can reach more than one directory: the working directory plus
//! whatever `--add-dir` and `workspace_paths` add. Naming those directories
//! lets the model and the user refer to them as `&docs/spec.md` instead of
//! spelling out an absolute path every time.
//!
//! The primary directory is always `main`. Names are derived from the last
//! path component, so the same configuration always produces the same names
//! and nothing has to be persisted.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The name of the primary root, which is the session's working directory.
pub const MAIN_ROOT: &str = "main";

/// What a tool path argument turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootRef<'a> {
    /// The input is absolute, or relative without a `&` prefix.
    Plain,
    /// `&name` (with an empty `relative`) or `&name/relative`.
    Root { name: &'a str, relative: &'a str },
    /// The input asks for `&name`, but no root carries that name.
    Unknown(&'a str),
}

/// Name every directory this session can reach.
///
/// `main` is always `primary`. Additional directories are named after their
/// last path component; a name already taken gains a `-2`, `-3` suffix, and a
/// path already registered is skipped.
pub fn generate_root_names(
    primary: &Path,
    additional_dirs: &[PathBuf],
    workspace_paths: &[PathBuf],
) -> BTreeMap<String, PathBuf> {
    let mut roots = BTreeMap::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    roots.insert(MAIN_ROOT.to_string(), primary.to_path_buf());
    seen_paths.insert(primary.to_path_buf());
    seen_names.insert(MAIN_ROOT.to_string());

    for path in additional_dirs.iter().chain(workspace_paths.iter()) {
        // A relative `--add-dir` is relative to the session, so anchor it
        // before it becomes a root the model resolves paths against.
        let path = if path.is_absolute() {
            path.clone()
        } else {
            primary.join(path)
        };
        if !seen_paths.insert(path.clone()) {
            continue;
        }

        let base = path
            .file_name()
            .map(|component| sanitize_root_name(&component.to_string_lossy()))
            .unwrap_or_else(|| "workspace".to_string());
        let mut name = base.clone();
        let mut counter = 2;
        while !seen_names.insert(name.clone()) {
            name = format!("{base}-{counter}");
            counter += 1;
        }

        roots.insert(name, path);
    }

    roots
}

/// Read a `&name` or `&name/relative` prefix off a tool path argument.
pub fn parse_root_ref<'a>(input: &'a str, roots: &BTreeMap<String, PathBuf>) -> RootRef<'a> {
    if Path::new(input).is_absolute() {
        return RootRef::Plain;
    }
    let Some(without_prefix) = input.strip_prefix('&') else {
        return RootRef::Plain;
    };

    let (name, relative) = match without_prefix.find(['/', '\\']) {
        Some(position) => (&without_prefix[..position], &without_prefix[position + 1..]),
        None => (without_prefix, ""),
    };

    if roots.contains_key(name) {
        RootRef::Root { name, relative }
    } else {
        RootRef::Unknown(name)
    }
}

/// Turn a directory name into something safe to type in a prompt: lowercase,
/// with every other character folded to a single dash.
fn sanitize_root_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut previous_dash = false;

    for ch in name.chars().flat_map(char::to_lowercase) {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        out.push(mapped);
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    #[test]
    fn the_working_directory_is_always_main() {
        let roots = generate_root_names(Path::new("/repo"), &[], &[]);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots.get(MAIN_ROOT), Some(&path("/repo")));
    }

    #[test]
    fn an_extra_directory_is_named_after_its_last_component() {
        let roots = generate_root_names(Path::new("/repo"), &[path("/other/docs")], &[]);
        assert_eq!(roots.get("docs"), Some(&path("/other/docs")));
    }

    #[test]
    fn workspace_paths_are_named_too() {
        let roots = generate_root_names(Path::new("/repo"), &[], &[path("/other/lib")]);
        assert_eq!(roots.get("lib"), Some(&path("/other/lib")));
    }

    #[test]
    fn a_repeated_name_gains_a_counter() {
        let roots = generate_root_names(
            Path::new("/repo"),
            &[path("/a/lib"), path("/b/lib"), path("/c/lib")],
            &[],
        );
        assert_eq!(roots.get("lib"), Some(&path("/a/lib")));
        assert_eq!(roots.get("lib-2"), Some(&path("/b/lib")));
        assert_eq!(roots.get("lib-3"), Some(&path("/c/lib")));
    }

    #[test]
    fn a_directory_named_main_does_not_take_the_primary_name() {
        let roots = generate_root_names(Path::new("/repo"), &[path("/other/main")], &[]);
        assert_eq!(roots.get(MAIN_ROOT), Some(&path("/repo")));
        assert_eq!(roots.get("main-2"), Some(&path("/other/main")));
    }

    #[test]
    fn the_same_path_is_registered_once() {
        let roots = generate_root_names(
            Path::new("/repo"),
            &[path("/other/docs"), path("/other/docs")],
            &[path("/other/docs")],
        );
        assert_eq!(roots.len(), 2);
    }

    #[test]
    fn the_working_directory_is_not_repeated_as_an_extra_root() {
        let roots = generate_root_names(Path::new("/repo"), &[path("/repo")], &[]);
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn a_relative_extra_directory_anchors_on_the_working_directory() {
        let roots = generate_root_names(Path::new("/repo"), &[path("../sibling")], &[]);
        assert_eq!(roots.get("sibling"), Some(&path("/repo/../sibling")));
    }

    #[test]
    fn a_name_is_lowercased_and_folded() {
        let roots = generate_root_names(
            Path::new("/repo"),
            &[path("/x/_ai-engine"), path("/x/My Project (API)")],
            &[],
        );
        assert_eq!(roots.get("_ai-engine"), Some(&path("/x/_ai-engine")));
        assert_eq!(
            roots.get("my-project-api"),
            Some(&path("/x/My Project (API)"))
        );
    }

    #[test]
    fn a_name_of_only_punctuation_falls_back() {
        let roots = generate_root_names(Path::new("/repo"), &[path("/x/@@@")], &[]);
        assert_eq!(roots.get("workspace"), Some(&path("/x/@@@")));
    }

    fn roots_fixture() -> BTreeMap<String, PathBuf> {
        generate_root_names(Path::new("/repo"), &[path("/other/docs")], &[])
    }

    #[test]
    fn a_root_reference_splits_into_name_and_remainder() {
        assert_eq!(
            parse_root_ref("&docs/spec.md", &roots_fixture()),
            RootRef::Root {
                name: "docs",
                relative: "spec.md"
            }
        );
    }

    #[test]
    fn a_bare_root_reference_has_no_remainder() {
        assert_eq!(
            parse_root_ref("&docs", &roots_fixture()),
            RootRef::Root {
                name: "docs",
                relative: ""
            }
        );
    }

    #[test]
    fn a_backslash_separates_too() {
        assert_eq!(
            parse_root_ref("&docs\\spec.md", &roots_fixture()),
            RootRef::Root {
                name: "docs",
                relative: "spec.md"
            }
        );
    }

    #[test]
    fn an_unknown_root_is_reported_rather_than_guessed() {
        assert_eq!(
            parse_root_ref("&nope/spec.md", &roots_fixture()),
            RootRef::Unknown("nope")
        );
    }

    #[test]
    fn an_ordinary_path_is_plain() {
        let roots = roots_fixture();
        assert_eq!(parse_root_ref("src/lib.rs", &roots), RootRef::Plain);
        assert_eq!(parse_root_ref("/etc/hosts", &roots), RootRef::Plain);
    }
}
