use serde::Deserialize;

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
    },
    bettyblocks::sse::handler as sse,
};

struct Component;

#[derive(Deserialize)]
struct PushRequest {
    stream_id: String,
    data: String,
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_default();

        match method {
            bindings::wasi::http::types::Method::Post => handle_push(request, response_out),
            bindings::wasi::http::types::Method::Get if path.starts_with("/connections/") => {
                handle_list_connections(&path, response_out)
            }
            _ => send_response(response_out, 405, b"Method not allowed"),
        }
    }
}

fn handle_push(request: IncomingRequest, response_out: ResponseOutparam) {
    let body = match read_request_body(request) {
        Ok(b) => b,
        Err(e) => {
            send_response(
                response_out,
                500,
                format!("Failed to read body: {e}").as_bytes(),
            );
            return;
        }
    };

    let req: PushRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            send_response(response_out, 400, format!("Invalid JSON: {e}").as_bytes());
            return;
        }
    };

    if !sse::is_registered(&req.stream_id) {
        send_response(
            response_out,
            404,
            format!("Stream '{}' not found", req.stream_id).as_bytes(),
        );
        return;
    }

    match sse::write_event(&req.stream_id, &req.data) {
        Ok(()) => {
            send_response(response_out, 200, b"Event sent");
        }
        Err(e) => {
            send_response(
                response_out,
                500,
                format!("Failed to write event: {e}").as_bytes(),
            );
        }
    }
}

fn handle_list_connections(path: &str, response_out: ResponseOutparam) {
    let scope_key = path.strip_prefix("/connections/").unwrap_or("");
    if scope_key.is_empty() {
        send_response(response_out, 400, b"Missing scope key");
        return;
    }

    let connections = sse::list_connections(scope_key);

    let json = serde_json::to_string(&connections).unwrap_or_else(|_| "[]".to_string());
    send_response(response_out, 200, json.as_bytes());
}

fn read_request_body(request: IncomingRequest) -> Result<String, String> {
    let body = request
        .consume()
        .map_err(|_| "Failed to consume request".to_string())?;

    let stream = body
        .stream()
        .map_err(|_| "Failed to get stream".to_string())?;

    let mut data = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => data.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(e) => return Err(format!("Stream error: {e:?}")),
        }
    }

    String::from_utf8(data).map_err(|e| format!("Invalid UTF-8: {e}"))
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
