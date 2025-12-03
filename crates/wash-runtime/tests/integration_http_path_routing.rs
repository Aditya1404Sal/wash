//! Integration test for HTTP path-based routing
//!
//! This test demonstrates:
//! 1. Multiple components on the same host with different path prefixes
//! 2. Path-based routing with longest prefix match
//! 3. Backwards compatibility with host-only routing

use anyhow::{Context, Result};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::timeout;

mod common;
use common::find_available_port;

use wash_runtime::{
    engine::Engine,
    host::{
        HostApi, HostBuilder,
        http::{DynamicRouter, HttpServer},
    },
    plugin::wasi_logging::WasiLogging,
    types::{Component, LocalResources, Workload, WorkloadStartRequest, WorkloadStopRequest},
    wit::WitInterface,
};

const HTTP_PATH_API: &[u8] = include_bytes!("fixtures/http_path_api.wasm");
const HTTP_PATH_ADMIN: &[u8] = include_bytes!("fixtures/http_path_admin.wasm");

#[tokio::test]
async fn test_path_based_routing() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("Starting path-based routing test");

    // Create engine
    let engine = Engine::builder().build()?;

    // Create HTTP server plugin with DynamicRouter
    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let http_handler = DynamicRouter::default();
    let http_plugin = HttpServer::new(http_handler, addr);

    // Create logging plugin
    let logging_plugin = WasiLogging {};

    // Build host
    let host = HostBuilder::new()
        .with_engine(engine.clone())
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(logging_plugin))?
        .build()?;

    println!("Created host with path-based routing");

    // Start the host
    let host = host.start().await.context("Failed to start host")?;
    println!("Host started, HTTP server listening on {addr}");

    // Create first workload at /api path
    let api_workload_id = uuid::Uuid::new_v4().to_string();
    let api_req = WorkloadStartRequest {
        workload_id: api_workload_id.clone(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "api-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(HTTP_PATH_API),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: HashMap::new(),
                    environment: HashMap::new(),
                    volume_mounts: vec![],
                    allowed_hosts: vec![],
                },
                pool_size: 1,
                max_invocations: 100,
            }],
            host_interfaces: vec![WitInterface {
                namespace: "wasi".to_string(),
                package: "http".to_string(),
                interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                version: None,
                config: {
                    let mut config = HashMap::new();
                    config.insert("host".to_string(), "localhost".to_string());
                    config.insert("path".to_string(), "/api".to_string());
                    config
                },
            }],
            volumes: vec![],
        },
    };

    // Create second workload at /admin path
    let admin_workload_id = uuid::Uuid::new_v4().to_string();
    let admin_req = WorkloadStartRequest {
        workload_id: admin_workload_id.clone(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "admin-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(HTTP_PATH_ADMIN),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: HashMap::new(),
                    environment: HashMap::new(),
                    volume_mounts: vec![],
                    allowed_hosts: vec![],
                },
                pool_size: 1,
                max_invocations: 100,
            }],
            host_interfaces: vec![WitInterface {
                namespace: "wasi".to_string(),
                package: "http".to_string(),
                interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                version: None,
                config: {
                    let mut config = HashMap::new();
                    config.insert("host".to_string(), "localhost".to_string());
                    config.insert("path".to_string(), "/admin".to_string());
                    config
                },
            }],
            volumes: vec![],
        },
    };

    // Start both workloads
    let _api_start_response = host
        .workload_start(api_req)
        .await
        .context("Failed to start api workload")?;
    println!("Started API workload at /api");

    let _admin_start_response = host
        .workload_start(admin_req)
        .await
        .context("Failed to start admin workload")?;
    println!("Started Admin workload at /admin");

    let client = reqwest::Client::new();

    // Test /api endpoint
    println!("Testing /api endpoint");
    let api_response = timeout(
        Duration::from_secs(5),
        client
            .get(format!("http://{addr}/api"))
            .header("HOST", "localhost")
            .send(),
    )
    .await
    .context("API request timed out")?
    .context("Failed to make API request")?;

    assert!(
        api_response.status().is_success(),
        "Expected success for /api, got {}",
        api_response.status()
    );
    let api_body = api_response.text().await?;
    assert!(
        api_body.contains("Hello from API!"),
        "Expected 'Hello from API' in response, got: {}",
        api_body
    );
    println!("✓ /api responded with correct body: {}", api_body.trim());

    // Test /admin endpoint
    println!("Testing /admin endpoint");
    let admin_response = timeout(
        Duration::from_secs(5),
        client
            .get(format!("http://{addr}/admin"))
            .header("HOST", "localhost")
            .send(),
    )
    .await
    .context("Admin request timed out")?
    .context("Failed to make Admin request")?;

    assert!(
        admin_response.status().is_success(),
        "Expected success for /admin, got {}",
        admin_response.status()
    );
    let admin_body = admin_response.text().await?;
    assert!(
        admin_body.contains("Hello from Admin!"),
        "Expected 'Hello from Admin' in response, got: {}",
        admin_body
    );
    println!(
        "✓ /admin responded with correct body: {}",
        admin_body.trim()
    );

    // Test nested paths (should route to correct prefix)
    println!("Testing nested path /api/users");
    let nested_response = timeout(
        Duration::from_secs(5),
        client
            .get(format!("http://{addr}/api/users"))
            .header("HOST", "localhost")
            .send(),
    )
    .await
    .context("Nested path request timed out")?
    .context("Failed to make nested path request")?;

    assert!(
        nested_response.status().is_success(),
        "Expected success for /api/users, got {}",
        nested_response.status()
    );
    let nested_body = nested_response.text().await?;
    assert!(
        nested_body.contains("Hello from API!"),
        "Expected 'Hello from API' (routed to /api workload), got: {}",
        nested_body
    );
    println!("✓ /api/users routed to /api workload correctly");

    // Test root path (should fail - no workload bound)
    println!("Testing root path / (should fail)");
    let root_response = client
        .get(format!("http://{addr}/"))
        .header("HOST", "localhost")
        .send()
        .await;

    match root_response {
        Ok(resp) => {
            assert!(
                resp.status().is_client_error(),
                "Expected 4xx error for /, got {}",
                resp.status()
            );
            println!("✓ Root path correctly rejected with {}", resp.status());
        }
        Err(_) => {
            println!("✓ Root path correctly rejected with connection error");
        }
    }

    // Clean up
    host.workload_stop(WorkloadStopRequest {
        workload_id: api_workload_id,
    })
    .await
    .context("Failed to stop api workload")?;

    host.workload_stop(WorkloadStopRequest {
        workload_id: admin_workload_id,
    })
    .await
    .context("Failed to stop admin workload")?;

    println!("Path-based routing test passed!");
    Ok(())
}

