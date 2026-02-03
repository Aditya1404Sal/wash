# WIP : This Component is **NOT** Ready
## HTTP-MCP Provider

A wasmCloud component that bridges Model Context Protocol (MCP) clients with internal wasmCloud actions.

## Quick Start

### Prerequisites

- Rust 1.75+
- `wash` CLI (wasmCloud tooling)
- `wit-bindgen` 0.34+

### Build

```bash
cargo build --release
```

### Build Wasm Component

```bash
wash build
```

### Run Tests

```bash
cargo test
```

## Usage

### 1. Initialize MCP Connection

```bash
curl -X POST http://localhost:8080/mcp/weather-server-001 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test-token-12345" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {}
    }
  }'
```

### 2. List Available Tools

```bash
curl -X POST http://localhost:8080/mcp/weather-server-001 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test-token-12345" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/list",
    "params": {}
  }'
```

### 3. Call a Tool

```bash
curl -X POST http://localhost:8080/mcp/weather-server-001 \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test-token-12345" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "get_weather",
      "arguments": {
        "location": "Amsterdam",
        "unit": "celsius"
      }
    }
  }'
```

## Project Structure

```
http-mcp-provider/
├── src/
│   ├── lib.rs           # Entry point
│   ├── types.rs         # Type definitions
│   ├── auth/            # Authentication
│   ├── config/          # Configuration
│   ├── mcp/             # MCP protocol handling
│   └── actions/         # Action execution
├── wit/
│   └── world.wit        # WIT interface definitions
├── Cargo.toml
└── README.md
```

## Configuration

### MCP Servers Config

The component loads MCP server configurations from WASI-Config or uses default configs for testing.

Example configuration structure:

```json
{
  "mcp-servers": [
    {
      "id": "weather-server-001",
      "tools": [
        {
          "action-id": "action-weather-get",
          "name": "get_weather",
          "description": "Get current weather",
          "inputSchema": {
            "type": "object",
            "properties": {
              "location": {"type": "string"}
            },
            "required": ["location"]
          }
        }
      ]
    }
  ]
}
```

### Authentication

Set the API token via:
- WASI-Secrets: `mcp_api_token`
- Environment variable: `MCP_API_TOKEN`
- Default for testing: `test-token-12345`

## Architecture

The component follows a layered architecture:

1. **HTTP Layer** (`lib.rs`): Request validation and routing
2. **Security Layer** (`auth/`): Token-based authentication
3. **Protocol Layer** (`mcp/`): JSON-RPC 2.0 handling
4. **Configuration Layer** (`config/`): Server and tool lookup
5. **Execution Layer** (`actions/`): Action invocation bridge

## Development

### Adding a New Tool

1. Add tool definition to configuration:
```json
{
  "action-id": "your-action-id",
  "name": "your_tool_name",
  "description": "Tool description",
  "inputSchema": { /* JSON Schema */ }
}
```

2. Implement action handler in `actions/mod.rs` or link to existing action

### Testing with Mock Actions

The component includes mock actions for testing:
- `action-weather-get`: Returns mock weather data
- `action-calc-add`: Adds two numbers

To add a mock action, update `mock_action_execution()` in `actions/mod.rs`.

## Deployment

### wasmCloud Deployment

```bash
# Build the component
wash build

# Deploy to wasmCloud
wash app deploy wadm.yaml
```

### Example wadm.yaml

```yaml
apiVersion: wasmcloud.dev/v1alpha1
kind: WadmWorkload
metadata:
  name: http-mcp-provider
spec:
  components:
    - name: http-mcp
      image: file://./build/http_mcp_provider_s.wasm
      traits:
        - type: spreadscaler
          properties:
            replicas: 1
```

## API Reference

### Supported Methods

| Method | Description | Params |
|--------|-------------|--------|
| `initialize` | Protocol handshake | `protocolVersion`, `capabilities` |
| `tools/list` | List available tools | None |
| `tools/call` | Execute a tool | `name`, `arguments` |

### Error Codes

| Code | Meaning |
|------|---------|
| -32600 | Invalid Request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32000 | Server error |

## Security

- All requests require valid API token in `Authorization` header
- Constant-time token comparison prevents timing attacks
- Path-based routing limits attack surface to `/mcp/*`
- Input validation against JSON schemas
- No sensitive information in error responses

## Performance

- Stateless design enables horizontal scaling
- Minimal dependencies for small binary size
- Efficient streaming HTTP responses
- No blocking operations in request path

## Roadmap

- [ ] WASI-Config integration
- [ ] WASI-Secrets integration
- [ ] Real action executor WIT binding
- [ ] Full JSON Schema validation
- [ ] Rate limiting
- [ ] Metrics and observability
- [ ] WebSocket support for MCP

## Contributing

See [PROJECT-SETUP.md](./PROJECT-SETUP.md) for detailed architecture documentation and workflow diagrams.

## License

[Your License Here]

## Support

For issues and questions, please open an issue on the repository.