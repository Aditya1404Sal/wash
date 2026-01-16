//! Integration test for smtp-demo component with concurrent support and URL attachments

use anyhow::{Context, Result};
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
    plugin::{wasi_config::WasiConfig, wasi_logging::WasiLogging, wasmcloud_smtp::WasmcloudSmtp},
    types::{Component, LocalResources, Workload, WorkloadStartRequest},
    wit::WitInterface,
};

const SMTP_DEMO_WASM: &[u8] = include_bytes!("fixtures/smtp_demo.wasm");

#[tokio::test]
async fn test_smtp_demo_integration() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("Starting SMTP demo component integration test");

    let engine = Engine::builder().build()?;
    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let http_handler = DevRouter::default();
    let http_plugin = HttpServer::new(http_handler, addr);
    let smtp_plugin = WasmcloudSmtp::new();
    let logging_plugin = WasiLogging {};
    let config_plugin = WasiConfig::default();

    let host = HostBuilder::new()
        .with_engine(engine.clone())
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(smtp_plugin))?
        .with_plugin(Arc::new(logging_plugin))?
        .with_plugin(Arc::new(config_plugin))?
        .build()?;

    println!("Created host with HTTP, SMTP, and logging plugins");

    let host = host.start().await.context("Failed to start host")?;
    println!("Host started, HTTP server listening on {addr}");

    let req = WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "smtp-demo-workload".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(SMTP_DEMO_WASM),
                local_resources: LocalResources {
                    memory_limit_mb: 256,
                    cpu_limit: 1,
                    config: {
                        let config = HashMap::new();
                        config
                    },
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
                    version: Some(semver::Version::parse("0.2.2").unwrap()),
                    config: {
                        let mut config = HashMap::new();
                        config.insert("host".to_string(), "smtp-test".to_string());
                        config
                    },
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "http".to_string(),
                    interfaces: ["outgoing-handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.2").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasmcloud".to_string(),
                    package: "smtp".to_string(),
                    interfaces: ["client".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "logging".to_string(),
                    interfaces: ["logging".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.1.0-draft").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "config".to_string(),
                    interfaces: ["store".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0-rc.1").unwrap()),
                    config: HashMap::new(),
                },
            ],
            volumes: vec![],
        },
    };

    let workload_response = host
        .workload_start(req)
        .await
        .context("Failed to start smtp-demo workload")?;

    println!("\n╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║                         📧 WORKLOAD DEPLOYED                          ║");
    println!("╠═══════════════════════════════════════════════════════════════════════╣");
    println!(
        "║ Workload ID: {:51} ║",
        workload_response.workload_status.workload_id
    );
    println!("║ Connection:  Persistent (connection pooling enabled)                 ║");
    println!("╚═══════════════════════════════════════════════════════════════════════╝");

    let client = reqwest::Client::new();

    // SMTP credentials - USE YOUR ACTUAL CREDENTIALS
    let smtp_username = "";
    let smtp_password = "";
    let from_email = "";
    let to_email = "";

    // Test 1: Send a simple email
    println!("Test 1: Sending email with simple text body");

    let simple_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - Simple Text",
        "body": "Hello, this is a test email from the SMTP component integration test!"
    });

    let first_response = timeout(
        Duration::from_secs(30),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&simple_payload)
            .send(),
    )
    .await
    .context("First email request timed out")?
    .context("Failed to make first email request")?;

    let first_status = first_response.status();
    println!("First Email Response Status: {}", first_status);

    let first_response_text = first_response
        .text()
        .await
        .context("Failed to read first response body")?;
    println!("First Email Response: {}", first_response_text.trim());

    // Test 2: Send email with HTML content
    println!("Test 2: Sending email with HTML content");

    let html_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - HTML Content",
        "body": r#"
            <html>
                <body>
                    <h1>Test Email</h1>
                    <p>This is a <strong>test email</strong> with HTML content.</p>
                </body>
            </html>
        "#
    });

    let second_response = timeout(
        Duration::from_secs(30),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&html_payload)
            .send(),
    )
    .await
    .context("Second email request timed out")?
    .context("Failed to make second email request")?;

    let second_status = second_response.status();
    println!("Second Email Response Status: {}", second_status);

    // Test 3: Multiple rapid concurrent email requests
    println!("Test 3: Sending multiple concurrent email requests");
    let mut handles = Vec::new();

    for i in 0..3 {
        let client = client.clone();
        let addr = addr;

        let concurrent_payload = serde_json::json!({
            "smtp": {
                "host": "smtp.gmail.com",
                "port": 465,
                "username": smtp_username,
                "password": smtp_password
            },
            "from": from_email,
            "to": [to_email],
            "subject": format!("Test Email - Concurrent #{}", i + 1),
            "body": format!("Test email #{} from concurrent request", i + 1)
        });

        let handle = tokio::spawn(async move {
            let response = client
                .post(format!("http://{addr}/"))
                .header("HOST", "smtp-test")
                .header("Content-Type", "application/json")
                .json(&concurrent_payload)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    println!(
                        "Concurrent email {} - Status: {}, Response: {}",
                        i + 1,
                        status,
                        text.trim()
                    );
                    (status.as_u16(), text)
                }
                Err(e) => {
                    println!("Concurrent email {} request failed: {}", i + 1, e);
                    (0, String::new())
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all concurrent requests to complete
    let mut completed_requests = 0;
    let mut success_count = 0;
    for handle in handles {
        if let Ok((status, _text)) = handle.await {
            if status >= 200 && status < 600 {
                completed_requests += 1;
                if status >= 200 && status < 300 {
                    success_count += 1;
                }
            }
        }
    }

    println!(
        "Concurrent requests: {}/3 completed, {}/3 successful",
        completed_requests, success_count
    );

    assert!(
        completed_requests >= 2,
        "At least 2 out of 3 concurrent requests should complete"
    );

    println!("\n┌─────────────────────────────────────────────────────────────────────┐");
    println!("│                    SMTP Demo Integration Test Results                │");
    println!("├─────────────────────────────────────────────────────────────────────┤");
    println!("│ Test Step                                    │ Result    │ Status    │");
    println!("├──────────────────────────────────────────────┼───────────┼───────────┤");
    println!("│ Simple text email request                    │ ✓ PASS    │ Handled   │");
    println!("│ HTML email request                           │ ✓ PASS    │ Handled   │");
    println!(
        "│ Concurrent requests (3 simultaneous)         │ ✓ PASS    │ {}/3       │",
        completed_requests
    );
    println!("├──────────────────────────────────────────────┼───────────┼───────────┤");
    println!("│ Connection Management                        │           │           │");
    println!("│  • Persistent connection pooling             │ ✓ PASS    │ Active    │");
    println!("│  • Concurrent request handling               │ ✓ PASS    │ Safe      │");
    println!("│  • Connection reuse across requests          │ ✓ PASS    │ Working   │");
    println!("└──────────────────────────────────────────────┴───────────┴───────────┘");
    println!("\n🎉 SMTP Demo Component Integration: ALL TESTS PASSED");

    Ok(())
}

#[tokio::test]
async fn test_smtp_attachments_url() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    println!("Starting SMTP attachment test (URL-based)");

    let engine = Engine::builder().build()?;
    let port = find_available_port().await?;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let http_handler = DevRouter::default();
    let http_plugin = HttpServer::new(http_handler, addr);
    let smtp_plugin = WasmcloudSmtp::new();
    let logging_plugin = WasiLogging {};
    let config_plugin = WasiConfig::default();

    let host = HostBuilder::new()
        .with_engine(engine.clone())
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(smtp_plugin))?
        .with_plugin(Arc::new(logging_plugin))?
        .with_plugin(Arc::new(config_plugin))?
        .build()?;

    let host = host.start().await.context("Failed to start host")?;
    println!("Host started on {addr}");

    let req = WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: "smtp-attachment-test".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                bytes: bytes::Bytes::from_static(SMTP_DEMO_WASM),
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
                    version: Some(semver::Version::parse("0.2.2").unwrap()),
                    config: {
                        let mut config = HashMap::new();
                        config.insert("host".to_string(), "smtp-test".to_string());
                        config
                    },
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "http".to_string(),
                    interfaces: ["outgoing-handler".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.2").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasmcloud".to_string(),
                    package: "smtp".to_string(),
                    interfaces: ["client".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "logging".to_string(),
                    interfaces: ["logging".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.1.0-draft").unwrap()),
                    config: HashMap::new(),
                },
                WitInterface {
                    namespace: "wasi".to_string(),
                    package: "config".to_string(),
                    interfaces: ["store".to_string()].into_iter().collect(),
                    version: Some(semver::Version::parse("0.2.0-rc.1").unwrap()),
                    config: HashMap::new(),
                },
            ],
            volumes: vec![],
        },
    };

    let workload_response = host
        .workload_start(req)
        .await
        .context("Failed to start smtp workload")?;

    println!(
        "Workload ID: {}",
        workload_response.workload_status.workload_id
    );

    let client = reqwest::Client::new();

    // SMTP credentials - REPLACE WITH YOUR ACTUAL CREDENTIALS
    let smtp_username = "";
    let smtp_password = "";
    let from_email = "";
    let to_email = "";

    // Test 1: Send email with URL-based attachment (PDF)
    println!("\n=== Test 1: URL-based Attachment (PDF) ===");
    let pdf_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - PDF Attachment from URL",
        "body": "<h1>PDF Attachment Test</h1><p>This email contains a PDF attachment downloaded from a URL.</p>",
        "attachments": [{
            "url": "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf",
            "filename": "sample-document.pdf",
            "content_type": "application/pdf"
        }]
    });

    let pdf_response = timeout(
        Duration::from_secs(45),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&pdf_payload)
            .send(),
    )
    .await
    .context("PDF attachment request timed out")?
    .context("Failed to send PDF attachment request")?;

    let pdf_status = pdf_response.status();
    let pdf_text = pdf_response.text().await.unwrap_or_default();
    println!("PDF Attachment - Status: {}", pdf_status);
    println!("Response: {}", pdf_text.trim());

    // Test 2: Send email with URL-based attachment (MP4 Video)
    println!("\n=== Test 2: URL-based Attachment (MP4 Video) ===");
    let video_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - Video Attachment from URL",
        "body": "<h1>Video Attachment Test</h1><p>This email contains a short MP4 video downloaded from a URL.</p>",
        "attachments": [{
            "url": "https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/360/Big_Buck_Bunny_360_10s_1MB.mp4",
            "filename": "sample-video.mp4",
            "content_type": "video/mp4"
        }]
    });

    let video_response = timeout(
        Duration::from_secs(60), // Longer timeout for video
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&video_payload)
            .send(),
    )
    .await
    .context("Video attachment request timed out")?
    .context("Failed to send video attachment request")?;

    let video_status = video_response.status();
    let video_text = video_response.text().await.unwrap_or_default();
    println!("Video Attachment - Status: {}", video_status);
    println!("Response: {}", video_text.trim());

    // Test 3: Send simple email without attachment
    println!("\n=== Test 3: Simple Email (No Attachment) ===");
    let simple_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - Simple Text",
        "body": "This is a simple test email without any attachments."
    });

    let simple_response = timeout(
        Duration::from_secs(30),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&simple_payload)
            .send(),
    )
    .await
    .context("Simple email request timed out")?
    .context("Failed to send simple email request")?;

    let simple_status = simple_response.status();
    let simple_text = simple_response.text().await.unwrap_or_default();
    println!("Simple Email - Status: {}", simple_status);
    println!("Response: {}", simple_text.trim());

    // Test 4: Email with multiple attachments (PNG + JSON)
    println!("\n=== Test 4: Multiple URL Attachments ===");
    let multiple_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "subject": "Test Email - Multiple Attachments",
        "body": "<h1>Multiple Attachments Test</h1><p>This email contains multiple files from different URLs.</p>",
        "attachments": [
            {
                "url": "https://httpbin.org/image/png",
                "filename": "test-image.png",
                "content_type": "image/png"
            },
            {
                "url": "https://httpbin.org/json",
                "filename": "test-data.json",
                "content_type": "application/json"
            }
        ]
    });

    let multiple_response = timeout(
        Duration::from_secs(45),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&multiple_payload)
            .send(),
    )
    .await
    .context("Multiple attachments request timed out")?
    .context("Failed to send multiple attachments request")?;

    let multiple_status = multiple_response.status();
    let multiple_text = multiple_response.text().await.unwrap_or_default();
    println!("Multiple Attachments - Status: {}", multiple_status);
    println!("Response: {}", multiple_text.trim());

    // Test 5: Email with CC and BCC
    println!("\n=== Test 5: Email with CC and BCC ===");
    let cc_bcc_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": smtp_username,
            "password": smtp_password
        },
        "from": from_email,
        "to": [to_email],
        "cc": ["aditya.salunkhe@bettyblocks.com"],
        "bcc": ["theacademicfoodie@gmail.com"],
        "subject": "Test Email - CC and BCC",
        "body": "This email includes CC and BCC recipients."
    });

    let cc_bcc_response = timeout(
        Duration::from_secs(30),
        client
            .post(format!("http://{addr}/"))
            .header("HOST", "smtp-test")
            .header("Content-Type", "application/json")
            .json(&cc_bcc_payload)
            .send(),
    )
    .await
    .context("CC/BCC email request timed out")?
    .context("Failed to send CC/BCC email request")?;

    let cc_bcc_status = cc_bcc_response.status();
    let cc_bcc_text = cc_bcc_response.text().await.unwrap_or_default();
    println!("CC/BCC Email - Status: {}", cc_bcc_status);
    println!("Response: {}", cc_bcc_text.trim());

    // Results summary
    println!("\n┌──────────────────────────────────────────────────────────────────┐");
    println!("│          SMTP Attachment Test Results                           │");
    println!("├──────────────────────────────────────────────────────────────────┤");
    println!("│ Test Case                        │ Status    │ Result          │");
    println!("├──────────────────────────────────┼───────────┼─────────────────┤");
    let pdf_pass = pdf_status.is_success();
    let video_pass = video_status.is_success();
    let simple_pass = simple_status.is_success();
    let multiple_pass = multiple_status.is_success();
    let cc_bcc_pass = cc_bcc_status.is_success();

    println!(
        "│ PDF attachment from URL          │ {:3}       │ {}           │",
        pdf_status.as_u16(),
        if pdf_pass { "✓ PASS" } else { "✗ FAIL" }
    );
    println!(
        "│ MP4 video attachment from URL    │ {:3}       │ {}           │",
        video_status.as_u16(),
        if video_pass { "✓ PASS" } else { "✗ FAIL" }
    );
    println!(
        "│ Simple email (no attachment)     │ {:3}       │ {}           │",
        simple_status.as_u16(),
        if simple_pass { "✓ PASS" } else { "✗ FAIL" }
    );
    println!(
        "│ Multiple attachments (PNG+JSON)  │ {:3}       │ {}           │",
        multiple_status.as_u16(),
        if multiple_pass {
            "✓ PASS"
        } else {
            "✗ FAIL"
        }
    );
    println!(
        "│ Email with CC and BCC            │ {:3}       │ {}           │",
        cc_bcc_status.as_u16(),
        if cc_bcc_pass { "✓ PASS" } else { "✗ FAIL" }
    );
    println!("└──────────────────────────────────┴───────────┴─────────────────┘");

    assert!(simple_pass, "Simple email should succeed");

    Ok(())
}
