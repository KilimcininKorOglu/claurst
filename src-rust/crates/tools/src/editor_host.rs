//! A client that hosts the files and the shell a tool works with.
//!
//! Normally a tool reads from disk and runs its own shell. When an editor is
//! driving the session, neither is right: the file the user is looking at may
//! have unsaved edits that are not on disk, a write that bypasses the editor
//! never enters its undo stack, and a command run in this process runs
//! somewhere the user cannot see.
//!
//! The trait mirrors `PermissionHandler`: declared here so tools can reach it,
//! implemented by whichever front end can answer. `ToolContext::editor` is
//! `None` everywhere else, and every tool falls back to disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

/// A shell the client is running on the agent's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalId(pub String);

/// What a hosted command has produced so far, and how it ended.
#[derive(Debug, Clone)]
pub struct TerminalOutput {
    pub output: String,
    /// Whether the output was cut to stay under the byte limit.
    pub truncated: bool,
    pub exit_code: Option<i32>,
    /// The signal that killed it, if one did.
    pub signal: Option<String>,
}

/// How a command should be started.
#[derive(Debug, Clone)]
pub struct TerminalRequest {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    /// How much output to keep. Beyond it the client drops the oldest and
    /// says so, rather than growing without bound.
    pub output_byte_limit: Option<u64>,
}

/// Whichever of these the client can do. A capability it does not have is
/// answered by falling back to the local path, never by failing the tool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditorCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
    pub terminal: bool,
}

#[async_trait]
pub trait EditorHost: Send + Sync {
    /// What the client can do. Read before every call, because a tool must
    /// not ask for something the client never offered.
    fn capabilities(&self) -> EditorCapabilities;

    /// The file as the client has it, unsaved edits included.
    async fn read_text_file(&self, path: &Path) -> std::io::Result<String>;

    /// Write through the client, so the change lands in its undo stack.
    async fn write_text_file(&self, path: &Path, contents: &str) -> std::io::Result<()>;

    /// Start a command in a shell the client owns and shows.
    async fn create_terminal(&self, request: TerminalRequest) -> std::io::Result<TerminalId>;

    /// Wait for it to finish, then report how it ended.
    async fn wait_for_terminal_exit(&self, id: &TerminalId) -> std::io::Result<TerminalOutput>;

    /// Everything it has written so far, without waiting.
    async fn terminal_output(&self, id: &TerminalId) -> std::io::Result<TerminalOutput>;

    /// Stop it without letting go of it, so its output can still be read.
    async fn kill_terminal(&self, id: &TerminalId) -> std::io::Result<()>;

    /// Let go of it. After this its id means nothing.
    async fn release_terminal(&self, id: &TerminalId) -> std::io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::allow_all_context;
    use std::sync::Arc;

    /// A client that answers reads and writes from memory, so a test can tell
    /// a bridged call from a local one by what came back.
    struct FakeEditor {
        capabilities: EditorCapabilities,
        files: parking_lot::Mutex<HashMap<PathBuf, String>>,
    }

    impl FakeEditor {
        fn with(capabilities: EditorCapabilities) -> Arc<Self> {
            Arc::new(Self {
                capabilities,
                files: parking_lot::Mutex::new(HashMap::new()),
            })
        }
    }

    #[async_trait]
    impl EditorHost for FakeEditor {
        fn capabilities(&self) -> EditorCapabilities {
            self.capabilities
        }

        async fn read_text_file(&self, path: &Path) -> std::io::Result<String> {
            self.files
                .lock()
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such buffer"))
        }

        async fn write_text_file(&self, path: &Path, contents: &str) -> std::io::Result<()> {
            self.files
                .lock()
                .insert(path.to_path_buf(), contents.to_string());
            Ok(())
        }

        async fn create_terminal(&self, _request: TerminalRequest) -> std::io::Result<TerminalId> {
            unimplemented!("not exercised here")
        }

        async fn wait_for_terminal_exit(
            &self,
            _id: &TerminalId,
        ) -> std::io::Result<TerminalOutput> {
            unimplemented!("not exercised here")
        }

        async fn terminal_output(&self, _id: &TerminalId) -> std::io::Result<TerminalOutput> {
            unimplemented!("not exercised here")
        }

        async fn kill_terminal(&self, _id: &TerminalId) -> std::io::Result<()> {
            unimplemented!("not exercised here")
        }

        async fn release_terminal(&self, _id: &TerminalId) -> std::io::Result<()> {
            unimplemented!("not exercised here")
        }
    }

    const HOSTED: EditorCapabilities = EditorCapabilities {
        read_text_file: true,
        write_text_file: true,
        terminal: true,
    };

    #[tokio::test]
    async fn with_no_client_a_read_comes_from_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "on disk").await.expect("seed");

        let ctx = allow_all_context(dir.path().to_path_buf());
        assert_eq!(ctx.read_text(&path).await.expect("reads"), "on disk");
    }

    #[tokio::test]
    async fn with_a_client_a_read_sees_what_the_user_is_looking_at() {
        // The whole point: the buffer has edits that were never saved.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "on disk").await.expect("seed");

        let editor = FakeEditor::with(HOSTED);
        editor
            .write_text_file(&path, "unsaved")
            .await
            .expect("stage the buffer");

        let mut ctx = allow_all_context(dir.path().to_path_buf());
        ctx.editor = Some(editor);

        assert_eq!(ctx.read_text(&path).await.expect("reads"), "unsaved");
    }

    #[tokio::test]
    async fn a_capability_the_client_lacks_falls_back_rather_than_failing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "on disk").await.expect("seed");

        let mut ctx = allow_all_context(dir.path().to_path_buf());
        ctx.editor = Some(FakeEditor::with(EditorCapabilities::default()));

        // The fake has no buffer for this path, so a bridged read would fail.
        assert_eq!(ctx.read_text(&path).await.expect("reads"), "on disk");

        ctx.write_text(&path, b"written locally")
            .await
            .expect("writes");
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("reads back"),
            "written locally"
        );
    }

    #[tokio::test]
    async fn each_capability_is_honoured_on_its_own() {
        // A client that reads but does not write must not have its writes
        // bridged, and the other way round.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("a.txt");
        tokio::fs::write(&path, "on disk").await.expect("seed");

        let editor = FakeEditor::with(EditorCapabilities {
            read_text_file: true,
            write_text_file: false,
            terminal: false,
        });
        editor
            .write_text_file(&path, "unsaved")
            .await
            .expect("stage the buffer");

        let mut ctx = allow_all_context(dir.path().to_path_buf());
        ctx.editor = Some(editor.clone());

        assert_eq!(ctx.read_text(&path).await.expect("reads"), "unsaved");
        ctx.write_text(&path, b"to disk").await.expect("writes");
        assert_eq!(
            tokio::fs::read_to_string(&path).await.expect("reads back"),
            "to disk",
            "the write must not have gone to the client"
        );
    }

    #[tokio::test]
    async fn binary_content_is_written_to_disk_even_with_a_client() {
        // The client's write carries text; handing it bytes would lose them.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("logo.png");

        let editor = FakeEditor::with(HOSTED);
        let mut ctx = allow_all_context(dir.path().to_path_buf());
        ctx.editor = Some(editor.clone());

        ctx.write_text(&path, &[0xff, 0xfe, 0x00])
            .await
            .expect("writes");

        assert_eq!(
            tokio::fs::read(&path).await.expect("reads back"),
            vec![0xff, 0xfe, 0x00]
        );
        assert!(
            editor.files.lock().is_empty(),
            "no bytes reached the client"
        );
    }
}
