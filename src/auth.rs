//! HTTP bearer-token gate + request shims for the public `/mcp` endpoint.

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

const JSON_MIME: &str = "application/json";
const EVENT_STREAM_MIME: &str = "text/event-stream";
const MCP_ACCEPT: &str = "application/json, text/event-stream";
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Used when a client POSTs an empty body (connectivity probes, misconfigured tools).
const DEFAULT_INITIALIZE: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"anonymous","version":"0.0.0"}}}"#;

/// Expected bearer secret from `MCP_BEARER_TOKEN`.
#[derive(Clone)]
pub struct BearerToken(pub Arc<str>);

pub async fn require_bearer(
    State(expected): State<BearerToken>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_bearer);

    match presented {
        Some(token) if constant_time_eq(token.as_bytes(), expected.0.as_bytes()) => {
            next.run(request).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized: send Authorization: Bearer <MCP_BEARER_TOKEN>",
        )
            .into_response(),
    }
}

/// Soften Streamable HTTP request requirements for imperfect clients:
/// - Ensure `Accept` lists both `application/json` and `text/event-stream`
/// - Ensure `Content-Type: application/json` on POST
/// - Replace an empty POST body with a default `initialize` request
pub async fn ensure_mcp_request(request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        let mut request = request;
        normalize_accept(request.headers_mut());
        return next.run(request).await;
    }

    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("request body exceeds {MAX_BODY_BYTES} bytes"),
            )
                .into_response();
        }
    };

    let bytes = if body_is_empty(&bytes) {
        tracing::warn!(
            "empty POST /mcp body — substituting default initialize JSON-RPC request"
        );
        Bytes::from_static(DEFAULT_INITIALIZE)
    } else {
        bytes
    };

    normalize_accept(&mut parts.headers);
    ensure_json_content_type(&mut parts.headers);

    let request = Request::from_parts(parts, Body::from(bytes));
    next.run(request).await
}

fn body_is_empty(bytes: &Bytes) -> bool {
    bytes.is_empty() || bytes.iter().all(|b| b.is_ascii_whitespace())
}

fn ensure_json_content_type(headers: &mut axum::http::HeaderMap) {
    let ok = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with(JSON_MIME));
    if !ok {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
}

fn normalize_accept(headers: &mut axum::http::HeaderMap) {
    let current = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let has_json = current.contains(JSON_MIME);
    let has_sse = current.contains(EVENT_STREAM_MIME);
    if has_json && has_sse {
        return;
    }

    let value = if current.trim().is_empty() || current.trim() == "*/*" {
        MCP_ACCEPT.to_string()
    } else {
        let mut parts = vec![current.trim().to_string()];
        if !has_json {
            parts.push(JSON_MIME.to_string());
        }
        if !has_sse {
            parts.push(EVENT_STREAM_MIME.to_string());
        }
        parts.join(", ")
    };

    if let Ok(hv) = HeaderValue::from_str(&value) {
        headers.insert(header::ACCEPT, hv);
    }
}

fn parse_bearer(value: &str) -> Option<&str> {
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn accepts_bearer_prefix() {
        assert_eq!(parse_bearer("Bearer secret"), Some("secret"));
        assert_eq!(parse_bearer("bearer secret"), Some("secret"));
        assert_eq!(parse_bearer("Basic secret"), None);
        assert_eq!(parse_bearer("Bearer "), None);
    }

    #[test]
    fn compares_in_constant_time_shape() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn fills_missing_accept_types() {
        let mut headers = HeaderMap::new();
        normalize_accept(&mut headers);
        assert_eq!(headers.get(header::ACCEPT).unwrap(), MCP_ACCEPT);

        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        normalize_accept(&mut headers);
        let v = headers.get(header::ACCEPT).unwrap().to_str().unwrap();
        assert!(v.contains(JSON_MIME) && v.contains(EVENT_STREAM_MIME));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        normalize_accept(&mut headers);
        assert_eq!(
            headers.get(header::ACCEPT).unwrap(),
            "application/json, text/event-stream"
        );
    }

    #[test]
    fn empty_body_detection() {
        assert!(body_is_empty(&Bytes::new()));
        assert!(body_is_empty(&Bytes::from_static(b"  \n\t")));
        assert!(!body_is_empty(&Bytes::from_static(b"{}")));
    }

    #[test]
    fn default_initialize_is_valid_json() {
        let v: serde_json::Value = serde_json::from_slice(DEFAULT_INITIALIZE).unwrap();
        assert_eq!(v["method"], "initialize");
    }
}
