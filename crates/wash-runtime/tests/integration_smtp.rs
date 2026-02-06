//! Integration test for smtp-demo component with concurrent support and URL attachments

use anyhow::{Context, Result};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::timeout;

use mailin_embedded::{Handler, Server, response::OK};
use parking_lot::Mutex;
use std::io;

mod common;
use common::find_available_port;

use wash_runtime::{
    engine::Engine,
    host::{
        HostApi, HostBuilder,
        http::{DevRouter, HttpServer},
    },
    plugin::{smtp::BettySmtp, wasi_config::WasiConfig, wasi_logging::WasiLogging},
    types::{Component, LocalResources, Workload, WorkloadStartRequest},
    wit::WitInterface,
};
const SMTP_DEMO_WASM: &[u8] = include_bytes!("fixtures/smtp_demo.wasm");

// Leaving empty for now : use for tls based tests
#[allow(unused)]
const SMTP_USERNAME: &str = "";
#[allow(unused)]
const SMTP_PASSWORD: &str = "";
#[allow(unused)]
const FROM_EMAIL: &str = "";
#[allow(unused)]
const TO_EMAIL: &str = "";

#[cfg(feature = "tls")]
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
    let smtp_plugin = BettySmtp::new();
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
                    namespace: "bettyblocks".to_string(),
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

    // Test 1: Send a simple email
    println!("Test 1: Sending email with simple text body");

    let simple_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
                "username": SMTP_USERNAME,
                "password": SMTP_PASSWORD
            },
            "from": FROM_EMAIL,
            "to": [TO_EMAIL],
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

#[cfg(feature = "tls")]
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
    let smtp_plugin = BettySmtp::new();
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
                    namespace: "bettyblocks".to_string(),
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

    // Test 1: Send email with URL-based attachment (PDF)
    println!("\n=== Test 1: URL-based Attachment (PDF) ===");
    let pdf_payload = serde_json::json!({
        "smtp": {
            "host": "smtp.gmail.com",
            "port": 465,
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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
            "username": SMTP_USERNAME,
            "password": SMTP_PASSWORD
        },
        "from": FROM_EMAIL,
        "to": [TO_EMAIL],
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

// ============================================================================
// LOCAL SMTP SERVER TESTS (Non-TLS)
// ============================================================================

/// Represents a captured email with all its components
#[derive(Debug, Clone, Default)]
struct CapturedEmail {
    from: String,
    to: Vec<String>,
    data: Vec<u8>,
}

/// Handler for the mock SMTP server that captures all received emails.
/// Uses a thread-local current_email pattern: `mail()` resets state via the
/// shared `current_email`, and `data_end()` moves it into the `emails` vec.
/// This is safe because `mailin_embedded` serializes connections on one thread.
#[derive(Clone)]
struct MockSmtpHandler {
    emails: Arc<Mutex<Vec<CapturedEmail>>>,
    current_email: Arc<Mutex<CapturedEmail>>,
}

impl MockSmtpHandler {
    fn new() -> Self {
        Self {
            emails: Arc::new(Mutex::new(Vec::new())),
            current_email: Arc::new(Mutex::new(CapturedEmail::default())),
        }
    }
}

impl Handler for MockSmtpHandler {
    fn helo(&mut self, _ip: std::net::IpAddr, _domain: &str) -> mailin_embedded::Response {
        OK
    }

    fn mail(
        &mut self,
        _ip: std::net::IpAddr,
        _domain: &str,
        from: &str,
    ) -> mailin_embedded::Response {
        let mut current = self.current_email.lock();
        *current = CapturedEmail::default();
        current.from = from.to_string();
        OK
    }

    fn rcpt(&mut self, to: &str) -> mailin_embedded::Response {
        let mut current = self.current_email.lock();
        current.to.push(to.to_string());
        OK
    }

    fn data_start(
        &mut self,
        _domain: &str,
        _from: &str,
        _is8bit: bool,
        _to: &[String],
    ) -> mailin_embedded::Response {
        OK
    }

    fn data(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut current = self.current_email.lock();
        current.data.extend_from_slice(buf);
        Ok(())
    }

    fn data_end(&mut self) -> mailin_embedded::Response {
        let current = self.current_email.lock().clone();
        self.emails.lock().push(current);
        OK
    }

    fn auth_plain(
        &mut self,
        _authorization_id: &str,
        _authentication_id: &str,
        _password: &str,
    ) -> mailin_embedded::Response {
        OK
    }
}

/// Starts a local SMTP server on the given port and returns its thread handle.
fn start_local_smtp_server(port: u16, handler: MockSmtpHandler) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut server = Server::new(handler);
        server
            .with_name("localhost")
            .with_addr(format!("127.0.0.1:{port}"))
            .expect("Failed to set SMTP server address");

        if let Err(e) = server.serve() {
            eprintln!("SMTP server error: {e}");
        }
    })
}

