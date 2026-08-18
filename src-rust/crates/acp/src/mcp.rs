//! MCP servers a client asked one session to use.
//!
//! `session/new`, `load`, `resume` and `fork` all accept a list of servers.
//! They belong to that session alone: a client that opens one panel against a
//! project's servers and another without them must get two different tool
//! rosters, so the connection and the roster it produces are held by the
//! session rather than the runtime.

use std::sync::Arc;

use agent_client_protocol_schema as acp;
use claurst_core::config::{McpServerConfig, McpServerOrigin};
use claurst_tools::Tool;
use tracing::{info, warn};

/// One session's own MCP connection, and the tools it added.
#[derive(Clone)]
pub struct SessionMcp {
    pub manager: Arc<claurst_mcp::McpManager>,
    pub tools: Arc<Vec<Box<dyn Tool>>>,
}

/// Connect the servers a request named, and build the roster they belong to.
///
/// `None` when the request named none, which is what a client that relies on
/// the agent's own configuration sends: that session shares the runtime's
/// roster rather than being given an empty one.
pub async fn connect(
    servers: &[acp::McpServer],
    advisor_model: Option<&str>,
) -> Result<Option<SessionMcp>, acp::Error> {
    if servers.is_empty() {
        return Ok(None);
    }

    let mut configs = Vec::with_capacity(servers.len());
    for server in servers {
        configs.push(to_config(server).map_err(|reason| {
            acp::Error::invalid_params().data(Some(serde_json::json!({
                "reason": reason,
            })))
        })?);
    }

    // These came from the client the user is driving, not from a repository
    // this process opened, so they are user-origin. The gate still runs, so
    // the invariant holds if that origin ever stops being true.
    let store = claurst_core::mcp_trust::McpTrustStore::load();
    let decision = claurst_core::mcp_trust::partition_mcp_servers(
        &configs,
        None,
        false,
        &std::collections::HashSet::new(),
        &store,
    );
    if !decision.pending.is_empty() {
        let names: Vec<&str> = decision.pending.iter().map(|s| s.name.as_str()).collect();
        warn!(servers = ?names, "ACP: skipping untrusted session MCP server(s)");
    }
    if decision.allowed.is_empty() {
        return Ok(None);
    }

    let manager = Arc::new(claurst_mcp::McpManager::connect_all(&decision.allowed).await);
    manager.clone().spawn_notification_poll_loop();
    let tools = claurst_query::build_tool_roster(Some(manager.clone()), advisor_model);
    info!(
        servers = decision.allowed.len(),
        tools = tools.len(),
        "ACP: session MCP servers connected"
    );
    Ok(Some(SessionMcp { manager, tools }))
}

/// Read one protocol server definition as the internal one.
///
/// An http or sse server carrying headers is refused rather than connected
/// without them: the internal config has nowhere to put a header, and an
/// Authorization header dropped on the way would surface as an unexplained
/// authentication failure much later.
fn to_config(server: &acp::McpServer) -> Result<McpServerConfig, String> {
    match server {
        acp::McpServer::Stdio(stdio) => Ok(McpServerConfig {
            name: stdio.name.clone(),
            command: Some(stdio.command.display().to_string()),
            args: stdio.args.clone(),
            env: stdio
                .env
                .iter()
                .map(|var| (var.name.clone(), var.value.clone()))
                .collect(),
            url: None,
            server_type: "stdio".to_string(),
            origin: McpServerOrigin::User,
        }),
        acp::McpServer::Http(http) => {
            if !http.headers.is_empty() {
                return Err(format!(
                    "MCP server '{}': headers are not supported; configure the server's \
                     credentials in the agent instead",
                    http.name
                ));
            }
            Ok(McpServerConfig {
                name: http.name.clone(),
                command: None,
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                url: Some(http.url.clone()),
                server_type: "http".to_string(),
                origin: McpServerOrigin::User,
            })
        }
        acp::McpServer::Sse(sse) => {
            if !sse.headers.is_empty() {
                return Err(format!(
                    "MCP server '{}': headers are not supported; configure the server's \
                     credentials in the agent instead",
                    sse.name
                ));
            }
            Ok(McpServerConfig {
                name: sse.name.clone(),
                command: None,
                args: Vec::new(),
                url: Some(sse.url.clone()),
                env: std::collections::HashMap::new(),
                server_type: "sse".to_string(),
                origin: McpServerOrigin::User,
            })
        }
        other => Err(format!(
            "unsupported MCP transport: {}",
            serde_json::to_value(other)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string())
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stdio_server_keeps_its_command_and_environment() {
        let server = acp::McpServer::Stdio(
            acp::McpServerStdio::new("docs", std::path::PathBuf::from("/usr/bin/npx"))
                .args(vec!["-y".to_string(), "@some/server".to_string()])
                .env(vec![acp::EnvVariable::new("TOKEN", "abc")]),
        );

        let config = to_config(&server).expect("a stdio server converts");
        assert_eq!(config.name, "docs");
        assert_eq!(config.command.as_deref(), Some("/usr/bin/npx"));
        assert_eq!(config.args, vec!["-y", "@some/server"]);
        assert_eq!(config.env.get("TOKEN").map(String::as_str), Some("abc"));
        assert_eq!(config.server_type, "stdio");
    }

    #[test]
    fn an_http_server_keeps_its_url() {
        let server =
            acp::McpServer::Http(acp::McpServerHttp::new("api", "https://example.com/mcp"));

        let config = to_config(&server).expect("an http server converts");
        assert_eq!(config.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(config.server_type, "http");
        assert!(config.command.is_none());
    }

    #[test]
    fn an_sse_server_is_named_as_one() {
        let server =
            acp::McpServer::Sse(acp::McpServerSse::new("events", "https://example.com/sse"));
        assert_eq!(
            to_config(&server)
                .expect("an sse server converts")
                .server_type,
            "sse"
        );
    }

    #[test]
    fn headers_are_refused_rather_than_quietly_dropped() {
        // Connecting without the Authorization header the client supplied
        // would fail later, somewhere that says nothing about the header.
        let server = acp::McpServer::Http(
            acp::McpServerHttp::new("api", "https://example.com/mcp")
                .headers(vec![acp::HttpHeader::new("Authorization", "Bearer x")]),
        );

        let Err(reason) = to_config(&server) else {
            panic!("a header must not be dropped");
        };
        assert!(reason.contains("headers"), "{reason}");
        assert!(reason.contains("api"), "{reason}");
    }

    #[tokio::test]
    async fn a_session_that_named_no_servers_shares_the_agents_roster() {
        assert!(connect(&[], None)
            .await
            .expect("no servers is fine")
            .is_none());
    }

    #[tokio::test]
    async fn a_bad_server_definition_is_reported_rather_than_skipped() {
        let server = acp::McpServer::Http(
            acp::McpServerHttp::new("api", "https://example.com/mcp")
                .headers(vec![acp::HttpHeader::new("Authorization", "Bearer x")]),
        );

        assert!(connect(std::slice::from_ref(&server), None).await.is_err());
    }
}
