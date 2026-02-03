use crate::types::{McpServerConfig, McpServersConfig};
use crate::wasi::config::store::get_all;

/// Load and validate server configuration for a specific MCP server ID.
/// This is the single entry point - combines existence check and config loading.
/// Returns the full McpServerConfig if found, or an error if not.
pub fn load_server_config(server_id: &str) -> Result<McpServerConfig, String> {
    let servers_config = load_all_servers_config()?;

    servers_config
        .mcp_servers
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| format!("MCP server '{}' not found in configuration", server_id))
}

/// Load all MCP servers from configuration (wasi:config with fallback to default)
fn load_all_servers_config() -> Result<McpServersConfig, String> {
    // Try to load from wasi:config first
    match get_all() {
        Ok(runtime_config) => {
            match load_all_servers_config_from_runtime(&runtime_config) {
                Ok(config) => Ok(config),
                Err(_) => {
                    // Fallback to default if runtime config doesn't have mcp_servers
                    eprintln!("[CONFIG] mcp_servers not found in runtime config, using defaults");
                    load_default_config()
                }
            }
        }
        Err(e) => {
            eprintln!("[CONFIG] Failed to get wasi config: {:?}, using defaults", e);
            load_default_config()
        }
    }
}

fn load_all_servers_config_from_runtime(
    config: &Vec<(String, String)>,
) -> Result<McpServersConfig, String> {
    // Look for "mcp_servers" key
    for (key, value) in config {
        if key == "mcp_servers" {
            return serde_json::from_str(value)
                .map_err(|e| format!("Failed to parse mcp_servers config: {}", e));
        }
    }
    Err("mcp_servers key not found in runtime configuration".to_string())
}

fn load_default_config() -> Result<McpServersConfig, String> {
    // Default configuration for testing
    let config_json = r#"{
        "mcp-servers": [
            {
                "id": "weather-server-001",
                "tools": [
                    {
                        "action-id": "action-weather-get",
                        "name": "get_weather",
                        "description": "Haalt de huidige weersomstandigheden op voor een specifieke locatie.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "location": {
                                    "type": "string",
                                    "description": "De stad en provincie, bijv. Amsterdam, NH"
                                },
                                "unit": {
                                    "type": "string",
                                    "enum": ["celsius", "fahrenheit"],
                                    "description": "De temperatuureenheid die gebruikt moet worden."
                                }
                            },
                            "required": ["location"]
                        }
                    }
                ]
            },
            {
                "id": "calculator-server-001",
                "tools": [
                    {
                        "action-id": "action-calc-add",
                        "name": "add_numbers",
                        "description": "Adds two numbers together",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "a": {
                                    "type": "number",
                                    "description": "First number"
                                },
                                "b": {
                                    "type": "number",
                                    "description": "Second number"
                                }
                            },
                            "required": ["a", "b"]
                        }
                    }
                ]
            }
        ]
    }"#;

    serde_json::from_str(config_json)
        .map_err(|e| format!("Failed to parse default configuration: {}", e))
}

// Requires wasm emv for testing, keeping it on-hold for now

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_load_default_config() -> Result<(), String> {
//         let config = load_all_servers_config()?;
//         assert_eq!(config.mcp_servers.len(), 2);
//         assert_eq!(config.mcp_servers[0].id, "weather-server-001");
//         assert_eq!(config.mcp_servers[1].id, "calculator-server-001");
//         Ok(())
//     }

//     #[test]
//     fn test_load_server_config() {
//         let config = load_server_config("weather-server-001").unwrap();
//         assert_eq!(config.id, "weather-server-001");
//         assert_eq!(config.tools.len(), 1);
//         assert_eq!(config.tools[0].tool.name, "get_weather");
//     }

//     #[test]
//     fn test_load_nonexistent_server() {
//         let result = load_server_config("nonexistent-server");
//         assert!(result.is_err());
//     }
// }
