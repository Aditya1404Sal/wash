//! Integration test for SSE (Server-Sent Events) end-to-end flow.
//!
//! Tests:
//! 1. Host intercepts SSE connection (Accept: text/event-stream)
//! 2. Component pushes events via bettyblocks:sse/handler WIT import
//! 3. SSE client receives the pushed event data
//! 4. POST with nonexistent stream returns 404

use anyhow::{Context, Result};
use serde_json;
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
    plugin::sse_writer::SseWriterPlugin,
    types::{Component, LocalResources, Workload, WorkloadStartRequest},
    wit::WitInterface,
};

const SSE_DEMO_WASM: &[u8] = include_bytes!("fixtures/sse_demo.wasm");

#[tokio::test]
async fn test_sse_push_event() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    // Create engine
    let engine = Engine::builder().build()?;

    // Create HTTP server with SSE enabled
    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let http_plugin = HttpServer::new(DevRouter::default(), addr)
        .with_sse("/api/record-updates/{model}/{recordId}");

    // Create SseWriterPlugin sharing the same registry
    let sse_registry = Arc::clone(http_plugin.sse_registry().unwrap());
    let sse_writer = SseWriterPlugin::new(sse_registry);

    // Build and start host
    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(sse_writer))?
        .build()?;

    let host = host.start().await.context("Failed to start host")?;

    // Start workload with the sse-demo component
    let req = WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "sse-demo".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(SSE_DEMO_WASM),
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
            host_interfaces: vec![
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "http".to_string(),
                    interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0").unwrap()),
                    config: {
                        let mut c = HashMap::new();
                        c.insert("host".to_string(), "foo".to_string());
                        c
                    },
                },
                WitInterface {
                    namespace: "bettyblocks".to_string(),
                    package: "sse".to_string(),
                    interfaces: ["handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.1.0").unwrap()),
                    config: HashMap::new(),
                },
            ],
            volumes: vec![],
        },
    };

    host.workload_start(req)
        .await
        .context("Failed to start workload")?;

    println!("Host started on {addr}, workload running");

    let client = reqwest::Client::new();

    // --- Test 1: Open SSE connection, push event, verify receipt ---

    // Open SSE connection (streaming response)
    let mut sse_resp = timeout(
        Duration::from_secs(10),
        client
            .get(format!(
                "http://{addr}/api/record-updates/chatMessage/323"
            ))
            .header("Accept", "text/event-stream")
            .header("HOST", "foo")
            .send(),
    )
    .await
    .context("SSE connection timed out")?
    .context("SSE connection failed")?;

    assert_eq!(sse_resp.status(), 200);
    assert_eq!(
        sse_resp.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let stream_id = sse_resp
        .headers()
        .get("X-SSE-Stream-Id")
        .context("Missing X-SSE-Stream-Id header")?
        .to_str()?
        .to_string();

    println!("SSE connection opened, stream_id: {stream_id}");

    // Query registered connections via the component's /connections/ endpoint
    // scope_key for chatMessage/323 is "sse:scope:323:chatMessage" (values sorted)
    let connections_resp = timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{addr}/connections/sse:scope:323:chatMessage"))
            .header("HOST", "foo")
            .send(),
    )
    .await
    .context("Connections query timed out")?
    .context("Connections query failed")?;

    assert_eq!(connections_resp.status(), 200);
    let connections_body = connections_resp.text().await?;
    let connections: Vec<String> = serde_json::from_str(&connections_body)?;
    println!("Registered connections for scope: {connections:?}");
    assert!(
        connections.contains(&stream_id),
        "Stream {stream_id} not found in connections list"
    );

    // Push event via component POST
    let push_resp = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "foo")
            .header("Content-Type", "application/json")
            .body(format!(
                r#"{{"stream_id":"{stream_id}","data":"hello from test"}}"#
            ))
            .send(),
    )
    .await
    .context("Push request timed out")?
    .context("Push request failed")?;

    assert_eq!(push_resp.status(), 200);
    println!("Event pushed successfully");

    // Read chunk from SSE stream
    let chunk = timeout(Duration::from_secs(10), sse_resp.chunk())
        .await
        .context("SSE read timed out")?
        .context("SSE read failed")?
        .context("SSE stream ended unexpectedly")?;

    assert_eq!(chunk, bytes::Bytes::from("data: hello from test\n\n"));
    println!("SSE event received: {:?}", chunk);

    // --- Test 2: POST with nonexistent stream → 404 ---

    let bad_resp = timeout(
        Duration::from_secs(10),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "foo")
            .header("Content-Type", "application/json")
            .body(r#"{"stream_id":"nonexistent","data":"x"}"#)
            .send(),
    )
    .await
    .context("Bad push request timed out")?
    .context("Bad push request failed")?;

    assert_eq!(bad_resp.status(), 404);
    println!("Nonexistent stream correctly returned 404");

    Ok(())
}

