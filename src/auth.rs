//! HTTP bearer-token gate for the public `/mcp` endpoint.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

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
}
