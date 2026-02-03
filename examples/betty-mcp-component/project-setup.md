# 1. Overall App Description

The **HTTP-MCP Provider** is a specialized Wasmcloud component designed to bridge Model Context Protocol (MCP) clients with internal wasmCloud actions. It bypasses the traditional ActionsAPI to provide a direct, high-performance ingress route.

**Key Functionality:**

- **Direct Routing:** Accessible via `app-name.betty.app/api/mcp/{mcp-server-id}`.
- **Dynamic Tools:** Tool definitions and schemas are loaded dynamically from **WASI-Config**, mapping public tool names to internal Action IDs.
- **Integrated Security:** Validates inbound requests using API tokens stored securely in **WASI-Secrets**.
- **Protocol Translation:** Acts as a JSON-RPC 2.0 server, translating `tools/call` methods into executable logic.

# 2 Cargo.toml

```toml
# Use the schema crate directly for WASI compatibility
rust-mcp-schema = { version = "0.1.0", default-features = false }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

# 2. WIT (WebAssembly Interface Type) Description

This component uses standard WASI interfaces for its world definition to ensure compatibility across the Wasmcloud ecosystem.

```wit
package betty:mcp;

world mcp-handler {
    // Inbound HTTP requests from Betty Ingress
    export wasi:http/incoming-handler@0.2.0;

    // To retrieve tool-to-action mapping from ConfigMaps
    import wasi:config/runtime@0.2.0-draft;

    // To fetch the API token for Authorization validation
    import wasi:secrets/store@0.2.0-draft;

    // Interface to execute the underlying tool logic
    import betty:actions/executor;
}
```

# 3. Workflows

### 1. Tool Discovery Flow (tools/list)

This flow allows the client to retrieve all available capabilities mapped to a specific server ID. The application fetches the server configuration from WASI-Config, filters for the requested GUID, and returns the list of tool names and their JSON input schemas.

![Flow 1](toolslist-flow.png)

```
sequenceDiagram
    autonumber
    participant Client
    participant Host as wasmCloud Host
    participant Lib as src/lib.rs
    participant Auth as src/auth/mod.rs
    participant Router as src/mcp/router.rs
    participant Config as src/config/mod.rs

    Note over Client: JSONRpc tools/list
    Client->>+Host: HTTP POST /api/mcp/{server-id}

    Note over Host: http::IncomingRequest
    Host->>+Lib: handle(request, response_out)

    Note over Lib: handle_request()
    Lib->>+Auth: is_authorized(token)
    Auth-->>-Lib: true (Authorized)

    Lib->>+Router: process_rpc(server_id, body)

    Router->>+Config: load_server_config(server_id)
    Config-->>-Router: ServerConfig (Tools & Actions)

    Note right of Router: Method: "tools/list"
    Router->>Router: handle_list_tools()

    Router-->>-Lib: JSON-RPC Result (Tool List)

    Note over Lib: response_out.set(response)
    Lib-->>-Host: Result Sent
    Host-->>-Client: HTTP 200 OK (JSON Body)

```

### 2. Tool Execution Flow (tools/call)

This flow triggers the execution of a specific wasmCloud action when a client invokes a tool. The application validates the request arguments against the stored schema, executes the mapped internal action, and translates the result back into the standard MCP JSON-RPC format.

![Flow 2](handletoolcall-flow.png)

```
sequenceDiagram
    autonumber
    participant Client
    participant Host as wasmCloud Host
    participant Lib as src/lib.rs
    participant Auth as src/auth/mod.rs
    participant Router as src/mcp/router.rs
    participant Config as src/config/mod.rs
    participant Action as src/actions/mod.rs
    participant Executor as betty:actions/executor

    Note over Client: JSONRpc tools/call
    Client->>+Host: HTTP POST /api/mcp/{server-id}

    Note over Host: IncomingRequest
    Host->>+Lib: handle(request, response_out)

    Note over Lib: handle_request()

    Lib->>Lib: validate_content_type()

    Lib->>+Auth: is_authorized(token)
    Auth-->>-Lib: true

    Lib->>+Router: process_rpc(server_id, body)

    Router->>+Config: load_server_config(server_id)
    Config-->>-Router: ServerConfig (Tools & Actions)

    Note right of Router: Method: "tools/call"
    Router->>+Router: handle_call_tool(params, config)

    Note over Router: Validate arguments against inputSchema

    Router->>+Action: execute_mapped_action(action_id, args)

    Action->>+Executor: perform_action(payload)
    Executor-->>-Action: ActionResponse

    Action->>Action: parse_action_output()
    Action-->>-Router: MCP Content Result

    Router-->>-Lib: JSON-RPC Result

    Note over Lib: Set Response (HTTP 200)
    Lib-->>Host: response_out.set(response)
    Host-->>-Client: HTTP 200 OK (Action Result)
