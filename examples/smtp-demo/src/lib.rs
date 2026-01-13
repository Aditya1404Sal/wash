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

    let client_mutex = SMTP_CLIENT.get_or_init(|| Mutex::new(None));
    let mut client_state = client_mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("Failed to acquire lock: {e}"))?;

    if client_state.is_none() {
        log(Level::Info, "", "🔌 Initializing SMTP client connection");

        match try_connect_smtp() {
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

    let message = build_email_message(&body_content)?;
    log(
        Level::Info,
        "",
        &format!("📨 Sending: {} with attachment", message.subject),
    );

    let result = client
        .send(&message)
        .map_err(|e| anyhow::anyhow!("Send failed: {e}"))?;

    Ok(format!(
        "Email with attachment sent! Accepted: {}, Server: {}, ID: {}",
        result.accepted,
        result.server.unwrap_or_else(|| "unknown".to_string()),
        result.message_id.unwrap_or_else(|| "none".to_string())
    ))
}

fn try_connect_smtp() -> Result<SmtpClient> {
    // Try port 465 (implicit TLS)
    log(Level::Info, "", "🔄 Trying port 465 (SSL/TLS)...");
    let creds_465 = Credentials {
        host: "smtp.gmail.com".to_string(),
        port: 465,
        username: Some("aditya.salunkh919@gmail.com".to_string()),
        password: Some("qmbyfkcmnmcafwlo".to_string()), // <-- PUT YOUR 16-CHAR APP PASSWORD
        secure: Some(true),                             // Implicit TLS
        ignore_tls: Some(false),
        require_tls: Some(true),
    };

    match SmtpClient::connect(&creds_465) {
        Ok(client) => {
            log(Level::Info, "", "✅ Connected via port 465 (SSL/TLS)");
            return Ok(client);
        }
        Err(e) => {
            log(Level::Error, "", &format!("❌ Port 465 failed: {}", e));
            return Err(anyhow::anyhow!(
                "Both ports failed. Port 587: Check app password and 2FA. Port 465: {}",
                e
            ));
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

fn build_email_message(body_content: &str) -> Result<Message> {
    Ok(Message {
        sender: Sender {
            from: "aditya.salunkh919@gmail.com".to_string(),
            reply_to: None,
        },
        recipient: Recipient {
            to: vec!["soyabeanie101@gmail.com".to_string()],
            cc: None,
            bcc: None,
        },
        subject: "Test Email from SMTP Component with PDF Attachment".to_string(),
        body: body_content.to_string(),
        attachment: Some(vec![Attachment {
            filename: "UNIT06.pdf".to_string(),
            source: AttachmentSource::Path("/home/aditya-sal/Downloads/UNIT06.pdf".to_string()),
        }]),
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
