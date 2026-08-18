//! The connected client, as the thing that hosts this session's files and
//! shell.
//!
//! Implements `claurst_tools::EditorHost` over the ACP connection, so a tool
//! reads the buffer the user is looking at rather than the older text on disk,
//! and writes through the editor so the change joins its undo stack.
//!
//! Every capability is checked before it is used: what the client declared in
//! `initialize` is the whole contract, and a tool that asked for more would be
//! answered with an error the user cannot act on.

use std::path::Path;
use std::sync::Arc;

use agent_client_protocol_schema as acp;
use async_trait::async_trait;
use claurst_tools::{EditorCapabilities, EditorHost, TerminalId, TerminalOutput, TerminalRequest};

use crate::connection::Connection;

/// One session's view of the client that opened it.
pub struct AcpEditorHost {
    connection: Arc<Connection>,
    session_id: acp::SessionId,
    capabilities: EditorCapabilities,
    /// The tool call whose terminals these are, so a client can draw the live
    /// output under the call that started it rather than on its own.
    tool_call: Option<acp::ToolCallId>,
}

impl AcpEditorHost {
    /// Build a host for this session, or `None` when the client hosts nothing:
    /// a context with no editor is exactly what the local path already is, so
    /// there is nothing to be gained by routing through an empty one.
    pub fn for_session(
        connection: Arc<Connection>,
        session_id: acp::SessionId,
        client: &acp::ClientCapabilities,
    ) -> Option<Arc<dyn EditorHost>> {
        let capabilities = capabilities_of(client);
        if capabilities == EditorCapabilities::default() {
            return None;
        }
        Some(Arc::new(Self {
            connection,
            session_id,
            capabilities,
            tool_call: None,
        }))
    }

    /// The same host, for one tool call.
    ///
    /// A tool is handed this copy so the terminal it starts is announced under
    /// its own call; the session's copy announces nothing, because a terminal
    /// belonging to no call has nowhere to be drawn.
    fn for_call(&self, tool_call: acp::ToolCallId) -> Self {
        Self {
            connection: self.connection.clone(),
            session_id: self.session_id.clone(),
            capabilities: self.capabilities,
            tool_call: Some(tool_call),
        }
    }

    /// Tell the client which call this terminal belongs to.
    ///
    /// Sent while the terminal is still alive: the protocol requires the
    /// update to precede `terminal/release`, or the client is left holding an
    /// id it can no longer read.
    async fn announce_terminal(&self, terminal: &acp::TerminalId) {
        let Some(tool_call) = &self.tool_call else {
            return;
        };
        let update = acp::ToolCallUpdate::new(
            tool_call.clone(),
            acp::ToolCallUpdateFields::new().content(Some(vec![acp::ToolCallContent::Terminal(
                acp::Terminal::new(terminal.clone()),
            )])),
        );
        let notification = acp::SessionNotification::new(
            self.session_id.clone(),
            acp::SessionUpdate::ToolCallUpdate(update),
        );
        if let Err(e) = self
            .connection
            .send_notification("session/update", notification)
            .await
        {
            tracing::warn!(?e, "ACP: could not attach a terminal to its tool call");
        }
    }

    /// One client call, with both failure kinds reported as io errors so a
    /// tool can treat them the way it treats a failed read.
    async fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> std::io::Result<R> {
        match self.connection.send_request::<P, R>(method, params).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(err)) => Err(std::io::Error::other(format!(
                "{method} was refused by the client: {}",
                err.message
            ))),
            Err(err) => Err(std::io::Error::other(format!(
                "{method} did not reach the client: {err}"
            ))),
        }
    }
}

/// What the client said it can do, in the tools' terms.
fn capabilities_of(client: &acp::ClientCapabilities) -> EditorCapabilities {
    EditorCapabilities {
        read_text_file: client.fs.read_text_file,
        write_text_file: client.fs.write_text_file,
        terminal: client.terminal,
    }
}

#[async_trait]
impl EditorHost for AcpEditorHost {
    fn capabilities(&self) -> EditorCapabilities {
        self.capabilities
    }

    fn for_tool_call(&self, tool_call_id: &str) -> Option<Arc<dyn EditorHost>> {
        Some(Arc::new(self.for_call(acp::ToolCallId::new(tool_call_id))))
    }

    async fn read_text_file(&self, path: &Path) -> std::io::Result<String> {
        let response: acp::ReadTextFileResponse = self
            .call(
                "fs/read_text_file",
                acp::ReadTextFileRequest::new(self.session_id.clone(), path.to_path_buf()),
            )
            .await?;
        Ok(response.content)
    }

    async fn write_text_file(&self, path: &Path, contents: &str) -> std::io::Result<()> {
        let _: acp::WriteTextFileResponse = self
            .call(
                "fs/write_text_file",
                acp::WriteTextFileRequest::new(
                    self.session_id.clone(),
                    path.to_path_buf(),
                    contents.to_string(),
                ),
            )
            .await?;
        Ok(())
    }

