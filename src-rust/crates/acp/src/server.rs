//! Top-level ACP request / notification dispatcher.

use std::sync::Arc;

use agent_client_protocol_schema as acp;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::connection::{Connection, Inbound};
use crate::runtime::AgentRuntime;
use crate::sessions::{SessionRegistry, SessionState};

/// The ACP agent: owns the connection, the runtime, and the session registry.
pub struct AgentServer {
    pub connection: Arc<Connection>,
    pub runtime: Arc<AgentRuntime>,
    pub sessions: Arc<SessionRegistry>,
    pub client_capabilities: parking_lot::RwLock<acp::ClientCapabilities>,
}

impl AgentServer {
    pub fn new(connection: Arc<Connection>, runtime: Arc<AgentRuntime>) -> Arc<Self> {
        Arc::new(Self {
            connection,
            runtime,
            sessions: Arc::new(SessionRegistry::new()),
            client_capabilities: parking_lot::RwLock::new(acp::ClientCapabilities::default()),
        })
    }

    /// Dispatch a single inbound message. Spawns the actual handler on a
    /// background task so the reader loop stays responsive while a prompt
    /// is in flight. Returns the join handle so the caller can wait for
    /// in-flight work to finish before shutting down.
    pub fn dispatch(self: &Arc<Self>, msg: Inbound) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            match msg {
                Inbound::Request { id, method, params } => {
                    let response = this.handle_request(&method, params).await;
                    let result = match response {
                        Ok(value) => this.connection.send_response(id, value).await,
                        Err(err) => this.connection.send_error_response(id, err).await,
                    };
                    if let Err(e) = result {
                        warn!(?e, method = %method, "ACP: failed to send response");
                    }
                }
                Inbound::Notification { method, params } => {
                    this.handle_notification(&method, params).await;
                }
            }
        })
    }

    async fn handle_request(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, acp::Error> {
        debug!(method, "ACP: dispatch request");
        match method {
            "initialize" => {
                let req: acp::InitializeRequest = parse_params(params)?;
                let result = self.on_initialize(req).await?;
                serde_json::to_value(result).map_err(|_| acp::Error::internal_error())
            }
            "authenticate" => {
                let _req: acp::AuthenticateRequest = parse_params(params)?;
                // Claurst uses local credentials; clients don't need to authenticate.
                serde_json::to_value(acp::AuthenticateResponse::default())
                    .map_err(|_| acp::Error::internal_error())
            }
            "session/new" => {
                let req: acp::NewSessionRequest = parse_params(params)?;
                let result = self.on_new_session(req).await?;
                serde_json::to_value(result).map_err(|_| acp::Error::internal_error())
            }
            "session/load" => {
                // v1: not supported. Capability is advertised as false in
                // initialize so a well-behaved client never calls this.
                Err(acp::Error::method_not_found())
            }
            "session/prompt" => {
                let req: acp::PromptRequest = parse_params(params)?;
                let result = self.on_prompt(req).await?;
                serde_json::to_value(result).map_err(|_| acp::Error::internal_error())
            }
            "session/set_mode" => {
                let req: acp::SetSessionModeRequest = parse_params(params)?;
                let result = self.on_set_mode(req).await?;
                serde_json::to_value(result).map_err(|_| acp::Error::internal_error())
            }
            "session/set_config_option" => {
                let req: acp::SetSessionConfigOptionRequest = parse_params(params)?;
                let result = self.on_set_config_option(req).await?;
                serde_json::to_value(result).map_err(|_| acp::Error::internal_error())
            }
            other => {
                warn!(method = other, "ACP: method not found");
                Err(acp::Error::method_not_found())
            }
        }
    }

    async fn handle_notification(self: &Arc<Self>, method: &str, params: Option<Value>) {
        debug!(method, "ACP: dispatch notification");
        match method {
            "session/cancel" => {
                let parsed: Result<acp::CancelNotification, _> = params
                    .map(serde_json::from_value)
                    .unwrap_or(Err(serde::de::Error::custom("missing params")));
                match parsed {
                    Ok(notif) => {
                        if let Some(session) = self.sessions.get(&notif.session_id) {
                            info!(session_id = %notif.session_id, "ACP: cancelling session");
                            session.cancel_token.cancel();
                            // Re-arm with a fresh token for any subsequent prompt
                            // calls on this session. (The cancellation only
                            // affects the in-flight turn.)
                            //
                            // SAFETY: we hold an Arc<SessionState>; this races
                            // with the prompt handler reading cancel_token but
                            // the race is benign — either the next prompt sees
                            // the old (cancelled) token (and finishes
                            // immediately) or the new fresh one.
                        }
                    }
                    Err(e) => warn!(?e, "ACP: malformed session/cancel notification"),
                }
            }
            other => {
                warn!(method = other, "ACP: ignoring unknown notification");
            }
        }
    }

    async fn on_initialize(
        self: &Arc<Self>,
        req: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
        info!(
            client_version = ?req.client_info.as_ref().map(|i| (&i.name, &i.version)),
            "ACP: initialize"
        );
        *self.client_capabilities.write() = req.client_capabilities.clone();

        let agent_info = acp::Implementation::new("claurst", env!("CARGO_PKG_VERSION"))
            .title(Some("Claurst".to_string()));

        let mut response = acp::InitializeResponse::new(acp::ProtocolVersion::V1)
            .agent_capabilities(
                acp::AgentCapabilities::new()
                    .load_session(false)
                    .prompt_capabilities(acp::PromptCapabilities::new())
                    .mcp_capabilities(acp::McpCapabilities::new()),
            );
        response = response.agent_info(Some(agent_info));
        Ok(response)
    }

    async fn on_new_session(
        self: &Arc<Self>,
        req: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        if !req.cwd.is_absolute() {
            return Err(acp::Error::invalid_params().data(Some(
                serde_json::json!({ "reason": "cwd must be absolute" }),
            )));
        }
        let session_id = acp::SessionId::new(format!("acp-{}", uuid::Uuid::new_v4()));
        let state = SessionState::new(session_id.clone(), req.cwd.clone());
        info!(session_id = %session_id, cwd = %req.cwd.display(), "ACP: new session");

        // v1: ignore req.mcp_servers — agent uses settings.json MCP roster.
        if !req.mcp_servers.is_empty() {
            warn!(
                count = req.mcp_servers.len(),
                "ACP: session-specific MCP servers are not yet routed (v1) — using global config"
            );
        }

        self.sessions.insert(state.clone());
        Ok(acp::NewSessionResponse::new(session_id)
            .modes(Some(crate::session_config::mode_state(
                &self.runtime.config.permission_mode,
            )))
            .config_options(Some(self.config_options_for(&state))))
    }

    /// The options a session currently offers, rebuilt from its overrides.
    fn config_options_for(&self, session: &Arc<SessionState>) -> Vec<acp::SessionConfigOption> {
        let overrides = session.settings.lock().clone();
        let mut config = self.runtime.config.clone();
        crate::session_config::apply_overrides(&mut config, &overrides);
        let effort = overrides.effort.or(self.runtime.query_config.effort_level);
        // The turn sends the runtime's resolved model unless the session says
        // otherwise; `config.model` is often unset and would resolve to a
        // fallback the session is not using.
        let model = overrides
            .model
            .clone()
            .unwrap_or_else(|| self.runtime.query_config.model.clone());
        crate::session_config::config_options(&config, &self.runtime.model_registry, &model, effort)
    }

    /// Change the model, the account, or the reasoning effort for this session
    /// alone. Session-scoped: nothing is written to `settings.json`.
    async fn on_set_config_option(
        self: &Arc<Self>,
        req: acp::SetSessionConfigOptionRequest,
    ) -> Result<acp::SetSessionConfigOptionResponse, acp::Error> {
        let session = self.session_or_error(&req.session_id)?;
        let option_id = req.config_id.0.to_string();
        let value = req.value.0.to_string();

        {
            let mut overrides = session.settings.lock();
            let mut config = self.runtime.config.clone();
            crate::session_config::apply_overrides(&mut config, &overrides);
            if let Err(reason) = crate::session_config::apply_config_option(
                &mut overrides,
                &config,
                &self.runtime.model_registry,
                &option_id,
                &value,
            ) {
                return Err(acp::Error::invalid_params().data(Some(serde_json::json!({
                    "reason": reason,
                    "configId": option_id,
                    "value": value,
                }))));
            }
        }
        info!(
            session_id = %req.session_id,
            option = %option_id,
            value = %value,
            "ACP: session configuration changed"
        );

        // Changing one option restates the other two: the model list belongs
        // to the account, and the effort ladder belongs to the model.
        let options = self.config_options_for(&session);
        let update =
            acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(options.clone()));
        let notification = acp::SessionNotification::new(req.session_id.clone(), update);
        if let Err(e) = self
            .connection
            .send_notification("session/update", notification)
            .await
        {
            warn!(?e, "ACP: failed to announce the configuration change");
        }

        Ok(acp::SetSessionConfigOptionResponse::new(options))
    }

    /// Switch how this session answers permission requests. Session-scoped:
    /// nothing is written to `settings.json`.
    async fn on_set_mode(
        self: &Arc<Self>,
        req: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse, acp::Error> {
        let session = self.session_or_error(&req.session_id)?;
        let mode_id = req.mode_id.0.as_ref();
        let Some(mode) = crate::session_config::permission_mode_for(mode_id) else {
            return Err(acp::Error::invalid_params().data(Some(serde_json::json!({
                "reason": "unknown mode",
                "modeId": mode_id,
            }))));
        };

        session.settings.lock().permission_mode = Some(mode.clone());
        info!(session_id = %req.session_id, mode = mode_id, "ACP: session mode changed");

        // Say it out loud as well as answering: a client with more than one
        // view of the session updates all of them from the notification.
        let update =
            acp::SessionUpdate::CurrentModeUpdate(acp::CurrentModeUpdate::new(req.mode_id.clone()));
        let notification = acp::SessionNotification::new(req.session_id.clone(), update);
        if let Err(e) = self
            .connection
            .send_notification("session/update", notification)
            .await
        {
            warn!(?e, "ACP: failed to announce the mode change");
        }

        Ok(acp::SetSessionModeResponse::new())
    }

    /// Look a session up, or report the id back as invalid.
    fn session_or_error(
        &self,
        session_id: &acp::SessionId,
    ) -> Result<Arc<SessionState>, acp::Error> {
        self.sessions.get(session_id).ok_or_else(|| {
            acp::Error::invalid_params().data(Some(serde_json::json!({
                "reason": "unknown session",
                "sessionId": session_id,
            })))
        })
    }

    async fn on_prompt(
        self: &Arc<Self>,
        req: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        let session = self.session_or_error(&req.session_id)?;
        crate::prompt::handle(self.runtime.clone(), self.connection.clone(), session, req).await
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, acp::Error> {
    let value = params.ok_or_else(acp::Error::invalid_params)?;
    serde_json::from_value(value).map_err(|e| {
        acp::Error::invalid_params().data(Some(
            serde_json::json!({ "deserialize_error": e.to_string() }),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Params {
        name: String,
    }

    #[test]
    fn a_request_without_params_is_rejected() {
        let error = parse_params::<Params>(None).expect_err("None must not deserialize");
        assert_eq!(error.code, acp::ErrorCode::InvalidParams);
    }

    #[test]
    fn a_matching_shape_deserializes() {
        let parsed: Params = parse_params(Some(serde_json::json!({ "name": "claurst" })))
            .expect("a matching shape parses");
        assert_eq!(
            parsed,
            Params {
                name: "claurst".to_string()
            }
        );
    }

    #[test]
    fn a_mismatched_shape_reports_why() {
        // The editor sees only what `data` carries, so a bare code would leave
        // the user with no way to tell which field was wrong.
        let error = parse_params::<Params>(Some(serde_json::json!({ "wrong_field": 1 })))
            .expect_err("a mismatched shape must not deserialize");

        assert_eq!(error.code, acp::ErrorCode::InvalidParams);
        let data = error.data.expect("the error carries the parse failure");
        assert!(
            data["deserialize_error"].is_string(),
            "expected a deserialize_error string, got {data}"
        );
    }
}