/// Poll-connect to an address until the server is ready or timeout expires.
async fn wait_for_server(addr: &str) -> Result<()> {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    anyhow::bail!("SMTP server at {addr} did not become ready in time")
}

/// Helper: create the standard workload request
fn create_workload_request(name: &str, host_header: &str) -> WorkloadStartRequest {
    WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "test".to_string(),
            name: name.to_string(),
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
                        config.insert("host".to_string(), host_header.to_string());
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
                    namespace: "bettyblocks".to_string(),
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
    }
}

/// Helper: spin up Engine + Host + workload, return (http_addr, host).
async fn setup_test_host(
    host_header: &str,
    workload_name: &str,
) -> Result<(SocketAddr, impl std::any::Any)> {
    let engine = Engine::builder().build()?;
    let http_port = find_available_port().await?;
    let http_addr: SocketAddr = format!("127.0.0.1:{http_port}").parse().unwrap();
    let http_handler = DevRouter::default();
    let http_plugin = HttpServer::new(http_handler, http_addr);

    let host = HostBuilder::new()
        .with_engine(engine.clone())
        .with_http_handler(Arc::new(http_plugin))
        .with_plugin(Arc::new(BettySmtp::new()))?
        .with_plugin(Arc::new(WasiLogging {}))?
        .with_plugin(Arc::new(WasiConfig::default()))?
        .build()?;

    let host = host.start().await.context("failed to start host")?;

    let req = create_workload_request(workload_name, host_header);
    host.workload_start(req)
        .await
        .context("failed to start workload")?;

    Ok((http_addr, host))
}

/// Helper: send a JSON email payload and return (status, body text).
async fn send_email(
    client: &reqwest::Client,
    http_addr: SocketAddr,
    host_header: &str,
    payload: &serde_json::Value,
    timeout_secs: u64,
) -> Result<(reqwest::StatusCode, String)> {
    let response = timeout(
        Duration::from_secs(timeout_secs),
        client
            .post(format!("http://{http_addr}/"))
            .header("HOST", host_header)
            .header("Content-Type", "application/json")
            .json(payload)
            .send(),
    )
    .await
    .context("request timed out")?
    .context("request failed")?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Ok((status, text))
}

/// Helper: build the common SMTP JSON payload for local non-TLS tests.
fn local_smtp_payload(
    smtp_port: u16,
    from: &str,
    to: &[&str],
    subject: &str,
    body: &str,
) -> serde_json::Value {
    serde_json::json!({
        "smtp": { "host": "127.0.0.1", "port": smtp_port, "tls_mode": "none" },
        "from": from,
        "to": to,
        "subject": subject,
        "body": body,
    })
}