    async fn create_terminal(&self, request: TerminalRequest) -> std::io::Result<TerminalId> {
        let mut params =
            acp::CreateTerminalRequest::new(self.session_id.clone(), request.command.clone())
                .args(request.args.clone())
                .env(
                    request
                        .env
                        .iter()
                        .map(|(name, value)| acp::EnvVariable::new(name.clone(), value.clone()))
                        .collect::<Vec<_>>(),
                );
        if let Some(cwd) = &request.cwd {
            params = params.cwd(Some(cwd.clone()));
        }
        if let Some(limit) = request.output_byte_limit {
            params = params.output_byte_limit(Some(limit));
        }

        let response: acp::CreateTerminalResponse = self.call("terminal/create", params).await?;
        self.announce_terminal(&response.terminal_id).await;
        Ok(TerminalId(response.terminal_id.0.to_string()))
    }

    async fn wait_for_terminal_exit(&self, id: &TerminalId) -> std::io::Result<TerminalOutput> {
        let exit: acp::WaitForTerminalExitResponse = self
            .call(
                "terminal/wait_for_exit",
                acp::WaitForTerminalExitRequest::new(
                    self.session_id.clone(),
                    acp::TerminalId::new(id.0.as_str()),
                ),
            )
            .await?;
        // The exit status says how it ended but not what it said, so the
        // output is fetched separately rather than reported as empty.
        let mut output = self.terminal_output(id).await?;
        output.exit_code = exit.exit_status.exit_code.map(|code| code as i32);
        output.signal = exit.exit_status.signal;
        Ok(output)
    }

    async fn terminal_output(&self, id: &TerminalId) -> std::io::Result<TerminalOutput> {
        let response: acp::TerminalOutputResponse = self
            .call(
                "terminal/output",
                acp::TerminalOutputRequest::new(
                    self.session_id.clone(),
                    acp::TerminalId::new(id.0.as_str()),
                ),
            )
            .await?;
        let (exit_code, signal) = match response.exit_status {
            Some(status) => (
                status.exit_code.map(|code| code as i32),
                status.signal.clone(),
            ),
            None => (None, None),
        };
        Ok(TerminalOutput {
            output: response.output,
            truncated: response.truncated,
            exit_code,
            signal,
        })
    }

    async fn kill_terminal(&self, id: &TerminalId) -> std::io::Result<()> {
        let _: acp::KillTerminalResponse = self
            .call(
                "terminal/kill",
                acp::KillTerminalRequest::new(
                    self.session_id.clone(),
                    acp::TerminalId::new(id.0.as_str()),
                ),
            )
            .await?;
        Ok(())
    }

    async fn release_terminal(&self, id: &TerminalId) -> std::io::Result<()> {
        let _: acp::ReleaseTerminalResponse = self
            .call(
                "terminal/release",
                acp::ReleaseTerminalRequest::new(
                    self.session_id.clone(),
                    acp::TerminalId::new(id.0.as_str()),
                ),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(read: bool, write: bool, terminal: bool) -> acp::ClientCapabilities {
        acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new()
                .read_text_file(read)
                .write_text_file(write))
            .terminal(terminal)
    }

    fn connection() -> Arc<Connection> {
        Connection::new(tokio::io::sink())
    }

    #[test]
    fn a_client_that_hosts_nothing_gets_no_host_at_all() {
        // Routing through a host that can do nothing would only add a layer
        // between the tools and the disk they were already using.
        assert!(AcpEditorHost::for_session(
            connection(),
            acp::SessionId::new("acp-1"),
            &client(false, false, false),
        )
        .is_none());
    }

    #[test]
    fn each_capability_is_carried_over_on_its_own() {
        assert_eq!(
            capabilities_of(&client(true, false, false)),
            EditorCapabilities {
                read_text_file: true,
                write_text_file: false,
                terminal: false,
            }
        );
        assert_eq!(
            capabilities_of(&client(false, true, false)),
            EditorCapabilities {
                read_text_file: false,
                write_text_file: true,
                terminal: false,
            }
        );
        assert_eq!(
            capabilities_of(&client(false, false, true)),
            EditorCapabilities {
                read_text_file: false,
                write_text_file: false,
                terminal: true,
            }
        );
    }

    #[test]
    fn a_hosts_own_copy_belongs_to_no_call() {
        // It is the dispatcher that knows which call is running; a terminal
        // announced by the session's copy would have nowhere to be drawn.
        let host = AcpEditorHost {
            connection: connection(),
            session_id: acp::SessionId::new("acp-1"),
            capabilities: EditorCapabilities {
                read_text_file: false,
                write_text_file: false,
                terminal: true,
            },
            tool_call: None,
        };

        assert!(host.tool_call.is_none());
        assert_eq!(
            host.for_call(acp::ToolCallId::new("toolu_9")).tool_call,
            Some(acp::ToolCallId::new("toolu_9"))
        );
    }

    #[test]
    fn a_per_call_copy_keeps_what_the_client_can_do() {
        let host = AcpEditorHost {
            connection: connection(),
            session_id: acp::SessionId::new("acp-1"),
            capabilities: EditorCapabilities {
                read_text_file: true,
                write_text_file: false,
                terminal: true,
            },
            tool_call: None,
        };

        let per_call = host
            .for_tool_call("toolu_9")
            .expect("this host distinguishes calls");
        assert_eq!(per_call.capabilities(), host.capabilities);
    }

    #[test]
    fn a_client_that_hosts_one_thing_gets_a_host_for_it() {
        let host = AcpEditorHost::for_session(
            connection(),
            acp::SessionId::new("acp-1"),
            &client(true, false, false),
        )
        .expect("a client that reads gets a host");

        assert!(host.capabilities().read_text_file);
        assert!(!host.capabilities().write_text_file);
    }
}
