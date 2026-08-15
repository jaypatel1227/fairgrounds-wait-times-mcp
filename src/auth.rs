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
const EMPTY_BODY_JSONRPC_ERROR: &str = r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"empty request body; send a JSON-RPC message"}}"#;

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
/// - Insert `Content-Type: application/json` on POST when that header is missing
/// - Reject empty POST bodies with HTTP 400 + a JSON-RPC error
pub async fn ensure_mcp_request(request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        let mut request = request;
        normalize_accept(request.headers_mut());
        return next.run(request).await;
    }

    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(err) => return body_read_error_response(&err),
    };

    if body_is_empty(&bytes) {
        return empty_body_response();
    }

    normalize_accept(&mut parts.headers);
    ensure_json_content_type(&mut parts.headers);

    let request = Request::from_parts(parts, Body::from(bytes));
    next.run(request).await
}

fn body_is_empty(bytes: &Bytes) -> bool {
    bytes.is_empty() || bytes.iter().all(|b| b.is_ascii_whitespace())
}

fn empty_body_response() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, JSON_MIME)],
        EMPTY_BODY_JSONRPC_ERROR,
    )
        .into_response()
}

fn body_read_error_response(err: &(dyn std::error::Error + 'static)) -> Response {
    if is_length_limit_error(err) {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("request body exceeds {MAX_BODY_BYTES} bytes"),
        )
            .into_response()
    } else {
        (StatusCode::BAD_REQUEST, "failed to read request body").into_response()
    }
}

/// `axum::body::to_bytes` wraps failures in `axum_core::Error`. Over-limit
/// bodies surface as `http_body_util::LengthLimitError` in the source chain;
/// connection/read failures do not.
fn is_length_limit_error(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if format!("{e:?}").contains("LengthLimitError") {
            return true;
        }
        current = e.source();
    }
    false
}

/// Media type of a Content-Type / Accept item: the token before any `;` parameters.
fn media_type(value: &str) -> &str {
    value.split(';').next().unwrap_or(value).trim()
}

fn accept_lists_media_type(accept: &str, expected: &str) -> bool {
    accept
        .split(',')
        .any(|part| media_type(part).eq_ignore_ascii_case(expected))
}

fn accept_is_wildcard_or_empty(accept: &str) -> bool {
    let trimmed = accept.trim();
    if trimmed.is_empty() {
        return true;
    }
    !trimmed.contains(',') && media_type(trimmed).eq_ignore_ascii_case("*/*")
}

fn ensure_json_content_type(headers: &mut axum::http::HeaderMap) {
    if headers.get(header::CONTENT_TYPE).is_none() {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(JSON_MIME));
    }
}

fn normalize_accept(headers: &mut axum::http::HeaderMap) {
    let current = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept_is_wildcard_or_empty(current) {
        headers.insert(header::ACCEPT, HeaderValue::from_static(MCP_ACCEPT));
        return;
    }

    let has_json = accept_lists_media_type(current, JSON_MIME);
    let has_sse = accept_lists_media_type(current, EVENT_STREAM_MIME);
    if has_json && has_sse {
        return;
    }

    let mut value = current.trim().to_string();
    if !has_json {
        value.push_str(", ");
        value.push_str(JSON_MIME);
    }
    if !has_sse {
        value.push_str(", ");
        value.push_str(EVENT_STREAM_MIME);
    }

    if let Ok(hv) = HeaderValue::from_str(&value) {
        headers.insert(header::ACCEPT, hv);
    }
}

fn parse_bearer(value: &str) -> Option<&str> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(|c: char| c.is_ascii_whitespace())?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = rest.trim();
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
        assert_eq!(parse_bearer("BEARER secret"), Some("secret"));
        assert_eq!(parse_bearer("BeArEr secret"), Some("secret"));
        assert_eq!(parse_bearer("Basic secret"), None);
        assert_eq!(parse_bearer("Bearer "), None);
        assert_eq!(parse_bearer("BEARER "), None);
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
        assert!(accept_lists_media_type(v, JSON_MIME) && accept_lists_media_type(v, EVENT_STREAM_MIME));

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
    fn accept_matches_exact_media_types_only() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json-patch+json"),
        );
        normalize_accept(&mut headers);
        let v = headers.get(header::ACCEPT).unwrap().to_str().unwrap();
        assert!(v.contains("application/json-patch+json"));
        assert!(accept_lists_media_type(v, JSON_MIME));
        assert!(accept_lists_media_type(v, EVENT_STREAM_MIME));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json;q=0.9"),
        );
        normalize_accept(&mut headers);
        let v = headers.get(header::ACCEPT).unwrap().to_str().unwrap();
        assert!(v.starts_with("application/json;q=0.9"));
        assert!(accept_lists_media_type(v, JSON_MIME));
        assert!(accept_lists_media_type(v, EVENT_STREAM_MIME));

        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("*/*;q=0.8"));
        normalize_accept(&mut headers);
        assert_eq!(headers.get(header::ACCEPT).unwrap(), MCP_ACCEPT);

        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("APPLICATION/JSON"));
        normalize_accept(&mut headers);
        let v = headers.get(header::ACCEPT).unwrap().to_str().unwrap();
        assert!(accept_lists_media_type(v, JSON_MIME));
        assert!(accept_lists_media_type(v, EVENT_STREAM_MIME));
    }

    #[test]
    fn inserts_json_content_type_only_when_missing() {
        let mut headers = HeaderMap::new();
        ensure_json_content_type(&mut headers);
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), JSON_MIME);

        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        ensure_json_content_type(&mut headers);
        assert_eq!(headers.get(header::CONTENT_TYPE).unwrap(), "text/plain");

        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        ensure_json_content_type(&mut headers);
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json; charset=utf-8"
        );
    }

    #[test]
    fn empty_body_detection() {
        assert!(body_is_empty(&Bytes::new()));
        assert!(body_is_empty(&Bytes::from_static(b"  \n\t")));
        assert!(!body_is_empty(&Bytes::from_static(b"{}")));
    }

    #[test]
    fn empty_body_response_is_400_jsonrpc() {
        let response = empty_body_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            JSON_MIME
        );

        let v: serde_json::Value = serde_json::from_str(EMPTY_BODY_JSONRPC_ERROR).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert!(v["id"].is_null());
        assert_eq!(v["error"]["code"], -32600);
        assert_eq!(
            v["error"]["message"],
            "empty request body; send a JSON-RPC message"
        );
    }

    #[test]
    fn io_errors_are_not_length_limit() {
        let err = std::io::Error::other("connection reset");
        assert!(!is_length_limit_error(&err));
        let response = body_read_error_response(&err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn to_bytes_over_limit_is_payload_too_large() {
        let body = Body::from(vec![0u8; 64]);
        let err = axum::body::to_bytes(body, 8).await.unwrap_err();
        assert!(is_length_limit_error(&err));
        let response = body_read_error_response(&err);
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
