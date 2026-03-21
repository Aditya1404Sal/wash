//! Server-Sent Events (SSE) connection management.
//!
//! The host detects incoming SSE requests (`Accept: text/event-stream`), holds
//! the connection open, and registers the stream in an in-memory registry.
//! Components can then push events to open SSE connections via the
//! `bettyblocks:sse/handler` WIT import backed by the [`SseRegistry`].

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, StreamBody};
use tokio::sync::{RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};
use wasmtime_wasi_http::body::HyperOutgoingBody;

/// Metadata for a single SSE connection.
#[derive(Debug, Clone)]
pub struct SseConnectionMeta {
    pub stream_id: String,
    pub url: String,
    pub scope_params: HashMap<String, String>,
    pub connected_at: String,
}

/// Handle to an open SSE connection held by the host.
pub struct SseStreamHandle {
    pub sender: mpsc::Sender<Result<http_body::Frame<Bytes>, Infallible>>,
    pub meta: SseConnectionMeta,
}

/// Registry of active SSE connections.
///
/// Shared between the HTTP handler (registers connections) and the
/// `SseWriter` plugin (pushes events from components).
#[derive(Clone)]
pub struct SseRegistry {
    streams: Arc<RwLock<HashMap<String, SseStreamHandle>>>,
    route_pattern: Arc<str>,
}

impl SseRegistry {
    pub fn new(route_pattern: impl Into<Arc<str>>) -> Self {
        Self {
            streams: Arc::new(RwLock::new(HashMap::new())),
            route_pattern: route_pattern.into(),
        }
    }

    /// Check if an incoming request is an SSE request.
    pub fn is_sse_request(req: &hyper::Request<hyper::body::Incoming>) -> bool {
        req.headers()
            .get(hyper::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("text/event-stream"))
            .unwrap_or(false)
    }

    /// Parse scope parameters from a URL path against the configured route pattern.
    pub fn parse_scope(&self, path: &str) -> Option<HashMap<String, String>> {
        let pattern_segments: Vec<&str> =
            self.route_pattern.split('/').filter(|s| !s.is_empty()).collect();
        let path_segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        if pattern_segments.len() != path_segments.len() {
            return None;
        }

        let mut params = HashMap::new();
        for (pattern, value) in pattern_segments.iter().zip(path_segments.iter()) {
            if pattern.starts_with('{') && pattern.ends_with('}') {
                let key = &pattern[1..pattern.len() - 1];
                params.insert(key.to_string(), value.to_string());
            } else if pattern != value {
                return None;
            }
        }

        Some(params)
    }

    /// Build a deterministic scope key from extracted parameters.
    pub fn scope_key(params: &HashMap<String, String>) -> String {
        let mut values: Vec<&str> = params.values().map(|v| v.as_str()).collect();
        values.sort();
        format!("sse:scope:{}", values.join(":"))
    }