/// Push 10 numbered events over an extended duration and verify all arrive in order.
#[tokio::test]
async fn test_sse_sustained_10_events() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let engine = Engine::builder().build()?;

    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let http_plugin = HttpServer::new(DevRouter::default(), addr)
        .with_sse("/api/record-updates/{model}/{recordId}");

    let sse_registry = Arc::clone(http_plugin.sse_registry().unwrap());
    let sse_writer = SseWriterPlugin::new(sse_registry);

    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(sse_writer))?
        .build()?;

    let host = host.start().await.context("Failed to start host")?;

    let req = WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "sse-demo-sustained".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(SSE_DEMO_WASM),
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
            host_interfaces: vec![
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "http".to_string(),
                    interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0").unwrap()),
                    config: {
                        let mut c = HashMap::new();
                        c.insert("host".to_string(), "foo".to_string());
                        c
                    },
                },
                WitInterface {
                    namespace: "bettyblocks".to_string(),
                    package: "sse".to_string(),
                    interfaces: ["handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.1.0").unwrap()),
                    config: HashMap::new(),
                },
            ],
            volumes: vec![],
        },
    };

    host.workload_start(req).await.context("Failed to start workload")?;

    let client = reqwest::Client::new();

    // Open SSE connection
    let mut sse_resp = timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{addr}/api/record-updates/chatMessage/999"))
            .header("Accept", "text/event-stream")
            .header("HOST", "foo")
            .send(),
    )
    .await
    .context("SSE connection timed out")?
    .context("SSE connection failed")?;

    assert_eq!(sse_resp.status(), 200);

    let stream_id = sse_resp
        .headers()
        .get("X-SSE-Stream-Id")
        .context("Missing X-SSE-Stream-Id header")?
        .to_str()?
        .to_string();

    println!("Sustained test: SSE connection opened, stream_id: {stream_id}");

    // Query registered connections via the component's /connections/ endpoint
    // scope_key for chatMessage/999 is "sse:scope:999:chatMessage" (values sorted)
    let connections_resp = timeout(
        Duration::from_secs(10),
        client
            .get(format!("http://{addr}/connections/sse:scope:999:chatMessage"))
            .header("HOST", "foo")
            .send(),
    )
    .await
    .context("Connections query timed out")?
    .context("Connections query failed")?;

    assert_eq!(connections_resp.status(), 200);
    let connections_body = connections_resp.text().await?;
    let connections: Vec<String> = serde_json::from_str(&connections_body)?;
    println!("Registered connections for scope: {connections:?}");
    assert!(
        connections.contains(&stream_id),
        "Stream {stream_id} not found in connections list"
    );

    // Push 10 events with a small delay between each
    for i in 1..=10 {
        let push_resp = timeout(
            Duration::from_secs(10),
            client
                .post(format!("http://{addr}/"))
                .header("HOST", "foo")
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"stream_id":"{stream_id}","data":"{i}"}}"#))
                .send(),
        )
        .await
        .context(format!("Push #{i} timed out"))?
        .context(format!("Push #{i} failed"))?;

        assert_eq!(push_resp.status(), 200, "Push #{i} returned non-200");

        // Small delay to simulate real-world pacing
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    println!("All 10 events pushed, reading from SSE stream...");

    // Read all 10 events from the stream and verify order
    for i in 1..=10 {
        let chunk = timeout(Duration::from_secs(10), sse_resp.chunk())
            .await
            .context(format!("SSE read #{i} timed out"))?
            .context(format!("SSE read #{i} failed"))?
            .context(format!("SSE stream ended before event #{i}"))?;

        let expected = format!("data: {i}\n\n");
        assert_eq!(
            chunk,
            bytes::Bytes::from(expected.clone()),
            "Event #{i} mismatch: got {:?}",
            String::from_utf8_lossy(&chunk)
        );
        println!("  Received event #{i}: {}", i);
    }

    println!("All 10 events received in order");

    Ok(())
}

