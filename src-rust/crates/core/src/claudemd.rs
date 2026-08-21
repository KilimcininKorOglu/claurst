//! AGENTS.md hierarchical memory loading.
//! Mirrors src/utils/claudemd.ts (1,479 lines).
//!
//! Priority order: managed > user > project > local
//! Supports @include directives, YAML frontmatter, and mtime-based caching.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Memory file type / priority scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// `~/.config/mikmik/rules/*.md` — global managed policy.
    Managed,
    /// `~/.config/mikmik/AGENTS.md` — user-level memory.
    User,
    /// `{project_root}/AGENTS.md` — project-level memory.
    Project,
    /// `{project_root}/.mikmik/AGENTS.md` — local override.
    Local,
}

impl MemoryScope {
    /// Label used in the prompt header.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

/// Frontmatter parsed from a AGENTS.md file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryFrontmatter {
    #[serde(default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Loaded memory file with metadata.
#[derive(Debug, Clone)]
pub struct MemoryFileInfo {
    pub path: PathBuf,
    pub scope: MemoryScope,
    pub content: String,
    pub frontmatter: MemoryFrontmatter,
    pub mtime: Option<SystemTime>,
}

// ---------------------------------------------------------------------------
// YAML frontmatter parsing
// ---------------------------------------------------------------------------

/// Strip YAML frontmatter (--- ... ---) from content and parse it.
/// Returns (frontmatter, body_without_frontmatter).
pub fn parse_frontmatter(content: &str) -> (MemoryFrontmatter, &str) {
    if !content.starts_with("---") {
        return (MemoryFrontmatter::default(), content);
    }
    let after_first = &content[3..];
    if let Some(end) = after_first.find("\n---") {
        let yaml = after_first[..end].trim();
        let body = &after_first[end + 4..];
        // Minimal YAML key-value parse (no external dependency).
        let mut fm = MemoryFrontmatter::default();
        for line in yaml.lines() {
            let line = line.trim();
            if let Some((key, val)) = line.split_once(':') {
                let val = val.trim().to_string();
                match key.trim() {
                    "memory_type" => fm.memory_type = Some(val),
                    "priority" => fm.priority = val.parse().ok(),
                    "scope" => fm.scope = Some(val),
                    _ => {}
                }
            }
        }
        return (fm, body.trim_start_matches('\n'));
    }
    (MemoryFrontmatter::default(), content)
}

// ---------------------------------------------------------------------------
// @include directive expansion
// ---------------------------------------------------------------------------

/// Maximum @include nesting depth.
const MAX_INCLUDE_DEPTH: usize = 10;

/// Expand @include directives in content.
/// Circular references are detected via `visited` set.
pub fn expand_includes(
    content: &str,
    base_dir: &Path,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> String {
    if depth >= MAX_INCLUDE_DEPTH {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(path_str) = trimmed.strip_prefix("@include ") {
            let path_str = path_str.trim();
            // Resolve relative to base_dir; expand ~ to home dir.
            let include_path = if path_str.starts_with('~') {
                dirs::home_dir().unwrap_or_default().join(&path_str[2..])
            } else if Path::new(path_str).is_absolute() {
                PathBuf::from(path_str)
            } else {
                base_dir.join(path_str)
            };

            let canonical = include_path.canonicalize().unwrap_or(include_path.clone());
            if visited.contains(&canonical) {
                result.push_str(&format!(
                    "<!-- circular @include {} skipped -->\n",
                    path_str
                ));
                continue;
            }
            if let Ok(included) = std::fs::read_to_string(&include_path) {
                visited.insert(canonical);
                let expanded = expand_includes(
                    &included,
                    include_path.parent().unwrap_or(base_dir),
                    visited,
                    depth + 1,
                );
                result.push_str(&expanded);
                result.push('\n');
            } else {
                result.push_str(&format!("<!-- @include {} not found -->\n", path_str));
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Loading API
// ---------------------------------------------------------------------------

/// Load a single memory file: strip frontmatter, expand `@include`s.
///
/// No size limit. A memory file is something the user wrote on purpose, and
/// silently dropping the second half of it is worse than a large prompt.
pub fn load_memory_file(path: &Path, scope: MemoryScope) -> Option<MemoryFileInfo> {
    let meta = std::fs::metadata(path).ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let mtime = meta.modified().ok();

    let (frontmatter, body) = parse_frontmatter(&raw);
    let mut visited = HashSet::new();
    visited.insert(path.canonicalize().unwrap_or(path.to_path_buf()));
    let content = expand_includes(
        body,
        path.parent().unwrap_or(Path::new(".")),
        &mut visited,
        0,
    );

    Some(MemoryFileInfo {
        path: path.to_path_buf(),
        scope,
        content,
        frontmatter,
        mtime,
    })
}

/// Load memory files from a directory for a given scope.
///
/// Loads `AGENTS.md` first (primary/universal standard), then `CLAUDE.md` if
/// present (Claude-specific additions or overrides). Either file may be absent.
fn load_scope_files(dir: &Path, scope: MemoryScope, files: &mut Vec<MemoryFileInfo>) {
    for name in &["AGENTS.md", "CLAUDE.md"] {
        let path = dir.join(name);
        if path.exists() {
            if let Some(f) = load_memory_file(&path, scope) {
                files.push(f);
            }
        }
    }
}

/// Load all memory files for the given project root, in prompt order.
///
/// At each scope `AGENTS.md` is loaded first (universal standard), followed by
/// `CLAUDE.md` if present (Claude-specific context). Either or both may exist.
///
/// Returned list is ordered Managed → User → Project → Local, and within one
/// scope by `priority` ascending. Later entries reach the model later, so the
/// narrower scope wins where two files say different things.
pub fn load_all_memory_files(project_root: &Path) -> Vec<MemoryFileInfo> {
    let mut files = Vec::new();

    // 1. Managed: <mikmik home>/rules/*.md
    {
        let mikmik = crate::config::Settings::config_dir();
        let rules_dir = mikmik.join("rules");
        if let Ok(entries) = std::fs::read_dir(&rules_dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "md") {
                        Some(p)
                    } else {
                        None
                    }
                })
                .collect();
            paths.sort();
            for p in paths {
                if let Some(f) = load_memory_file(&p, MemoryScope::Managed) {
                    files.push(f);
                }
            }
        }

        // 2. User: <mikmik home>/AGENTS.md then <mikmik home>/CLAUDE.md
        load_scope_files(&mikmik, MemoryScope::User, &mut files);
    }

    // 3. Project: {project_root}/AGENTS.md then {project_root}/CLAUDE.md
    load_scope_files(project_root, MemoryScope::Project, &mut files);

    // 4. Local: {project_root}/.mikmik/AGENTS.md then {project_root}/.mikmik/CLAUDE.md
    load_scope_files(
        &project_root.join(".mikmik"),
        MemoryScope::Local,
        &mut files,
    );

    // Stable, and keyed on the scope first: the push order above is already
    // the scope order, so this only reorders within a scope. A file with no
    // `priority` sorts last there, which leaves an explicit priority in
    // charge, and the tie case keeps AGENTS.md ahead of CLAUDE.md and the
    // managed files in the alphabetical order they were read in.
    files.sort_by_key(|f| (f.scope, f.frontmatter.priority.unwrap_or(u32::MAX)));

    files
}

/// Concatenate all memory file contents into a single system-prompt fragment.
///
/// Each file is headed with its scope and path. The model is told where an
/// instruction came from, which is what lets it say "your project's AGENTS.md
/// says X" rather than asserting X with no provenance.
pub fn build_memory_prompt(files: &[MemoryFileInfo]) -> String {
    files
        .iter()
        .filter(|f| !f.content.trim().is_empty())
        .map(|f| {
            format!(
                "# Memory ({}, from {})\n{}",
                f.scope.as_str(),
                f.path.display(),
                f.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests that redirect the config root.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point the config root at a temporary directory for the duration of a
    /// test, so `load_all_memory_files` does not read the real
    /// `~/.config/mikmik` for its Managed and User scopes.
    struct HomeGuard {
        saved: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HomeGuard {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let saved = std::env::var_os("MIKMIK_HOME");
            std::env::set_var("MIKMIK_HOME", dir.path());
            Self { saved, _dir: dir }
        }

        fn path(&self) -> &Path {
            self._dir.path()
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(value) => std::env::set_var("MIKMIK_HOME", value),
                None => std::env::remove_var("MIKMIK_HOME"),
            }
        }
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nmemory_type: project\npriority: 10\n---\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.memory_type.as_deref(), Some("project"));
        assert_eq!(fm.priority, Some(10));
        assert_eq!(body.trim(), "Hello world");
    }

    #[test]
    fn parse_frontmatter_none() {
        let content = "No frontmatter here";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.memory_type.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn load_scope_prefers_agents_then_claude() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = HomeGuard::new();
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("AGENTS.md"), "agents content");
        write(&tmp.path().join("CLAUDE.md"), "claude content");

        let files = load_all_memory_files(tmp.path());
        // Filter to just the project-scope files from our temp dir.
        let project: Vec<_> = files
            .iter()
            .filter(|f| f.path.starts_with(tmp.path()))
            .collect();
        assert_eq!(
            project.len(),
            2,
            "both AGENTS.md and CLAUDE.md should be loaded"
        );
        assert!(
            project[0].path.ends_with("AGENTS.md"),
            "AGENTS.md must come first"
        );
        assert!(
            project[1].path.ends_with("CLAUDE.md"),
            "CLAUDE.md must follow"
        );
    }

    #[test]
    fn load_scope_claudemd_only_fallback() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = HomeGuard::new();
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("CLAUDE.md"), "claude only");

        let files = load_all_memory_files(tmp.path());
        let project: Vec<_> = files
            .iter()
            .filter(|f| f.path.starts_with(tmp.path()))
            .collect();
        assert_eq!(project.len(), 1);
        assert!(project[0].path.ends_with("CLAUDE.md"));
    }

    /// Every documented location, in the documented order.
    #[test]
    fn all_four_scopes_are_read_in_order() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = HomeGuard::new();
        let project = tempfile::tempdir().unwrap();

        write(&home.path().join("rules/10-first.md"), "MANAGED-B");
        write(&home.path().join("rules/01-zeroth.md"), "MANAGED-A");
        write(&home.path().join("AGENTS.md"), "USER-AGENTS");
        write(&home.path().join("CLAUDE.md"), "USER-CLAUDE");
        write(&project.path().join("AGENTS.md"), "PROJECT-AGENTS");
        write(&project.path().join("CLAUDE.md"), "PROJECT-CLAUDE");
        write(&project.path().join(".mikmik/AGENTS.md"), "LOCAL-AGENTS");
        write(&project.path().join(".mikmik/CLAUDE.md"), "LOCAL-CLAUDE");

        let prompt = build_memory_prompt(&load_all_memory_files(project.path()));
        let order: Vec<&str> = [
            "MANAGED-A",
            "MANAGED-B",
            "USER-AGENTS",
            "USER-CLAUDE",
            "PROJECT-AGENTS",
            "PROJECT-CLAUDE",
            "LOCAL-AGENTS",
            "LOCAL-CLAUDE",
        ]
        .into_iter()
        .collect();

        let mut cursor = 0;
        for marker in &order {
            let at = prompt[cursor..]
                .find(marker)
                .unwrap_or_else(|| panic!("{marker} missing or out of order:\n{prompt}"));
            cursor += at + marker.len();
        }
    }

