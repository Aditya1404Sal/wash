mod bindings {
    wit_bindgen::generate!({
        generate_all,
    });
}

use bindings::{
    exports::wasi::http::incoming_handler::Guest,
    wasi::{
        config::store::get_all,
        http::types::{
            Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
        },
    },
};

struct Component;

impl Guest for Component {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let config_entries = match get_all() {
            Ok(entries) => entries,
            Err(_) => vec![],
        };

        let map: std::collections::BTreeMap<String, String> =
            config_entries.into_iter().collect();

        let json_body = serde_json::json!({ "config": map }).to_string();

        let headers = Fields::new();
        headers
            .set(&"content-type".to_string(), &[b"application/json".to_vec()])
            .ok();

        let response = OutgoingResponse::new(headers);
        response.set_status_code(200).unwrap();
        let body = response.body().unwrap();
        ResponseOutparam::set(response_out, Ok(response));

        let stream = body.write().unwrap();
        stream
            .blocking_write_and_flush(json_body.as_bytes())
            .unwrap();
        drop(stream);
        OutgoingBody::finish(body, None).unwrap();
    }
}

bindings::export!(Component with_types_in bindings);
