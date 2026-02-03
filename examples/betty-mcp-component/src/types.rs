use rust_mcp_schema::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============ MCP Tool Types ============

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolWithAction {
    #[serde(flatten)]
    pub tool: Tool,
    #[serde(rename = "action-id")]
    pub action_id: String,
}

// ============ MCP Server Configuration ============

#[derive(Debug, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub tools: Vec<ToolWithAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct McpServersConfig {
    #[serde(rename = "mcp-servers")]
    pub mcp_servers: Vec<McpServerConfig>,
}

// ============ Action Execution Types ============
#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub struct ActionPayload {
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
pub struct ActionResponse {
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}
