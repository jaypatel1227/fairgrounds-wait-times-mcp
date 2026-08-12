mod auth;
mod data;
mod server;

use std::{env, net::SocketAddr, sync::Arc};

use anyhow::{bail, Context, Result};
use axum::{middleware, routing::get, Router};
use rmcp::{
    transport::stdio,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServiceExt,
};
use tracing_subscriber::{prelude::*, EnvFilter};

use auth::{ensure_mcp_accept, require_bearer, BearerToken};
use server::FairgroundsServer;

#[tokio::main]
async fn main() -> Result<()> {
    let mode = TransportMode::from_args_env();

    // Always log to stderr so stdio MCP keeps stdout clean.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    match mode {
        TransportMode::Stdio => run_stdio().await,
        TransportMode::Http => run_http().await,
    }
}

#[derive(Debug, Clone, Copy)]
enum TransportMode {
    /// Local MCP clients (Cursor, Claude Desktop) over stdin/stdout.
    Stdio,
    /// Stateless Streamable HTTP — Railway / serverless friendly (MCP 2026-07-28).
    Http,
}

impl TransportMode {
    fn from_args_env() -> Self {
        if let Some(arg) = env::args().nth(1) {
            match arg.as_str() {
                "--stdio" | "stdio" => return Self::Stdio,
                "--http" | "http" => return Self::Http,
                "--help" | "-h" => {
                    eprintln!(
                        "Fairgrounds Wait Times MCP (UGM 2027 demo)\n\n\
                         Usage:\n  \
                         fairgrounds-wait-times-mcp [--http]   # default: stateless Streamable HTTP\n  \
                         fairgrounds-wait-times-mcp --stdio    # stdio transport for local clients\n\n\
                         Env:\n  \
                         PORT / HOST          HTTP bind (default 0.0.0.0:8080)\n  \
                         MCP_TRANSPORT       http | stdio (overridden by flags)\n  \
                         MCP_BEARER_TOKEN    required for HTTP; Authorization: Bearer <token>\n  \
                         MCP_ALLOWED_HOSTS   comma-separated Host allowlist, or * to disable\n"
                    );
                    std::process::exit(0);
                }
                other => {
                    eprintln!("unknown argument: {other} (try --help)");
                    std::process::exit(2);
                }
            }
        }

        match env::var("MCP_TRANSPORT")
            .unwrap_or_else(|_| "http".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "stdio" => Self::Stdio,
            _ => Self::Http,
        }
    }
}

async fn run_stdio() -> Result<()> {
    tracing::info!("Starting Fairgrounds Wait Times MCP (stdio) — UGM 2027 demo");

    let service = FairgroundsServer
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;

    service.waiting().await?;
    Ok(())
}

fn load_bearer_token() -> Result<BearerToken> {
    let raw = env::var("MCP_BEARER_TOKEN").unwrap_or_default();
    let token = raw.trim();
    if token.is_empty() {
        bail!(
            "MCP_BEARER_TOKEN is required for HTTP mode. \
             Set a long random secret and send it as `Authorization: Bearer <token>` \
             from Epic (or any client). /health stays public for Railway checks."
        );
    }
    if token.len() < 16 {
        bail!("MCP_BEARER_TOKEN must be at least 16 characters");
    }
    Ok(BearerToken(Arc::from(token)))
}

/// DNS-rebinding protection in `rmcp` defaults to loopback-only Host headers.
/// Public Railway / IPv6 deployments must allow the public hostname (or `*`).
///
/// Resolution order:
/// 1. `MCP_ALLOWED_HOSTS=*` → disable Host checks (bearer auth still required)
/// 2. `MCP_ALLOWED_HOSTS=host1,host2` → explicit allowlist
/// 3. else start from loopback defaults, plus `RAILWAY_PUBLIC_DOMAIN` when set
fn resolve_allowed_hosts() -> Option<Vec<String>> {
    if let Ok(raw) = env::var("MCP_ALLOWED_HOSTS") {
        let trimmed = raw.trim();
        if trimmed == "*" {
            return None; // disable allowlist
        }
        let hosts: Vec<String> = trimmed
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect();
        if !hosts.is_empty() {
            return Some(hosts);
        }
    }

    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];

    if let Ok(domain) = env::var("RAILWAY_PUBLIC_DOMAIN") {
        let domain = domain.trim();
        if !domain.is_empty() {
            hosts.push(domain.to_string());
        }
    }

    Some(hosts)
}

async fn run_http() -> Result<()> {
    let bearer = load_bearer_token()?;

    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid bind address {host}:{port}"))?;

    // Stateless Streamable HTTP (SEP-2567 / MCP 2026-07-28):
    // - no Mcp-Session-Id affinity → safe for Railway serverless / multi-instance
    // - json_response for simple tool request/response without SSE
    // - fresh FairgroundsServer per request (no in-memory session state)
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);

    match resolve_allowed_hosts() {
        None => {
            tracing::warn!(
                "MCP_ALLOWED_HOSTS=* — Host header checks disabled \
                 (DNS-rebinding protection off; bearer auth still enforced)"
            );
            config = config.disable_allowed_hosts();
        }
        Some(hosts) => {
            tracing::info!(?hosts, "Streamable HTTP allowed Host headers");
            config = config.with_allowed_hosts(hosts);
        }
    }

    let mcp: StreamableHttpService<FairgroundsServer, LocalSessionManager> =
        StreamableHttpService::new(
            || Ok(FairgroundsServer),
            Arc::new(LocalSessionManager::default()),
            config,
        );

    // Bearer auth + Accept shim wrap /mcp only. /health stays open for Railway.
    // Outer layers run first: auth → Accept normalize → rmcp.
    let protected_mcp = Router::new()
        .fallback_service(mcp)
        .layer(middleware::from_fn(ensure_mcp_accept))
        .layer(middleware::from_fn_with_state(bearer, require_bearer));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/",
            get(|| async {
                "Fairgrounds Wait Times MCP — UGM 2027 demo. POST /mcp with Authorization: Bearer <token>"
            }),
        )
        .nest("/mcp", protected_mcp);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;

    tracing::info!(
        %addr,
        "Fairgrounds Wait Times MCP listening (stateless Streamable HTTP + bearer auth) — UGM 2027 demo"
    );
    tracing::info!("MCP endpoint: http://{addr}/mcp  health: http://{addr}/health");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server error")?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
