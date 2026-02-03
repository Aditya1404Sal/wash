use serde_json::json;

wit_bindgen::generate!({
    world: "mcp",
    generate_all,
});

use exports::wasi::http::incoming_handler::Guest as McpHandler;

mod actions;
mod config;
mod mcp;
mod types;

use crate::betty_blocks::auth::jwt::{validate_token, AuthError};

struct Component;

fn format_auth_error(err: &AuthError) -> String {
    match err {
        AuthError::MissingHeader => "missing Authorization header".to_string(),
        AuthError::InvalidFormat => "invalid Authorization header format".to_string(),
        AuthError::MalformedToken => "malformed JWT token".to_string(),
        AuthError::UnsupportedAlgorithm(alg) => format!("unsupported algorithm: {alg}"),
        AuthError::MissingConfig(key) => format!("missing configuration: {key}"),
        AuthError::InvalidPublicKey(msg) => format!("invalid public key: {msg}"),
        AuthError::ValidationFailed(msg) => format!("validation failed: {msg}"),
    }
}

impl McpHandler for Component {
    fn handle(
        request: crate::wasi::http::types::IncomingRequest,
        response_out: crate::wasi::http::types::ResponseOutparam,
    ) {
        inner_handle(request, response_out);
    }
}

fn inner_handle(
    request: crate::wasi::http::types::IncomingRequest,
    response_out: crate::wasi::http::types::ResponseOutparam,
) {
    use crate::wasi::http::types::Method;

    // Match on method and path pattern
    match (request.method(), request.path_with_query().as_deref()) {
        (Method::Post, Some(path)) if path.starts_with("/mcp/") => {
            handle_mcp_request(request, response_out, path);
        }
        _ => {
            eprintln!("[MCP-COMPONENT] No route matched, returning 405 Method Not Allowed");
            send_error_response(
                response_out,
                405,
                "Method Not Allowed. Expected POST to /mcp/{{server-id}}".to_string(),
            );
        }
    }
}

fn handle_mcp_request(
    request: crate::wasi::http::types::IncomingRequest,
    response_out: crate::wasi::http::types::ResponseOutparam,
    path: &str,
) {
    // Step 1: Extract server ID from path
    let server_id = match extract_server_id_from_path(path) {
        Ok(id) => id,
        Err(e) => {
            send_error_response(response_out, 400, e);
            return;
        }
    };

    // Step 2: Validate Content-Type as application/json
    if let Err(e) = validate_content_type(&request) {
        send_error_response(response_out, 400, e);
        return;
    }

    // Step 3: Authenticate request (JWT)
    let headers = request
        .headers()
        .entries()
        .into_iter()
        .map(|(k, v)| (k, String::from_utf8_lossy(&v).to_string()))
        .collect::<Vec<_>>();
    if let Err(auth_err) = validate_token(&headers) {
        let error_msg = format_auth_error(&auth_err);
        eprintln!("[MCP-COMPONENT] JWT validation failed: {error_msg}");
        send_error_response(response_out, 401, error_msg);
        return;
    }

    // Step 4: Read request body
    let body = match read_request_body(&request) {
        Ok(b) => b,
        Err(e) => {
            send_error_response(response_out, 400, e);
            return;
        }
    };

    // Step 5: Process MCP RPC request (validates server_id + JSON-RPC in one call)
    match mcp::process_rpc(&server_id, &body) {
        Ok(result) => {
            // Serialize typed JsonrpcResponse into a string for the HTTP response
            match serde_json::to_string(&result) {
                Ok(body_str) => send_success_response(response_out, body_str),
                Err(e) => {
                    eprintln!("Failed to serialize JsonrpcResponse: {}", e);
                    send_error_response(response_out, 500, "Internal server error".to_string());
                }
            }
        }
        Err(e) => {
            // Inspect the error message field on the typed error response
            if e.error.message.contains("Invalid JSON-RPC") {
                send_error_response(response_out, 500, "Invalid JSON-RPC request".to_string());
            } else {
                send_error_response(response_out, 400, e.error.message.clone());
            }
        }
    }
}

