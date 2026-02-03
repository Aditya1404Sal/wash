//! Integration test for betty-mcp-component with JWT authentication and actions
//!
//! Tests the full MCP (Model Context Protocol) flow:
//! 1. JWT token validation via betty-auth-component
//! 2. Config store loading for MCP server configuration
//! 3. JSON-RPC request handling (initialize, tools/list, tools/call)
//! 4. Action execution via mock-actions component

#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::timeout;

mod common;
use common::find_available_port;

use wash_runtime::{
    engine::Engine,
    host::{
        HostApi, HostBuilder,
        http::{DevRouter, HttpServer},
    },
    plugin::{wasi_config::WasiConfig, wasi_logging::WasiLogging},
    types::{Component, LocalResources, Workload, WorkloadStartRequest},
    wit::WitInterface,
};

const BETTY_MCP_COMPONENT_WASM: &[u8] = include_bytes!("fixtures/betty_mcp_component.wasm");
const JWT_AUTH_COMPONENT_WASM: &[u8] = include_bytes!("fixtures/jwt_auth_component.wasm");
const MOCK_ACTIONS_WASM: &[u8] = include_bytes!("fixtures/mock_actions.wasm");

// ============ JWT Token Generation ============

#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    app_uuid: String,
    aud: String,
    auth_profile: String,
    cas_token: String,
    exp: u64,
    iat: u64,
    iss: String,
    jti: String,
    locale: Option<String>,
    nbf: u64,
    roles: Vec<u32>,
    user_id: u32,
}

fn generate_rsa_key_pair() -> (String, String) {
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};

    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).expect("failed to generate private key");
    let public_key = private_key.to_public_key();

    let private_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("failed to encode private key")
        .to_string();

    let public_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .expect("failed to encode public key")
        .to_string();

    (private_pem, public_pem)
}

fn generate_claims(exp_offset_seconds: i64) -> JwtClaims {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    JwtClaims {
        app_uuid: "ca432225a56f242f7bd2131da647d3c82".to_string(),
        aud: "Joken".to_string(),
        auth_profile: "5bf9eba34636495d80ed5a790ca39077".to_string(),
        cas_token: "d652585964ecfd59bd738bb33f5a421ce85c493e".to_string(),
        exp: (now as i64 + exp_offset_seconds) as u64,
        iat: now,
        iss: "Joken".to_string(),
        jti: "326h3prprbgrfr4u9k03luh2".to_string(),
        locale: None,
        nbf: now,
        roles: vec![1],
        user_id: 1,
    }
}

fn generate_jwt_token_rs256(private_key_pem: &str, claims: &JwtClaims) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    let header = Header::new(Algorithm::RS256);
    let private_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .expect("failed to load private key for signing");
    encode(&header, claims, &private_key).expect("failed to encode JWT")
}

// ============ MCP Server Configuration ============

fn mcp_servers_config_json() -> String {
    serde_json::json!({
        "mcp-servers": [
            {
                "id": "weather-server-001",
                "tools": [
                    {
                        "action-id": "action-weather-get",
                        "name": "get_weather",
                        "description": "Gets weather for a location",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "location": {
                                    "type": "string",
                                    "description": "City and province"
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
                                "a": { "type": "number", "description": "First number" },
                                "b": { "type": "number", "description": "Second number" }
                            },
                            "required": ["a", "b"]
                        }
                    }
                ]
            }
        ]
    })
    .to_string()
}

// ============ WIT Interfaces ============

fn mcp_host_interfaces(http_host_config: &str, pub_key: String) -> Vec<WitInterface> {
    vec![
        // HTTP incoming handler
        WitInterface {
            namespace: "wasi".to_string(),
            package: "http".to_string(),
            interfaces: ["incoming-handler".to_string()].into_iter().collect(),
            version: Some(semver::Version::parse("0.2.2").unwrap()),
            config: {
                let mut config = HashMap::new();
                config.insert("host".to_string(), http_host_config.to_string());
                config
            },
        },
        // Config store
        WitInterface {
            namespace: "wasi".to_string(),
            package: "config".to_string(),
            interfaces: ["store".to_string()].into_iter().collect(),
            version: Some(semver::Version::parse("0.2.0-rc.1").unwrap()),
            config: HashMap::from([("mcp_servers".to_string(), mcp_servers_config_json())]),
        },
        // Logging
        WitInterface {
            namespace: "wasi".to_string(),
            package: "logging".to_string(),
            interfaces: ["logging".to_string()].into_iter().collect(),
            version: Some(semver::Version::parse("0.1.0-draft").unwrap()),
            config: HashMap::new(),
        },
        WitInterface {
            namespace: "wasi".to_string(),
            package: "cli".to_string(),
            interfaces: ["environment".to_string()].into_iter().collect(),
            version: Some(semver::Version::parse("0.2.0").unwrap()),
            config: HashMap::from([
                ("JWT_PUBLIC_KEY".to_string(), pub_key),
                ("JWT_ISSUER".to_string(), "Joken".to_string()),
                ("JWT_AUDIENCE".to_string(), "Joken".to_string()),
            ]),
        },
    ]
}

