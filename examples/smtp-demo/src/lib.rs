use anyhow::{Context, Result};
use std::sync::{Mutex, OnceLock};

mod bindings {
    wit_bindgen::generate!({
        generate_all,
    });
}

use bindings::{
    exports::wasi::http::incoming_handler::Guest,
    wasi::{
        http::types::{Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam},
        io::streams::StreamError,
        logging::logging::{log, Level},
    },
    wasmcloud::smtp::client::{
        Attachment, AttachmentSource, Credentials, Message, Recipient, Sender, SmtpClient,
    },
};
use serde_json::Value;

struct Component;

enum SmtpClientState {
    Connected(SmtpClient),
    Failed(String),
}

static SMTP_CLIENT: OnceLock<Mutex<Option<SmtpClientState>>> = OnceLock::new();

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        match handle_request(request) {
            Ok(message) => {
                log(Level::Info, "", &format!("✅ {}", message));
                send_response(response_out, 200, message.as_bytes());
            }
            Err(e) => {
                log(Level::Error, "", &format!("❌ Error: {e}"));
                let error_msg = format!("Failed to send email: {e}");
                send_response(response_out, 500, error_msg.as_bytes());
            }
        }
    }
}

fn handle_request(request: IncomingRequest) -> Result<String> {
    log(Level::Info, "", "📧 Processing incoming SMTP request");

    let body_content = read_request_body(request)?;
    log(
        Level::Info,
        "",
        &format!("📥 Request body: {} bytes", body_content.len()),
    );

    // Parse the request to extract SMTP credentials
    let smtp_config = parse_smtp_config(&body_content)?;

    let client_mutex = SMTP_CLIENT.get_or_init(|| Mutex::new(None));
    let mut client_state = client_mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock: {e}"))?;

    if client_state.is_none() {
        log(Level::Info, "", "🔌 Initializing SMTP client connection");

        match try_connect_smtp(&smtp_config) {
            Ok(client) => {
                log(
                    Level::Info,
                    "",
                    "✅ SMTP connection established with pooling",
                );
                *client_state = Some(SmtpClientState::Connected(client));
            }
            Err(e) => {
                let error_msg = format!("{e}");
                log(
                    Level::Error,
                    "",
                    &format!("❌ Connection failed: {}", error_msg),
                );
                *client_state = Some(SmtpClientState::Failed(error_msg.clone()));
                return Err(anyhow::anyhow!(error_msg));
            }
        }
    }

    let client = match client_state.as_ref() {
        Some(SmtpClientState::Connected(client)) => client,
        Some(SmtpClientState::Failed(error)) => {
            return Err(anyhow::anyhow!("Previous connection failed: {}", error));
        }
        None => return Err(anyhow::anyhow!("Client not initialized")),
    };

    let message = build_email_message(&body_content, &smtp_config)?;
    let attachment_info = if message.attachment.is_some() {
        "with attachment"
    } else {
        "without attachment"
    };

    log(
        Level::Info,
        "",
        &format!("📨 Sending: {} {}", message.subject, attachment_info),
    );

    let result = client
        .send(&message)
        .map_err(|e| anyhow::anyhow!("Send failed: {e}"))?;

    Ok(format!(
        "Email {} sent! Accepted: {}, Server: {}, ID: {}",
        attachment_info,
        result.accepted,
        result.server.unwrap_or_else(|| "unknown".to_string()),
        result.message_id.unwrap_or_else(|| "none".to_string())
    ))
}

#[derive(Clone)]
struct SmtpConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    from: String,
    to: Vec<String>,
}

fn parse_smtp_config(body_content: &str) -> Result<SmtpConfig> {
    let json: Value = serde_json::from_str(body_content)
        .context("Request body must be valid JSON with SMTP configuration")?;

    // Parse SMTP credentials
    let smtp = json
        .get("smtp")
        .ok_or_else(|| anyhow::anyhow!("Missing 'smtp' configuration object"))?;

    let host = smtp
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("smtp.gmail.com")
        .to_string();

    let port = smtp.get("port").and_then(|v| v.as_u64()).unwrap_or(465) as u16;

    let username = smtp
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'smtp.username'"))?
        .to_string();

    let password = smtp
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'smtp.password'"))?
        .to_string();

    // Parse email addresses
    let from = json
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'from' email address"))?
        .to_string();

    let to = if let Some(to_val) = json.get("to") {
        let mut parsed = Vec::new();
        if to_val.is_array() {
            if let Some(arr) = to_val.as_array() {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        parsed.push(s.to_string());
                    }
                }
            }
        } else if let Some(s) = to_val.as_str() {
            parsed.push(s.to_string());
        }
        if parsed.is_empty() {
            return Err(anyhow::anyhow!(
                "'to' field must contain at least one email address"
            ));
        }
        parsed
    } else {
        return Err(anyhow::anyhow!("Missing 'to' email address(es)"));
    };

    log(
        Level::Info,
        "",
        &format!(
            "📧 SMTP Config: {}:{} (from: {}, to: {:?})",
            host, port, from, to
        ),
    );

    Ok(SmtpConfig {
        host,
        port,
        username,
        password,
        from,
        to,
    })
}

