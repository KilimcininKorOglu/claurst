//! Makes a tool served by an MCP server look like a native one.
//!
//! Lives here rather than in the binary because every front end that runs a
//! query loop needs it: a session started from an editor over ACP reads the
//! same `settings.json` as one started from a terminal, and would otherwise
//! be handed a roster with every configured MCP server missing from it.

use std::sync::Arc;

use async_trait::async_trait;
use claurst_core::types::ToolDefinition;

use crate::{PermissionLevel, Tool, ToolContext, ToolResult};

/// One tool from a connected MCP server, called through its manager.
pub struct McpToolWrapper {
    tool_def: ToolDefinition,
    server_name: String,
    manager: Arc<claurst_mcp::McpManager>,
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn description(&self) -> &str {
        &self.tool_def.description
    }

    fn permission_level(&self) -> PermissionLevel {
        // MCP tools run external processes – treat as Execute.
        PermissionLevel::Execute
    }

    fn input_schema(&self) -> serde_json::Value {
        self.tool_def.input_schema.clone()
    }

    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolResult {
        let desc = format!("Run MCP tool {}", self.tool_def.name);
        if let Err(e) = ctx.check_permission(self.name(), &desc, false) {
            return ToolResult::error(e.to_string());
        }

        // Strip the server-name prefix to get the bare tool name.
        let prefix = format!("{}_", self.server_name);
        let bare_name = self
            .tool_def
            .name
            .strip_prefix(&prefix)
            .unwrap_or(&self.tool_def.name);

        let args = if input.is_null() { None } else { Some(input) };

        match self.manager.call_tool(&self.tool_def.name, args).await {
            Ok(result) => {
                let text = claurst_mcp::mcp_result_to_string(&result);
                if result.is_error {
                    ToolResult::error(text)
                } else {
                    ToolResult::success(text)
                }
            }
            Err(e) => ToolResult::error(format!("MCP tool '{}' failed: {}", bare_name, e)),
        }
    }
}

/// Every tool the connected MCP servers offer, ready to join a roster.
pub fn mcp_tools(manager: &Arc<claurst_mcp::McpManager>) -> Vec<Box<dyn Tool>> {
    manager
        .all_tool_definitions()
        .into_iter()
        .map(|(server_name, tool_def)| {
            Box::new(McpToolWrapper {
                tool_def,
                server_name,
                manager: Arc::clone(manager),
            }) as Box<dyn Tool>
        })
        .collect()
}
