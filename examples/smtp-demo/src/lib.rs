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
        http::outgoing_handler,
        http::types::{
            Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
            ResponseOutparam, Scheme,
        },
        io::streams::StreamError,
        logging::logging::{log, Level},
    },
    wasmcloud::smtp::client::{Attachment, Credentials, Message, Recipient, Sender, SmtpClient},
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

    // Parse the request to extract SMTP credentials
    let smtp_config = parse_smtp_config(&body_content)?;

    // Handle Connection Pooling
    let client_mutex = SMTP_CLIENT.get_or_init(|| Mutex::new(None));
    let mut client_state = client_mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock: {e}"))?;

    // Connect if not already connected
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

    // Retrieve client from state
    let client = match client_state.as_ref() {
        Some(SmtpClientState::Connected(client)) => client,
        Some(SmtpClientState::Failed(error)) => {
            // Retry logic could go here, but for now we fail fast
            return Err(anyhow::anyhow!("Previous connection failed: {}", error));
        }
        None => return Err(anyhow::anyhow!("Client not initialized")),
    };

    // Build message (Downloads files from URLs if needed)
    let message = build_email_message(&body_content, &smtp_config)?;

    let attachment_info = if message.attachments.is_some() {
        "with attachment"
    } else {
        "without attachment"
    };

    log(
        Level::Info,
        "",
        &format!("📨 Sending: {} {}", message.subject, attachment_info),
    );

    // Send via the Provider
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
    // Automatic TLS Selection Logic
    // Port 465 -> Implicit TLS (SSL)
    // Port 587/25/2525 -> Explicit TLS (STARTTLS)
    let implicit_tls = config.port == 465;

    let tls_mode = if implicit_tls {
        "Implicit (SSL)"
    } else {
        "Explicit (STARTTLS)"
    };

    log(
        Level::Info,
        "",
        &format!(
            "🔄 Connecting to {}:{} using {}...",
            config.host, config.port, tls_mode
        ),
    );

    let creds = Credentials {
        host: config.host.clone(),
        port: Some(config.port),
        username: config.username.clone(),
        password: config.password.clone(),
        implicit_tls,
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
        match input_stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => body_data.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(e) => return Err(anyhow::anyhow!("Stream error: {e:?}")),
        }
    }

    String::from_utf8(body_data).context("Invalid UTF-8 in request body")
}

/// Downloads a file from a URL using WASI HTTP outgoing handler
fn download_from_url(url: &str) -> Result<Vec<u8>> {
    log(
        Level::Info,
        "",
        &format!("🌐 Downloading attachment from: {}", url),
    );

    // Parse the URL to extract components
    let parsed_url = parse_url(url)?;

    // Create outgoing request
    let headers = Fields::new();
    let outgoing_request = OutgoingRequest::new(headers);

    // Set the request method to GET
    outgoing_request
        .set_method(&Method::Get)
        .map_err(|_| anyhow::anyhow!("Failed to set method"))?;

    // Set the scheme (http or https)
    outgoing_request
        .set_scheme(Some(&parsed_url.scheme))
        .map_err(|_| anyhow::anyhow!("Failed to set scheme"))?;

    // Set the authority (host:port)
    outgoing_request
        .set_authority(Some(&parsed_url.authority))
        .map_err(|_| anyhow::anyhow!("Failed to set authority"))?;

    // Set the path and query
    outgoing_request
        .set_path_with_query(Some(&parsed_url.path_and_query))
        .map_err(|_| anyhow::anyhow!("Failed to set path"))?;

    log(
        Level::Info,
        "",
        &format!("📤 Sending HTTP request to {}", url),
    );

    // Send the request
    let future_response = outgoing_handler::handle(outgoing_request, None)
        .map_err(|e| anyhow::anyhow!("Failed to send HTTP request: {e:?}"))?;

    // Wait for the response
    let incoming_response = match future_response.get() {
        Some(result) => result.map_err(|e| anyhow::anyhow!("HTTP request failed: {e:?}"))?,
        None => {
            future_response.subscribe().block();
            future_response
                .get()
                .ok_or_else(|| anyhow::anyhow!("Failed to get response"))?
                .map_err(|e| anyhow::anyhow!("HTTP request failed: {e:?}"))?
        }
    }
    .map_err(|e| anyhow::anyhow!("HTTP response error: {e:?}"))?;

    // Check status code
    let status = incoming_response.status();
    if status < 200 || status >= 300 {
        return Err(anyhow::anyhow!(
            "HTTP request failed with status code: {}",
            status
        ));
    }

    log(
        Level::Info,
        "",
        &format!("✅ Received response with status: {}", status),
    );

    // Read the response body
    let response_body = incoming_response
        .consume()
        .map_err(|_| anyhow::anyhow!("Failed to consume response"))?;

    let input_stream = response_body
        .stream()
        .map_err(|_| anyhow::anyhow!("Failed to get response stream"))?;

    let mut data = Vec::new();
    loop {
        match input_stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => data.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Stream error while reading response: {e:?}"
                ))
            }
        }
    }

    log(
        Level::Info,
        "",
        &format!("📦 Downloaded {} bytes from URL", data.len()),
    );

    Ok(data)
}

