use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use waki::Client;

mod bindings {
    wit_bindgen::generate!({
        generate_all,
    });
}

use bindings::{
    bettyblocks::smtp::client::{Attachment, Credentials, Message, Recipient, Sender, SmtpClient, TlsMode},
    exports::wasi::http::incoming_handler::Guest,
    wasi::{
        http::types::{
            Fields, IncomingRequest, OutgoingBody, OutgoingResponse,
            ResponseOutparam,
        },
        io::streams::StreamError,
        logging::logging::{log, Level},
    },
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        match handle_request(request) {
            Ok(message) => {
                log(Level::Info, "", &format!("{}", message));
                send_response(response_out, 200, message.as_bytes());
            }
            Err(e) => {
                log(Level::Error, "", &format!("Error: {e}"));
                let error_msg = format!("Failed to send email: {e}");
                send_response(response_out, 500, error_msg.as_bytes());
            }
        }
    }
}

fn handle_request(request: IncomingRequest) -> Result<String> {
    log(Level::Info, "", "Processing incoming SMTP request");

    let body_content = read_request_body(request)?;

    let smtp_config: SmtpConfig = serde_json::from_str(&body_content)
        .context("Request body must be valid JSON with SMTP configuration")?;

    let client = connect_smtp(&smtp_config)?;

    let message = build_email_message(&body_content, &smtp_config)?;

    let attachment_info = if message.attachments.is_some() {
        "with attachment"
    } else {
        "without attachment"
    };

    log(
        Level::Info,
        "",
        &format!("Sending: {} {}", message.subject, attachment_info),
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

#[derive(Clone, Deserialize)]
struct SmtpConfig {
    smtp: SmtpCredentials,
    from: String,
    to: ToRecipients,
}

#[derive(Clone, Deserialize)]
struct SmtpCredentials {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
    username: Option<String>,
    password: Option<String>,
    tls_mode: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum ToRecipients {
    Single(String),
    Multiple(Vec<String>),
}

impl ToRecipients {
    fn into_vec(self) -> Vec<String> {
        match self {
            ToRecipients::Single(s) => vec![s],
            ToRecipients::Multiple(v) => v,
        }
    }
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_port() -> u16 {
    25
}

fn connect_smtp(config: &SmtpConfig) -> Result<SmtpClient> {
    let (tls_mode, tls_mode_str) = if let Some(ref mode_str) = config.smtp.tls_mode {
        match mode_str.to_lowercase().as_str() {
            "none" | "plaintext" => (TlsMode::None, "None (plaintext)"),
            "starttls" | "explicit" => (TlsMode::Starttls, "Explicit (STARTTLS)"),
            "implicit" | "ssl" | "tls" => (TlsMode::Implicit, "Implicit (SSL)"),
            _ => {
                log(
                    Level::Warn,
                    "",
                    &format!("Unknown TLS mode '{}', falling back to port-based detection", mode_str),
                );
                match config.smtp.port {
                    465 => (TlsMode::Implicit, "Implicit (SSL)"),
                    25 => (TlsMode::None, "None (plaintext)"),
                    _ => (TlsMode::Starttls, "Explicit (STARTTLS)"),
                }
            }
        }
    } else {
        match config.smtp.port {
            465 => (TlsMode::Implicit, "Implicit (SSL)"),
            25 => (TlsMode::None, "None (plaintext)"),
            _ => (TlsMode::Starttls, "Explicit (STARTTLS)"),
        }
    };

    log(
        Level::Info,
        "",
        &format!(
            "Connecting to {}:{} using {}...",
            config.smtp.host, config.smtp.port, tls_mode_str
        ),
    );

    let creds = Credentials {
        host: config.smtp.host.clone(),
        port: Some(config.smtp.port),
        username: config.smtp.username.clone(),
        password: config.smtp.password.clone(),
        tls_mode,
    };

    match SmtpClient::connect(&creds) {
        Ok(client) => {
            log(
                Level::Info,
                "",
                &format!("Connected to {}:{}", config.smtp.host, config.smtp.port),
            );
            Ok(client)
        }
        Err(e) => {
            log(Level::Error, "", &format!("Connection failed: {}", e));
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

fn download_from_url(url: &str) -> Result<Vec<u8>> {
    log(
        Level::Info,
        "",
        &format!("Downloading attachment from: {}", url),
    );

    let client = Client::new();
    let response = client
        .get(url)
        .send()
        .map_err(|e| anyhow::anyhow!("Failed to send HTTP request: {e:?}"))?;

    let status = response.status_code();
    if status < 200 || status >= 300 {
        return Err(anyhow::anyhow!(
            "HTTP request failed with status code: {}",
            status
        ));
    }

    log(
        Level::Info,
        "",
        &format!("Received response with status: {}", status),
    );

    let data = response
        .body()
        .map_err(|e| anyhow::anyhow!("Failed to read response body: {e:?}"))?;

    log(
        Level::Info,
        "",
        &format!("Downloaded {} bytes from URL", data.len()),
    );

    Ok(data)
}

fn extract_filename_from_url(url: &str) -> String {
    url.split('/')
        .last()
        .and_then(|s| s.split('?').next())
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

    if let Some(attachments_val) = json.get("attachments") {
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
                    &format!("Fetching attachment from URL: {}", url),
                );

                let content = download_from_url(url)
                    .with_context(|| format!("Failed to download attachment from URL: {}", url))?;

                let filename = attachment_obj
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| extract_filename_from_url(url));

                let content_type = attachment_obj
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                log(
                    Level::Info,
                    "",
                    &format!(
                        "Downloaded {} bytes as '{}' ({})",
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
            from: config.from.clone(),
            reply_to: None,
            display_name: None,
        },
        recipient: Recipient {
            to: config.to.clone().into_vec(),
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