fn try_connect_smtp(config: &SmtpConfig) -> Result<SmtpClient> {
    log(
        Level::Info,
        "",
        &format!(
            "🔄 Connecting to {}:{} (SSL/TLS)...",
            config.host, config.port
        ),
    );

    let creds = Credentials {
        host: config.host.clone(),
        port: config.port,
        username: Some(config.username.clone()),
        password: Some(config.password.clone()),
        secure: Some(true),
        ignore_tls: Some(false),
        require_tls: Some(true),
    };

    match SmtpClient::connect(&creds) {
        Ok(client) => {
            log(
                Level::Info,
                "",
                &format!("✅ Connected to {}:{}", config.host, config.port),
            );
            Ok(client)
        }
        Err(e) => {
            log(Level::Error, "", &format!("❌ Connection failed: {}", e));
            Err(anyhow::anyhow!("Failed to connect to SMTP server: {}", e))
        }
    }
}

fn read_request_body(request: IncomingRequest) -> Result<String> {
    let body_stream = request
        .consume()
        .map_err(|_| anyhow::anyhow!("Failed to consume request"))?;

    let input_stream = body_stream
        .stream()
        .map_err(|_| anyhow::anyhow!("Failed to get stream"))?;

    let mut body_data = Vec::new();
    loop {
        match input_stream.read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => body_data.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(e) => return Err(anyhow::anyhow!("Stream error: {e:?}")),
        }
    }

    String::from_utf8(body_data).context("Invalid UTF-8 in request body")
}

fn build_email_message(body_content: &str, config: &SmtpConfig) -> Result<Message> {
    // Parse JSON for email content
    let json: Value = serde_json::from_str(body_content)?;

    let mut email_body = json
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("Test email from SMTP component")
        .to_string();

    let mut attachment: Option<Vec<Attachment>> = None;

    let subject = json
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("Test Email from SMTP Component")
        .to_string();

    // Parse filesystem attachment
    if let Some(path_val) = json.get("attachment_path").and_then(|v| v.as_str()) {
        log(
            Level::Info,
            "",
            &format!("📎 Adding filesystem attachment: {}", path_val),
        );

        let filename = std::path::Path::new(path_val)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "attachment".to_string());

        attachment = Some(vec![Attachment {
            filename,
            source: AttachmentSource::Path(path_val.to_string()),
        }]);
    }

    // Parse URL-based attachment (takes precedence if both are provided)
    if let Some(url_val) = json.get("attachment_url").and_then(|v| v.as_str()) {
        log(
            Level::Info,
            "",
            &format!("🌐 Adding URL-based attachment: {}", url_val),
        );

        let filename = std::path::Path::new(url_val)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "downloaded-attachment.pdf".to_string());

        attachment = Some(vec![Attachment {
            filename,
            source: AttachmentSource::Url(url_val.to_string()),
        }]);
    }

    // Optional CC and BCC
    let cc = json.get("cc").and_then(|v| {
        let mut emails = Vec::new();
        if v.is_array() {
            if let Some(arr) = v.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        emails.push(s.to_string());
                    }
                }
            }
        } else if let Some(s) = v.as_str() {
            emails.push(s.to_string());
        }
        if emails.is_empty() {
            None
        } else {
            Some(emails)
        }
    });

    let bcc = json.get("bcc").and_then(|v| {
        let mut emails = Vec::new();
        if v.is_array() {
            if let Some(arr) = v.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        emails.push(s.to_string());
                    }
                }
            }
        } else if let Some(s) = v.as_str() {
            emails.push(s.to_string());
        }
        if emails.is_empty() {
            None
        } else {
            Some(emails)
        }
    });

    Ok(Message {
        sender: Sender {
            from: config.from.clone(),
            reply_to: None,
        },
        recipient: Recipient {
            to: config.to.clone(),
            cc,
            bcc,
        },
        subject,
        body: email_body,
        attachment,
    })
}

fn send_response(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let response = OutgoingResponse::new(Fields::new());
    response.set_status_code(status).unwrap();
    let response_body = response.body().unwrap();
    ResponseOutparam::set(response_out, Ok(response));
    let stream = response_body.write().unwrap();
    stream.blocking_write_and_flush(body).unwrap();
    drop(stream);
    OutgoingBody::finish(response_body, None).unwrap();
}

bindings::export!(Component with_types_in bindings);