// ============ Host Setup ============

async fn start_mcp_host(addr: SocketAddr) -> Result<impl HostApi> {
    let engine = Engine::builder().build()?;
    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(HttpServer::new(DevRouter::default(), addr)))
        .with_plugin(Arc::new(WasiLogging {}))?
        .with_plugin(Arc::new(WasiConfig::default()))?
        .build()?;

    host.start().await.context("failed to start host")
}

fn mcp_workload_request(http_host_config: &str, public_key_pem: &str) -> WorkloadStartRequest {
    WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "mcp-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![
                // Betty MCP Component - imports auth/jwt and actions, exports HTTP handler
                Component {
                    bytes: bytes::Bytes::from_static(BETTY_MCP_COMPONENT_WASM),
                    local_resources: LocalResources {
                        memory_limit_mb: 256,
                        cpu_limit: 1,
                        config: HashMap::from([(
                            "mcp_servers".to_string(),
                            mcp_servers_config_json(),
                        )]),
                        environment: HashMap::from([
                            ("JWT_PUBLIC_KEY".to_string(), public_key_pem.to_string()),
                            ("JWT_ISSUER".to_string(), "Joken".to_string()),
                            ("JWT_AUDIENCE".to_string(), "Joken".to_string()),
                        ]),
                        volume_mounts: vec![],
                        allowed_hosts: vec![],
                    },
                    pool_size: 1,
                    max_invocations: 100,
                },
                // JWT Auth Component - exports betty-blocks:auth/jwt
                Component {
                    bytes: bytes::Bytes::from_static(JWT_AUTH_COMPONENT_WASM),
                    local_resources: LocalResources {
                        memory_limit_mb: 128,
                        cpu_limit: 1,
                        config: HashMap::new(),
                        environment: HashMap::new(),
                        volume_mounts: vec![],
                        allowed_hosts: vec![],
                    },
                    pool_size: 1,
                    max_invocations: 100,
                },
                // Mock Actions Component - exports betty-blocks:actions/actions
                Component {
                    bytes: bytes::Bytes::from_static(MOCK_ACTIONS_WASM),
                    local_resources: LocalResources {
                        memory_limit_mb: 128,
                        cpu_limit: 1,
                        config: HashMap::new(),
                        environment: HashMap::new(),
                        volume_mounts: vec![],
                        allowed_hosts: vec![],
                    },
                    pool_size: 1,
                    max_invocations: 100,
                },
            ],
            host_interfaces: mcp_host_interfaces(http_host_config, public_key_pem.to_string()),
            volumes: vec![],
        },
    }
}

// ============ Tests ============

#[tokio::test]
async fn test_mcp_initialize() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-test", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // JSON-RPC initialize request
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {}
        },
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-test")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&init_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await?;

    eprintln!("Initialize response status: {status}");
    eprintln!("Initialize response body: {response_text}");

    assert!(
        status.is_success(),
        "initialize should succeed. status: {status}, body: {response_text}"
    );

    let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
    assert_eq!(response_json["jsonrpc"], "2.0");
    assert!(response_json["result"]["serverInfo"].is_object());

    Ok(())
}

#[tokio::test]
async fn test_mcp_tools_list() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-tools-list", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // JSON-RPC tools/list request
    let list_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {},
        "id": 2
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-tools-list")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&list_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await?;

    eprintln!("Tools/list response status: {status}");
    eprintln!("Tools/list response body: {response_text}");

    assert!(
        status.is_success(),
        "tools/list should succeed. status: {status}, body: {response_text}"
    );

    let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
    assert_eq!(response_json["jsonrpc"], "2.0");

    let tools = response_json["result"]["tools"].as_array();
    assert!(tools.is_some(), "response should contain tools array");
    assert!(
        !tools.unwrap().is_empty(),
        "tools array should not be empty"
    );

    // Verify the weather tool is present
    let tool_names: Vec<&str> = tools
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        tool_names.contains(&"get_weather"),
        "should contain get_weather tool"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_tools_call() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-tools-call", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // JSON-RPC tools/call request
    let call_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": {
            "name": "get_weather",
            "arguments": {
                "location": "Amsterdam, NH"
            }
        },
        "id": 3
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-tools-call")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&call_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await.context("failed to read response body")?;

    eprintln!("Tools/call response status: {status}");
    eprintln!("Tools/call response body: {response_text}");

    assert!(
        status.is_success(),
        "tools/call should succeed. status: {status}, body: {response_text}"
    );

    let response_json: serde_json::Value =
        serde_json::from_str(&response_text).context("failed to parse response as JSON")?;
    assert_eq!(response_json["jsonrpc"], "2.0");

    // Verify the response contains content from mock-actions
    let content = &response_json["result"]["content"];
    assert!(content.is_array(), "response should contain content array");

    Ok(())
}