/// Simple URL parser for extracting scheme, authority, and path
struct ParsedUrl {
    scheme: Scheme,
    authority: String,
    path_and_query: String,
}

fn parse_url(url: &str) -> Result<ParsedUrl> {
    // Split scheme
    let (scheme_str, rest) = url
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("Invalid URL: missing scheme"))?;

    let scheme = match scheme_str.to_lowercase().as_str() {
        "http" => Scheme::Http,
        "https" => Scheme::Https,
        other => Scheme::Other(other.to_string()),
    };

    // Split authority and path
    let (authority, path_and_query) = if let Some(idx) = rest.find('/') {
        let (auth, path) = rest.split_at(idx);
        (auth.to_string(), path.to_string())
    } else {
        (rest.to_string(), "/".to_string())
    };

    Ok(ParsedUrl {
        scheme,
        authority,
        path_and_query,
    })
}

/// Extracts filename from URL (last segment of path)
fn extract_filename_from_url(url: &str) -> String {
    url.split('/')
        .last()
        .and_then(|s| s.split('?').next()) // Remove query params
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "downloaded_attachment".to_string())
}

fn build_email_message(body_content: &str, config: &SmtpConfig) -> Result<Message> {
    let json: Value = serde_json::from_str(body_content)?;

    let email_body = json
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("Test email from SMTP component")
        .to_string();

    let subject = json
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("Test Email from SMTP Component")
        .to_string();

    let mut attachments: Option<Vec<Attachment>> = None;

    // Handle URL Attachment(s) - Download from URL using WASI HTTP outgoing handler
    if let Some(attachments_val) = json.get("attachments") {
        // Support array of attachment objects with url and optional content_type
        if let Some(attachments_array) = attachments_val.as_array() {
            let mut attachment_list = Vec::new();

            for attachment_obj in attachments_array {
                let url = attachment_obj
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'url' in attachment object"))?;

                log(
                    Level::Info,
                    "",
                    &format!("🌐 Fetching attachment from URL: {}", url),
                );

                // Download the file from the URL
                let content = download_from_url(url)
                    .with_context(|| format!("Failed to download attachment from URL: {}", url))?;

                // Use provided filename or extract from URL
                let filename = attachment_obj
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| extract_filename_from_url(url));

                // Use provided content_type or default to octet-stream
                let content_type = attachment_obj
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                log(
                    Level::Info,
                    "",
                    &format!(
                        "📎 Downloaded {} bytes as '{}' ({})",
                        content.len(),
                        filename,
                        content_type
                    ),
                );

                attachment_list.push(Attachment {
                    filename,
                    content_type,
                    content,
                });
            }

            if !attachment_list.is_empty() {
                attachments = Some(attachment_list);
            }
        }
    }

    // Helper to parse email lists
    let parse_list = |key: &str| -> Option<Vec<String>> {
        json.get(key).and_then(|v| {
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
        })
    };

    Ok(Message {
        sender: Sender {
            address: config.from.clone(),
            reply_to: None,
            name: None,
        },
        recipient: Recipient {
            to: config.to.clone(),
            cc: parse_list("cc"),
            bcc: parse_list("bcc"),
        },
        subject,
        body: email_body,
        attachments,
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
