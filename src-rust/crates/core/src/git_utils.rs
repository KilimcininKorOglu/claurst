//! Git utilities for Claurst.
//!
//! Read-only: every helper here reports on a repository and none of them
//! writes to one. Anything that changes a user's checkout belongs somewhere
//! that can explain itself, not behind a one-line wrapper.

use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Repository discovery
// ---------------------------------------------------------------------------

/// Walk up the directory tree to find the nearest `.git` directory.
pub fn get_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Run a git command in `repo_root` and return stdout as a String.
/// Returns empty string on failure (non-zero exit, not-a-repo, etc.).
fn git_output(repo_root: &Path, args: &[&str]) -> String {
    Command::new("git")
        .current_dir(repo_root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Branch / status
// ---------------------------------------------------------------------------

/// Return the current branch name (or "HEAD" if detached).
pub fn get_current_branch(repo_root: &Path) -> String {
    let branch = git_output(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    if branch.is_empty() {
        "HEAD".to_string()
    } else {
        branch
    }
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Return the staged diff (index vs HEAD).
pub fn get_staged_diff(repo_root: &Path) -> String {
    git_output(repo_root, &["diff", "--cached"])
}

/// Return the unstaged diff (working tree vs index).
pub fn get_unstaged_diff(repo_root: &Path) -> String {
    git_output(repo_root, &["diff"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn get_repo_root_finds_git() {
        // Run from within the src-rust workspace which has .git
        let result = get_repo_root(Path::new("."));
        // Should find the repo root (may or may not exist in test env)
        // Just verify it doesn't panic.
        let _ = result;
    }

    #[test]
    fn a_directory_outside_any_repository_has_no_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(get_repo_root(dir.path()), None);
    }

    #[test]
    fn a_path_that_is_not_a_repository_reads_as_detached() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(get_current_branch(dir.path()), "HEAD");
    }

    #[test]
    fn the_diffs_of_a_non_repository_are_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(get_staged_diff(dir.path()).is_empty());
        assert!(get_unstaged_diff(dir.path()).is_empty());
    }
}