    /// Handle an SSE request: register the connection and return a streaming response.
    pub async fn handle_sse_request(
        &self,
        req: &hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<HyperOutgoingBody>, hyper::Error> {
        let path = req.uri().path().to_string();
        let scope_params = self.parse_scope(&path).unwrap_or_default();
        let stream_id = format!("sse-conn-{}", uuid::Uuid::new_v4());

        let meta = SseConnectionMeta {
            stream_id: stream_id.clone(),
            url: path.clone(),
            scope_params: scope_params.clone(),
            connected_at: Utc::now().to_rfc3339(),
        };

        let (tx, rx) = mpsc::channel::<Result<http_body::Frame<Bytes>, Infallible>>(32);

        self.streams.write().await.insert(
            stream_id.clone(),
            SseStreamHandle {
                sender: tx,
                meta,
            },
        );

        info!(
            stream_id = %stream_id,
            url = %path,
            scope = ?scope_params,
            "SSE connection registered"
        );

        let body_stream = ReceiverStream::new(rx);
        let stream_body = StreamBody::new(body_stream)
            .map_err(|_: Infallible| unreachable!())
            .boxed();

        let response = hyper::Response::builder()
            .status(200)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            .header("X-SSE-Stream-Id", &stream_id)
            .body(stream_body)
            .expect("failed to build SSE response");

        Ok(response)
    }

    /// Write an SSE event to a specific stream. Called by the SseWriter plugin.
    pub async fn write_event(&self, stream_id: &str, data: &str) -> Result<(), String> {
        let streams = self.streams.read().await;
        let handle = streams
            .get(stream_id)
            .ok_or_else(|| format!("stream '{}' not found", stream_id))?;

        let sse_data = format!("data: {}\n\n", data);
        let frame = http_body::Frame::data(Bytes::from(sse_data));

        handle
            .sender
            .send(Ok(frame))
            .await
            .map_err(|_| format!("stream '{}' channel closed", stream_id))
    }

    /// Check if a stream ID is registered.
    pub async fn is_registered(&self, stream_id: &str) -> bool {
        self.streams.read().await.contains_key(stream_id)
    }

    /// List all stream IDs matching a scope key.
    pub async fn list_connections(&self, scope_key: &str) -> Vec<String> {
        self.streams
            .read()
            .await
            .values()
            .filter(|h| Self::scope_key(&h.meta.scope_params) == scope_key)
            .map(|h| h.meta.stream_id.clone())
            .collect()
    }

    /// Run the keepalive loop. Sends `:keepalive\n\n` every 30 seconds.
    /// Removes dead connections when the send fails.
    pub async fn run_keepalive(self: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;

            let mut dead_ids = Vec::new();

            {
                let streams = self.streams.read().await;
                for (id, handle) in streams.iter() {
                    let frame =
                        http_body::Frame::data(Bytes::from_static(b":keepalive\n\n"));
                    if handle.sender.send(Ok(frame)).await.is_err() {
                        warn!(stream_id = %id, "SSE keepalive failed, connection dead");
                        dead_ids.push(id.clone());
                    }
                }
            }

            if !dead_ids.is_empty() {
                let mut streams = self.streams.write().await;
                for id in &dead_ids {
                    streams.remove(id);
                }
                info!(count = dead_ids.len(), "Cleaned up dead SSE connections");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scope_match() {
        let registry = SseRegistry::new("/api/record-updates/{model}/{recordId}");
        let params = registry
            .parse_scope("/api/record-updates/chatMessage/323")
            .unwrap();
        assert_eq!(params.get("model").unwrap(), "chatMessage");
        assert_eq!(params.get("recordId").unwrap(), "323");
    }

    #[test]
    fn test_parse_scope_no_match() {
        let registry = SseRegistry::new("/api/record-updates/{model}/{recordId}");
        assert!(registry.parse_scope("/api/other/chatMessage/323").is_none());
        assert!(registry.parse_scope("/api/record-updates/chatMessage").is_none());
    }

    #[test]
    fn test_scope_key_deterministic() {
        let mut params = HashMap::new();
        params.insert("model".to_string(), "chatMessage".to_string());
        params.insert("recordId".to_string(), "323".to_string());

        let mut params2 = HashMap::new();
        params2.insert("recordId".to_string(), "323".to_string());
        params2.insert("model".to_string(), "chatMessage".to_string());

        assert_eq!(
            SseRegistry::scope_key(&params),
            SseRegistry::scope_key(&params2)
        );
    }

    #[tokio::test]
    async fn test_write_event_and_receive() {
        let registry = SseRegistry::new("/test/{id}");

        let (tx, mut rx) = mpsc::channel(32);
        registry.streams.write().await.insert(
            "test-stream".to_string(),
            SseStreamHandle {
                sender: tx,
                meta: SseConnectionMeta {
                    stream_id: "test-stream".to_string(),
                    url: "/test/42".to_string(),
                    scope_params: HashMap::from([("id".to_string(), "42".to_string())]),
                    connected_at: Utc::now().to_rfc3339(),
                },
            },
        );

        registry
            .write_event("test-stream", r#"{"msg":"hello"}"#)
            .await
            .unwrap();

        let frame = rx.recv().await.unwrap().unwrap();
        let data = frame.into_data().unwrap();
        assert_eq!(data, Bytes::from("data: {\"msg\":\"hello\"}\n\n"));
    }

    #[tokio::test]
    async fn test_write_event_not_found() {
        let registry = SseRegistry::new("/test/{id}");
        assert!(registry.write_event("nonexistent", "data").await.is_err());
    }
}