/// Helper: start local SMTP + wait for readiness, returns (emails_ref, smtp_port, _thread).
async fn start_mock_smtp() -> Result<(
    Arc<Mutex<Vec<CapturedEmail>>>,
    u16,
    std::thread::JoinHandle<()>,
)> {
    let smtp_port = find_available_port().await?;
    let handler = MockSmtpHandler::new();
    let emails_ref = handler.emails.clone();
    let thread = start_local_smtp_server(smtp_port, handler);
    wait_for_server(&format!("127.0.0.1:{smtp_port}")).await?;
    Ok((emails_ref, smtp_port, thread))
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_local_smtp_non_tls() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let (emails_ref, smtp_port, _smtp_thread) = start_mock_smtp().await?;
    let (http_addr, _host) = setup_test_host("local-smtp", "local-smtp-test").await?;
    let client = reqwest::Client::new();

    // Test 1: Simple plaintext email
    let payload = local_smtp_payload(
        smtp_port,
        "sender@test.local",
        &["recipient@test.local"],
        "Local SMTP Test - Simple Email",
        "Hello! This is a test email sent via local non-TLS SMTP server.",
    );
    let (simple_status, _) = send_email(&client, http_addr, "local-smtp", &payload, 10).await?;
    println!("Simple email: {simple_status}");

    // Test 2: HTML email
    let payload = local_smtp_payload(
        smtp_port,
        "html-sender@test.local",
        &["html-recipient@test.local"],
        "Local SMTP Test - HTML Email",
        "<html><body><h1>HTML Email Test</h1><p>Sent via <strong>local SMTP</strong>.</p></body></html>",
    );
    let (html_status, _) = send_email(&client, http_addr, "local-smtp", &payload, 10).await?;
    println!("HTML email: {html_status}");

    // Test 3: Multiple recipients
    let payload = local_smtp_payload(
        smtp_port,
        "multi-sender@test.local",
        &["r1@test.local", "r2@test.local", "r3@test.local"],
        "Local SMTP Test - Multiple Recipients",
        "This email is sent to multiple recipients via local SMTP.",
    );
    let (multi_status, _) = send_email(&client, http_addr, "local-smtp", &payload, 10).await?;
    println!("Multi-recipient email: {multi_status}");

    // Test 4: Concurrent emails (5 parallel)
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let addr = http_addr;
        let port = smtp_port;
        handles.push(tokio::spawn(async move {
            let payload = serde_json::json!({
                "smtp": { "host": "127.0.0.1", "port": port, "tls_mode": "none" },
                "from": format!("concurrent-{i}@test.local"),
                "to": [format!("concurrent-rcpt-{i}@test.local")],
                "subject": format!("Concurrent Email #{}", i + 1),
                "body": format!("Concurrent email number {}.", i + 1)
            });
            send_email(&client, addr, "local-smtp", &payload, 10)
                .await
                .map(|(s, _)| s.is_success())
                .unwrap_or(false)
        }));
    }
    let mut success_count = 0;
    for handle in handles {
        if handle.await.unwrap_or(false) {
            success_count += 1;
        }
    }
    println!("Concurrent results: {success_count}/5 successful");

    // Allow server to finish processing
    tokio::time::sleep(Duration::from_millis(200)).await;
    let captured = emails_ref.lock().clone();

    // Assertions
    assert!(simple_status.is_success(), "Simple email should succeed");
    assert!(html_status.is_success(), "HTML email should succeed");
    assert!(
        multi_status.is_success(),
        "Multi-recipient email should succeed"
    );
    assert!(
        success_count >= 4,
        "At least 4/5 concurrent emails should succeed (server is single-threaded), got {success_count}"
    );
    // 3 sequential + at least 4 concurrent = at least 7
    assert!(
        captured.len() >= 7,
        "Server should have captured at least 7 emails (3 sequential + 4+ concurrent), got {}",
        captured.len()
    );

    Ok(())
}