#[tokio::test]
async fn test_longest_prefix_match() -> Result<()> {
    println!("Starting longest prefix match test");

    let engine = Engine::builder().build()?;
    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let http_handler = DynamicRouter::default();
    let http_plugin = HttpServer::new(http_handler, addr);
    let logging_plugin = WasiLogging {};

    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(logging_plugin))?
        .build()?
        .start()
        .await?;

    // Create workload for /api
    let api_workload_id = uuid::Uuid::new_v4().to_string();
    let api_req = WorkloadStartRequest {
        workload_id: api_workload_id.clone(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "api-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(HTTP_PATH_API),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: HashMap::new(),
                    environment: HashMap::new(),
                    volume_mounts: vec![],
                    allowed_hosts: vec![],
                },
                pool_size: 1,
                max_invocations: 100,
            }],
            host_interfaces: vec![WitInterface {
                namespace: "wasi".to_string(),
                package: "http".to_string(),
                interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                version: None,
                config: {
                    let mut config = HashMap::new();
                    config.insert("host".to_string(), "localhost".to_string());
                    config.insert("path".to_string(), "/api".to_string());
                    config
                },
            }],
            volumes: vec![],
        },
    };

    // Create workload for /api/v2 (longer prefix)
    let api_v2_workload_id = uuid::Uuid::new_v4().to_string();
    let api_v2_req = WorkloadStartRequest {
        workload_id: api_v2_workload_id.clone(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "api-v2-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(HTTP_PATH_ADMIN),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: HashMap::new(),
                    environment: HashMap::new(),
                    volume_mounts: vec![],
                    allowed_hosts: vec![],
                },
                pool_size: 1,
                max_invocations: 100,
            }],
            host_interfaces: vec![WitInterface {
                namespace: "wasi".to_string(),
                package: "http".to_string(),
                interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                version: None,
                config: {
                    let mut config = HashMap::new();
                    config.insert("host".to_string(), "localhost".to_string());
                    config.insert("path".to_string(), "/api/v2".to_string());
                    config
                },
            }],
            volumes: vec![],
        },
    };

    let _api_start_response = host.workload_start(api_req).await?;
    println!("Started workload at /api");

    let _api_v2_start_response = host.workload_start(api_v2_req).await?;
    println!("Started workload at /api/v2");

    let client = reqwest::Client::new();

    // Request to /api/users should match /api (shorter prefix)
    let api_users_response = client
        .get(format!("http://{addr}/api/users"))
        .header("HOST", "localhost")
        .send()
        .await?;
    assert!(api_users_response.status().is_success());
    let api_users_body = api_users_response.text().await?;
    assert!(
        api_users_body.contains("Hello from API!"),
        "Expected /api/users to route to API component"
    );
    println!("✓ /api/users matched /api workload");

    // Request to /api/v2/users should match /api/v2 (longer prefix - better match)
    let api_v2_users_response = client
        .get(format!("http://{addr}/api/v2/users"))
        .header("HOST", "localhost")
        .send()
        .await?;
    assert!(api_v2_users_response.status().is_success());
    let api_v2_users_body = api_v2_users_response.text().await?;
    assert!(
        api_v2_users_body.contains("Hello from Admin!"),
        "Expected /api/v2/users to route to Admin component (longest prefix match)"
    );
    println!("✓ /api/v2/users matched /api/v2 workload (longest prefix)");

    // Clean up
    host.workload_stop(WorkloadStopRequest {
        workload_id: api_workload_id,
    })
    .await?;
    host.workload_stop(WorkloadStopRequest {
        workload_id: api_v2_workload_id,
    })
    .await?;

    println!("Longest prefix match test passed!");
    Ok(())
}