/// Manual testing server — starts the host on port 8080 and waits forever.
///
/// Run with: cargo test -p wash-runtime --test integration_sse manual_sse_server -- --ignored --nocapture
///
/// Then in two terminals:
///   Terminal 1 (SSE listener):
///     curl -N -H "Accept: text/event-stream" -H "HOST: foo" http://localhost:8080/api/record-updates/chatMessage/323
///     (note the X-SSE-Stream-Id header in the response)
///
///   Terminal 2 (push events):
///     curl -X POST http://localhost:8080/ -H "HOST: foo" -H "Content-Type: application/json" \
///       -d '{"stream_id":"<paste-stream-id-here>","data":"hello from curl"}'
#[tokio::test]
#[ignore]
async fn manual_sse_server() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("wash_runtime=debug".parse().unwrap()),
        )
        .init();

    let engine = Engine::builder().build()?;

    let addr: SocketAddr = "127.0.0.1:8080".parse()?;
    let http_plugin = HttpServer::new(DevRouter::default(), addr)
        .with_sse("/api/record-updates/{model}/{recordId}");

    let sse_registry = Arc::clone(http_plugin.sse_registry().unwrap());
    let sse_writer = SseWriterPlugin::new(sse_registry);

    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(sse_writer))?
        .build()?;

    let host = host.start().await.context("Failed to start host")?;

    let req = WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "sse-demo-manual".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(SSE_DEMO_WASM),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: HashMap::new(),
                    environment: HashMap::new(),
                    volume_mounts: vec![],
                    allowed_hosts: vec![],
                },
                pool_size: 1,
                max_invocations: 1000,
            }],
            host_interfaces: vec![
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "http".to_string(),
                    interfaces: ["incoming-handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0").unwrap()),
                    config: {
                        let mut c = HashMap::new();
                        c.insert("host".to_string(), "foo".to_string());
                        c
                    },
                },
                WitInterface {
                    namespace: "bettyblocks".to_string(),
                    package: "sse".to_string(),
                    interfaces: ["handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.1.0").unwrap()),
                    config: HashMap::new(),
                },
            ],
            volumes: vec![],
        },
    };

    host.workload_start(req).await.context("Failed to start workload")?;

    println!("\n========================================");
    println!("  SSE server running on http://localhost:8080");
    println!("========================================");
    println!("\nTerminal 1 (SSE listener):");
    println!("  curl -N -H \"Accept: text/event-stream\" -H \"HOST: foo\" http://localhost:8080/api/record-updates/chatMessage/323");
    println!("\nTerminal 2 (push events):");
    println!("  curl -X POST http://localhost:8080/ -H \"HOST: foo\" -H \"Content-Type: application/json\" \\");
    println!("    -d '{{\"stream_id\":\"<paste-stream-id>\",\"data\":\"hello\"}}'");
    println!("\nPress Ctrl+C to stop.\n");

    // Sleep forever — Ctrl+C to stop
    tokio::signal::ctrl_c().await?;
    println!("Shutting down...");

    Ok(())
}