#[tokio::test]
async fn test_mcp_unauthorized_without_token() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (_, public_key) = generate_rsa_key_pair();

    let req = mcp_workload_request("mcp-no-auth", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // Request without Authorization header
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {},
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-no-auth")
            .header("Content-Type", "application/json")
            // No Authorization header
            .json(&init_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    eprintln!("Unauthorized response status: {status}");

    assert_eq!(
        status.as_u16(),
        401,
        "request without token should return 401 Unauthorized"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_unauthorized_with_invalid_token() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    // Generate two different key pairs - sign with one, verify with other
    let (private_key1, _) = generate_rsa_key_pair();
    let (_, public_key2) = generate_rsa_key_pair();

    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key1, &claims);

    // Configure workload with public_key2, but token is signed with private_key1
    let req = mcp_workload_request("mcp-invalid-token", &public_key2);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {},
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-invalid-token")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&init_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    eprintln!("Invalid token response status: {status}");

    assert_eq!(
        status.as_u16(),
        401,
        "request with invalid token should return 401 Unauthorized"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_unauthorized_with_expired_token() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    // Generate expired token (expired 1 hour ago)
    let claims = generate_claims(-3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-expired-token", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {},
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-expired-token")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&init_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    eprintln!("Expired token response status: {status}");

    assert_eq!(
        status.as_u16(),
        401,
        "request with expired token should return 401 Unauthorized"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_server_not_found() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-not-found", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // Request for non-existent server
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {},
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/nonexistent-server-999"))
            .header("HOST", "mcp-not-found")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&init_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await?;

    eprintln!("Server not found response status: {status}");
    eprintln!("Server not found response body: {response_text}");

    // Should return an error (either 4xx or JSON-RPC error)
    let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
    assert!(
        response_json["error"].is_object() || status.is_client_error(),
        "non-existent server should return error"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_invalid_jsonrpc() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-invalid-rpc", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // Invalid JSON-RPC (missing required fields)
    let invalid_request = serde_json::json!({
        "not_jsonrpc": "invalid"
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-invalid-rpc")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&invalid_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await?;

    eprintln!("Invalid JSON-RPC response status: {status}");
    eprintln!("Invalid JSON-RPC response body: {response_text}");

    // Should return JSON-RPC error
    let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
    assert!(
        response_json["error"].is_object(),
        "invalid JSON-RPC should return error object"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_method_not_found() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-method-not-found", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // Unknown method
    let unknown_method_request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "unknown/method",
        "params": {},
        "id": 1
    });

    let response = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-method-not-found")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {token}"))
            .json(&unknown_method_request)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    let response_text = response.text().await?;

    eprintln!("Method not found response status: {status}");
    eprintln!("Method not found response body: {response_text}");

    let response_json: serde_json::Value = serde_json::from_str(&response_text)?;
    assert!(
        response_json["error"].is_object(),
        "unknown method should return error object"
    );
    // MCP component returns -32000 (server error) with "Method not found" message
    assert_eq!(
        response_json["error"]["code"], -32000,
        "should return server error code for unknown method"
    );
    assert!(
        response_json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Method not found"),
        "error message should indicate method not found"
    );

    Ok(())
}

#[tokio::test]
async fn test_mcp_wrong_http_method() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let host = start_mcp_host(addr).await?;

    let (private_key, public_key) = generate_rsa_key_pair();
    let claims = generate_claims(3600);
    let token = generate_jwt_token_rs256(&private_key, &claims);

    let req = mcp_workload_request("mcp-wrong-method", &public_key);
    host.workload_start(req)
        .await
        .context("failed to start MCP workload")?;

    let client = reqwest::Client::new();

    // GET request instead of POST
    let response = timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{addr}/mcp/weather-server-001"))
            .header("HOST", "mcp-wrong-method")
            .header("Authorization", format!("Bearer {token}"))
            .send(),
    )
    .await
    .context("request timed out")?
    .context("failed to make request")?;

    let status = response.status();
    eprintln!("Wrong HTTP method response status: {status}");

    assert_eq!(
        status.as_u16(),
        405,
        "GET request should return 405 Method Not Allowed"
    );

    Ok(())
}