```

# 4. File Structure & Function Suggestions

| File Path              | Component       | Function                | Diagram Reference                             |
| :--------------------- | :-------------- | :---------------------- | :-------------------------------------------- |
| **src/lib.rs**         | Entry Point     | `handle`                | Host -> Lib entry point (WASI Export).        |
| **src/lib.rs**         | Entry Point     | `handle_request`        | Logic orchestrator (Note over Lib).           |
| **src/lib.rs**         | Entry Point     | `validate_content_type` | Validation Step 2.5 (application/json check). |
| **src/lib.rs**         | Entry Point     | `read_request_body`     | Converts WASI InputStream to body string.     |
| **src/lib.rs**         | Entry Point     | `extract_server_id`     | Parses {server-id} from the URI path.         |
| **src/auth/mod.rs**    | Security        | `is_authorized`         | Auth check via wasi:secrets (Step 3/4).       |
| **src/mcp/router.rs**  | MCP Logic       | `process_rpc`           | Protocol entry point (Step 5/6).              |
| **src/mcp/router.rs**  | MCP Logic       | `handle_list_tools`     | Method handler for "tools/list".              |
| **src/mcp/router.rs**  | MCP Logic       | `handle_call_tool`      | Method handler for "tools/call".              |
| **src/config/mod.rs**  | WASI Config     | `load_server_config`    | Fetching tools from wasi:config (Step 7/8).   |
| **src/actions/mod.rs** | Action Bridge   | `execute_mapped_action` | Action execution trigger.                     |
| **src/actions/mod.rs** | Action Bridge   | `parse_action_output`   | Translating ActionResponse to MCP result.     |
| **(WIT Import)**       | Action Executor | `perform_action`        | Call to betty:actions/executor.               |

# 5. Description of Structs Needed

### Imported structs

use rust_mcp_schema::{JsonRpcError, ErrorData};

### Configuration Mapping

To link MCP tools to your internal wasmCloud actions, we use **Serde composition**. Since Rust does not support struct inheritance, the `#[serde(flatten)]` attribute is used to "extend" the standard SDK types.

```rust
use serde::{Deserialize, Serialize};
use rust_mcp_schema::{Tool, ListToolsResult}; // From mcp-sdk-rs / rust-mcp-schema

/// Extends the standard MCP Tool with a wasmCloud action-id
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolWithAction {
    #[serde(flatten)]
    pub tool: Tool,

    /// The internal wasmCloud action ID this tool maps to
    #[serde(rename = "action-id")]
    pub action_id: String,
}

/// Extends the standard ListToolsResult to return our custom tools
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ListToolsWithActionResult {
    pub tools: Vec<ToolWithAction>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
```

### Configuration Store Structs

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// The unique GUID used in the URL: /api/mcp/{id}
    pub id: String,

    /// The list of tools available for this specific server instance
    pub tools: Vec<ToolWithAction>,
}
```

### JSON-RPC 2.0 Communication

Standard result types imported from the SDK for tool execution.

```rust
use rust_mcp_schema::{CallToolResult, ContentBlock, TextContent};

// Example usage in execution flow:
// let result = CallToolResult {
//     content: vec![ContentBlock::Text(TextContent {
//         text: "Action executed successfully".to_string()
//     })],
//     is_error: Some(false),
//     ..Default::default()
// };
```

# 6. Tests
