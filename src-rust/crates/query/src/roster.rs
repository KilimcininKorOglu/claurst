//! The set of tools a session runs with.
//!
//! One builder, shared by every front end. A session started from an editor
//! reads the same `settings.json` as one started from a terminal, so it must
//! end up with the same tools; building the roster in each front end is how
//! they drifted apart.

use std::sync::Arc;

use claurst_tools::Tool;
use tracing::debug;

/// Built-in tools, the sub-agent tool, the advisor when a model backs it, the
/// ACP bridge when agents are configured, and every tool the connected MCP
/// servers offer.
///
/// Takes the whole config rather than the individual fields it gates on: two
/// of the tools are already conditional and threading one more derived value
/// through six call sites buys nothing.
pub fn build_tool_roster(
    mcp_manager: Option<Arc<claurst_mcp::McpManager>>,
    config: &claurst_core::Config,
) -> Arc<Vec<Box<dyn Tool>>> {
    let mut tools: Vec<Box<dyn Tool>> = claurst_tools::all_tools();
    tools.push(Box::new(crate::AgentTool));

    // Offer the advisor only when a model backs it, so a session without one
    // pays neither the tool schema nor the system-prompt guideline for it.
    if config
        .advisor_model
        .as_deref()
        .is_some_and(|model| !model.trim().is_empty())
    {
        tools.push(Box::new(claurst_tools::AdvisorTool));
    }

    // Same reasoning for the ACP bridge: without a configured agent the tool
    // could only ever answer "nothing is configured", so offering it would
    // spend schema tokens to advertise a dead end.
    if !config.acp_agents.is_empty() {
        tools.push(Box::new(claurst_tools::AcpAgentTool));
    }

    if let Some(manager) = &mcp_manager {
        tools.extend(claurst_tools::mcp_tools(manager));
        debug!(total_tools = tools.len(), "MCP tools registered");
    }

    Arc::new(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    use claurst_core::Config;

    fn names(tools: &[Box<dyn Tool>]) -> Vec<&str> {
        tools.iter().map(|t| t.name()).collect()
    }

    fn with_advisor(model: Option<&str>) -> Config {
        Config {
            advisor_model: model.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_session_always_gets_the_built_ins_and_the_sub_agent_tool() {
        let config = Config::default();
        let tools = build_tool_roster(None, &config);
        let names = names(&tools);

        assert!(names.contains(&"Bash"), "{names:?}");
        assert!(names.contains(&"Read"), "{names:?}");
        assert!(names.contains(&"Agent"), "{names:?}");
    }

    #[test]
    fn the_advisor_is_offered_only_when_a_model_backs_it() {
        assert!(!names(&build_tool_roster(None, &with_advisor(None))).contains(&"Advisor"));
        assert!(!names(&build_tool_roster(None, &with_advisor(Some("   ")))).contains(&"Advisor"));
        assert!(names(&build_tool_roster(
            None,
            &with_advisor(Some("claude-haiku-4-5"))
        ))
        .contains(&"Advisor"));
    }

    #[test]
    fn the_acp_bridge_is_offered_only_when_an_agent_is_configured() {
        let bare = Config::default();
        assert!(!names(&build_tool_roster(None, &bare)).contains(&"AcpAgent"));

        let mut configured = Config::default();
        configured.acp_agents.insert(
            "cursor".to_string(),
            claurst_core::AcpAgentConfig {
                command: "agent".to_string(),
                args: vec!["--force".to_string(), "acp".to_string()],
                env: Default::default(),
            },
        );
        assert!(names(&build_tool_roster(None, &configured)).contains(&"AcpAgent"));
    }
}