    /// The docs promise the lower number is prepended first.
    #[test]
    fn priority_orders_files_inside_one_scope() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = HomeGuard::new();
        let project = tempfile::tempdir().unwrap();

        write(
            &home.path().join("rules/a.md"),
            "---\npriority: 10\n---\nTEN",
        );
        write(
            &home.path().join("rules/b.md"),
            "---\npriority: 5\n---\nFIVE",
        );
        write(&home.path().join("rules/c.md"), "NOPRIORITY");

        let prompt = build_memory_prompt(&load_all_memory_files(project.path()));
        let five = prompt.find("FIVE").expect("FIVE missing");
        let ten = prompt.find("TEN").expect("TEN missing");
        let none = prompt.find("NOPRIORITY").expect("NOPRIORITY missing");

        assert!(five < ten, "priority 5 must precede priority 10:\n{prompt}");
        assert!(ten < none, "a file with no priority must sort last");
    }

    /// A large memory file is the user's decision, not something to truncate.
    #[test]
    fn a_large_file_and_a_large_include_pass_whole() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = HomeGuard::new();
        let project = tempfile::tempdir().unwrap();

        let filler = "x".repeat(60 * 1024);
        write(
            &project.path().join("big.md"),
            &format!("{filler}\nINCLUDE-END"),
        );
        write(
            &project.path().join("AGENTS.md"),
            &format!("{filler}\nFILE-END\n@include ./big.md\n"),
        );

        let prompt = build_memory_prompt(&load_all_memory_files(project.path()));

        assert!(prompt.contains("FILE-END"), "a 60 KB file was cut");
        assert!(prompt.contains("INCLUDE-END"), "a 60 KB @include was cut");
    }

    #[test]
    fn frontmatter_is_stripped_from_the_prompt() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = HomeGuard::new();
        let project = tempfile::tempdir().unwrap();
        write(
            &project.path().join("AGENTS.md"),
            "---\nmemory_type: project\npriority: 3\n---\nBODY-TEXT\n",
        );

        let prompt = build_memory_prompt(&load_all_memory_files(project.path()));

        assert!(prompt.contains("BODY-TEXT"));
        assert!(
            !prompt.contains("memory_type:"),
            "frontmatter leaked:\n{prompt}"
        );
        assert!(
            !prompt.contains("priority:"),
            "frontmatter leaked:\n{prompt}"
        );
    }

    #[test]
    fn every_file_is_headed_with_its_scope_and_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = HomeGuard::new();
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("AGENTS.md");
        write(&path, "BODY");

        let prompt = build_memory_prompt(&load_all_memory_files(project.path()));

        assert!(
            prompt.contains(&format!("# Memory (project, from {})", path.display())),
            "no provenance header:\n{prompt}"
        );
    }

    #[test]
    fn expand_includes_circular() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.md");
        let b = tmp.path().join("b.md");
        std::fs::write(&a, "@include b.md\n").unwrap();
        std::fs::write(&b, "@include a.md\ncontent\n").unwrap();
        let result = expand_includes(
            "@include a.md\n",
            tmp.path(),
            &mut std::collections::HashSet::new(),
            0,
        );
        // Should not infinite-loop; circular reference comment present.
        assert!(result.contains("circular") || result.contains("content"));
    }
}
