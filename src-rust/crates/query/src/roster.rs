//! The set of tools a session runs with.
//!
//! One builder, shared by every front end. A session started from an editor
//! reads the same `settings.json` as one started from a terminal, so it must
//! end up with the same tools; building the roster in each front end is how
//! they drifted apart.

use std::sync::Arc;

use claurst_tools::Tool;
use tracing::debug;

/// Built-in tools, the sub-agent tool, the advisor when a model backs it, and
/// every tool the connected MCP servers offer.
pub fn build_tool_roster(
    mcp_manager: Option<Arc<claurst_mcp::McpManager>>,
    advisor_model: Option<&str>,
) -> Arc<Vec<Box<dyn Tool>>> {
    let mut tools: Vec<Box<dyn Tool>> = claurst_tools::all_tools();
    tools.push(Box::new(crate::AgentTool));

    // Offer the advisor only when a model backs it, so a session without one
    // pays neither the tool schema nor the system-prompt guideline for it.
    if advisor_model.is_some_and(|model| !model.trim().is_empty()) {
        tools.push(Box::new(claurst_tools::AdvisorTool));
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

    fn names(tools: &[Box<dyn Tool>]) -> Vec<&str> {
        tools.iter().map(|t| t.name()).collect()
    }

    #[test]
    fn a_session_always_gets_the_built_ins_and_the_sub_agent_tool() {
        let tools = build_tool_roster(None, None);
        let names = names(&tools);

        assert!(names.contains(&"Bash"), "{names:?}");
        assert!(names.contains(&"Read"), "{names:?}");
        assert!(names.contains(&"Agent"), "{names:?}");
    }

    #[test]
    fn the_advisor_is_offered_only_when_a_model_backs_it() {
        assert!(!names(&build_tool_roster(None, None)).contains(&"Advisor"));
        assert!(!names(&build_tool_roster(None, Some("   "))).contains(&"Advisor"));
        assert!(names(&build_tool_roster(None, Some("claude-haiku-4-5"))).contains(&"Advisor"));
    }
}
