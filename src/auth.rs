//! HTTP bearer-token gate + Accept-header shim for the public `/mcp` endpoint.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

const JSON_MIME: &str = "application/json";
const EVENT_STREAM_MIME: &str = "text/event-stream";
const MCP_ACCEPT: &str = "application/json, text/event-stream";

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

/// `rmcp` Streamable HTTP rejects POSTs unless `Accept` contains both
/// `application/json` and `text/event-stream`. Some clients (and intermediate
/// proxies) only send JSON — fill in the missing type so the demo still works.
pub async fn ensure_mcp_accept(mut request: Request, next: Next) -> Response {
    normalize_accept(request.headers_mut());
    next.run(request).await
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
}