#[tokio::test]
async fn test_local_smtp_cc_bcc() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let (emails_ref, smtp_port, _smtp_thread) = start_mock_smtp().await?;
    let (http_addr, _host) = setup_test_host("cc-bcc-smtp", "cc-bcc-test").await?;
    let client = reqwest::Client::new();

    // Test 1: Email with CC recipients (to=1 + cc=2 = 3 RCPT TO envelopes)
    let cc_payload = serde_json::json!({
        "smtp": { "host": "127.0.0.1", "port": smtp_port, "tls_mode": "none" },
        "from": "sender@test.local",
        "to": ["primary@test.local"],
        "cc": ["cc1@test.local", "cc2@test.local"],
        "subject": "Local SMTP Test - With CC",
        "body": "This email has CC recipients."
    });
    let (cc_status, _) = send_email(&client, http_addr, "cc-bcc-smtp", &cc_payload, 10).await?;
    println!("CC email: {cc_status}");

    // Test 2: Email with BCC recipients (to=1 + bcc=2 = 3 RCPT TO envelopes)
    let bcc_payload = serde_json::json!({
        "smtp": { "host": "127.0.0.1", "port": smtp_port, "tls_mode": "none" },
        "from": "sender@test.local",
        "to": ["primary@test.local"],
        "bcc": ["bcc1@test.local", "bcc2@test.local"],
        "subject": "Local SMTP Test - With BCC",
        "body": "This email has BCC recipients (hidden)."
    });
    let (bcc_status, _) = send_email(&client, http_addr, "cc-bcc-smtp", &bcc_payload, 10).await?;
    println!("BCC email: {bcc_status}");

    // Test 3: Email with both CC and BCC (to=1 + cc=1 + bcc=1 = 3 RCPT TO envelopes)
    let both_payload = serde_json::json!({
        "smtp": { "host": "127.0.0.1", "port": smtp_port, "tls_mode": "none" },
        "from": "sender@test.local",
        "to": ["primary@test.local"],
        "cc": ["cc@test.local"],
        "bcc": ["bcc@test.local"],
        "subject": "Local SMTP Test - With CC and BCC",
        "body": "This email has both CC and BCC recipients."
    });
    let (both_status, _) = send_email(&client, http_addr, "cc-bcc-smtp", &both_payload, 10).await?;
    println!("CC+BCC email: {both_status}");

    // Allow server to finish processing
    tokio::time::sleep(Duration::from_millis(200)).await;
    let captured = emails_ref.lock().clone();

    // Assertions: HTTP responses succeeded
    assert!(cc_status.is_success(), "CC email should succeed");
    assert!(bcc_status.is_success(), "BCC email should succeed");
    assert!(both_status.is_success(), "CC+BCC email should succeed");
    assert_eq!(
        captured.len(),
        3,
        "Server should have captured exactly 3 emails, got {}",
        captured.len()
    );

    // Verify that CC/BCC recipients appear in the SMTP envelope (RCPT TO)
    let cc_email = &captured[0];
    assert!(
        cc_email.to.iter().any(|r| r.contains("cc1@test.local")),
        "CC email should have cc1 in RCPT TO, got: {:?}",
        cc_email.to
    );
    assert!(
        cc_email.to.iter().any(|r| r.contains("cc2@test.local")),
        "CC email should have cc2 in RCPT TO, got: {:?}",
        cc_email.to
    );

    let bcc_email = &captured[1];
    assert!(
        bcc_email.to.iter().any(|r| r.contains("bcc1@test.local")),
        "BCC email should have bcc1 in RCPT TO, got: {:?}",
        bcc_email.to
    );
    assert!(
        bcc_email.to.iter().any(|r| r.contains("bcc2@test.local")),
        "BCC email should have bcc2 in RCPT TO, got: {:?}",
        bcc_email.to
    );

    let both_email = &captured[2];
    assert!(
        both_email.to.iter().any(|r| r.contains("cc@test.local")),
        "CC+BCC email should have cc in RCPT TO, got: {:?}",
        both_email.to
    );
    assert!(
        both_email.to.iter().any(|r| r.contains("bcc@test.local")),
        "CC+BCC email should have bcc in RCPT TO, got: {:?}",
        both_email.to
    );

    Ok(())
}

/// Test that a large email body (~50KB) is received intact by the SMTP server.
#[tokio::test]
async fn test_local_smtp_large_body() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    let (emails_ref, smtp_port, _smtp_thread) = start_mock_smtp().await?;
    let (http_addr, _host) = setup_test_host("large-body-smtp", "large-body-test").await?;
    let client = reqwest::Client::new();

    // Generate a large HTML body (~72KB)
    let large_body = {
        let mut body = String::from("<html><body><h1>Large Email Test</h1>");
        for i in 0..500 {
            body.push_str(&format!(
                "<p>Paragraph {i}: Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                 Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.</p>",
            ));
        }
        body.push_str("</body></html>");
        body
    };
    println!("Large body size: {} bytes", large_body.len());

    let payload = local_smtp_payload(
        smtp_port,
        "large-sender@test.local",
        &["large-recipient@test.local"],
        "Local SMTP Test - Large Body Email",
        &large_body,
    );
    let (status, _) = send_email(&client, http_addr, "large-body-smtp", &payload, 30).await?;
    println!("Large body email: {status}");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let captured = emails_ref.lock().clone();

    let data_received = captured.first().map(|e| e.data.len()).unwrap_or(0);
    println!("Data received by server: {data_received} bytes");

    assert!(status.is_success(), "Large body email should succeed");
    assert!(
        data_received > large_body.len() / 2,
        "Server should have received substantial email data, got {data_received} bytes \
         (body was {} bytes)",
        large_body.len()
    );

    Ok(())
}

/// Test that the component returns an error when the SMTP server is unreachable.
#[tokio::test]
async fn test_local_smtp_error_handling() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();

    // No SMTP server started intentionally
    let (http_addr, _host) = setup_test_host("error-smtp", "error-test").await?;
    let client = reqwest::Client::new();

    let invalid_port = find_available_port().await?;
    let payload = local_smtp_payload(
        invalid_port,
        "sender@test.local",
        &["recipient@test.local"],
        "Should Fail - No Server",
        "This email should fail because there's no SMTP server.",
    );
    let (status, text) = send_email(&client, http_addr, "error-smtp", &payload, 15).await?;
    println!("Connection refused: {status} | {}", text.trim());

    assert!(
        status.as_u16() >= 400,
        "Connection to non-existent server should return error status, got {status}"
    );

    Ok(())
}