fn validate_content_type(
    request: &crate::wasi::http::types::IncomingRequest,
) -> Result<(), String> {
    let headers = request.headers();
    let entries = headers.entries();

    for (key, value) in entries {
        if key.to_lowercase() == "content-type" {
            let value_str = String::from_utf8_lossy(&value);
            if value_str.contains("application/json") {
                return Ok(());
            }
        }
    }

    Err("Content-Type must be application/json".to_string())
}

fn extract_server_id_from_path(path: &str) -> Result<String, String> {
    // Expected format: /mcp/{server-id}
    let parts: Vec<&str> = path.split('/').collect();

    if parts.len() >= 3 && parts[1] == "mcp" {
        // Extract server-id (parts[2]), removing any query parameters
        let server_id = parts[2].split('?').next().unwrap_or(parts[2]);
        if server_id.is_empty() {
            Err("Server ID cannot be empty. Expected /mcp/{server-id}".to_string())
        } else {
            Ok(server_id.to_string())
        }
    } else {
        Err("Invalid path format. Expected /mcp/{server-id}".to_string())
    }
}

fn read_request_body(
    request: &crate::wasi::http::types::IncomingRequest,
) -> Result<String, String> {
    let body_stream = request.consume().map_err(|_| "Failed to get body stream")?;
    let input_stream = body_stream
        .stream()
        .map_err(|_| "Failed to get input stream")?;

    let mut buf = Vec::new();
    while let Ok(chunk) = input_stream.blocking_read(1024 * 1024) {
        if chunk.is_empty() {
            break;
        }
        buf.extend_from_slice(&chunk);
    }

    String::from_utf8(buf).map_err(|e| format!("Invalid UTF-8 in body: {}", e))
}

fn send_success_response(response_out: crate::wasi::http::types::ResponseOutparam, body: String) {
    use crate::wasi::http::types::{Fields, OutgoingBody, OutgoingResponse};

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);

    let response = OutgoingResponse::new(headers);
    if let Err(e) = response.set_status_code(200) {
        eprintln!("Failed to set status code: {:?}", e);
        return;
    }

    let response_body = match response.body() {
        Ok(rb) => rb,
        Err(e) => {
            eprintln!("Failed to get response body: {:?}", e);
            return;
        }
    };
    crate::wasi::http::types::ResponseOutparam::set(response_out, Ok(response));

    let output_stream = match response_body.write() {
        Ok(os) => os,
        Err(e) => {
            eprintln!("Failed to get output stream: {:?}", e);
            return;
        }
    };
    if let Err(e) = output_stream.blocking_write_and_flush(body.as_bytes()) {
        eprintln!("Failed to write response: {:?}", e);
        return;
    }

    drop(output_stream);
    if let Err(e) = OutgoingBody::finish(response_body, None) {
        eprintln!("Failed to finish body: {:?}", e);
    }
}

fn send_error_response(
    response_out: crate::wasi::http::types::ResponseOutparam,
    status: u16,
    message: String,
) {
    use crate::wasi::http::types::{Fields, OutgoingBody, OutgoingResponse};

    let error_body = json!({
        "jsonrpc": "2.0",
        "error": {
            "code": -32000,
            "message": message
        },
        "id": null
    });

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);

    let response = OutgoingResponse::new(headers);
    if let Err(e) = response.set_status_code(status) {
        eprintln!("Failed to set status code: {:?}", e);
        return;
    }

    let response_body = match response.body() {
        Ok(rb) => rb,
        Err(e) => {
            eprintln!("Failed to get response body: {:?}", e);
            return;
        }
    };
    crate::wasi::http::types::ResponseOutparam::set(response_out, Ok(response));

    let output_stream = match response_body.write() {
        Ok(os) => os,
        Err(e) => {
            eprintln!("Failed to get output stream: {:?}", e);
            return;
        }
    };
    let body_str = serde_json::to_string(&error_body).unwrap();
    if let Err(e) = output_stream.blocking_write_and_flush(body_str.as_bytes()) {
        eprintln!("Failed to write response: {:?}", e);
        return;
    }

    drop(output_stream);
    if let Err(e) = OutgoingBody::finish(response_body, None) {
        eprintln!("Failed to finish body: {:?}", e);
    }
}

export!(Component);
