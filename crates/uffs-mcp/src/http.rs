// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Streamable HTTP gateway for the UFFS MCP server.
//!
//! Wraps the same [`UffsMcpServer`] handler used
//! by the stdio transport and exposes it via HTTP using [`rmcp`]'s
//! `StreamableHttpService`.
//!
//! # Endpoints
//!
//! | Method | Path       | Description                    |
//! |--------|------------|--------------------------------|
//! | `*`    | `/mcp`     | MCP Streamable HTTP (JSON-RPC) |
//! | `GET`  | `/health`  | Liveness probe (always `200`)  |
//! | `GET`  | `/status`  | Server status + uptime JSON    |
//!
//! # Authentication
//!
//! When a bearer token is configured (`--auth-token`), all `/mcp` requests
//! must include `Authorization: Bearer <token>`.  The `/health` endpoint
//! is always unauthenticated.

use alloc::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::StreamableHttpService;

use crate::handler::UffsMcpServer;
use crate::stats::McpStats;

/// Shared state for the axum application.
#[derive(Clone)]
pub(crate) struct AppState {
    /// Bearer token required for `/mcp` — `None` means no auth.
    auth_token: Option<Arc<str>>,
    /// Server boot time (for uptime reporting).
    boot: Instant,
    /// Shared MCP stats across all sessions.
    stats: Arc<McpStats>,
}

/// Configuration for the HTTP gateway.
///
/// # Field discipline (Phase 3b §3.4)
///
/// All three fields are `pub` because this is a **configuration DTO**.
/// `bind_addr` is a required parameter (no default that makes sense);
/// `auth_token` is genuinely optional; `daemon_spawn_args` is a
/// forwarded `Vec<OsString>` with no invariants to protect.
///
/// # `#[non_exhaustive]` decision (Phase 3b §3.6)
///
/// **Kept exhaustive.**  Same rationale as [`crate::McpConfig`] —
/// `uffs-mcp` is a bin-dominant internal app and the construction
/// sites all live in this crate's bins (`src/main.rs`,
/// `src/bin/http_gateway.rs`) plus the test module below.  Revisit
/// when / if `uffs-mcp` exposes an embedding API.
pub struct HttpGatewayConfig {
    /// TCP bind address (e.g. `127.0.0.1:8080`).
    pub bind_addr: core::net::SocketAddr,
    /// Optional bearer token for authenticating MCP requests.
    pub auth_token: Option<String>,
    /// Extra CLI args forwarded to `uffs --daemon run` on auto-start.
    pub daemon_spawn_args: Vec<std::ffi::OsString>,
}

/// Build the axum [`Router`] with MCP, health, and status endpoints.
///
/// The router is returned without binding — call [`run_gateway`] to serve.
pub(crate) fn build_router(config: &HttpGatewayConfig) -> Router {
    let spawn_args: Arc<[std::ffi::OsString]> = config.daemon_spawn_args.clone().into();
    let stats = Arc::new(McpStats::default());
    let stats_for_factory = Arc::clone(&stats);

    let mcp_service: StreamableHttpService<UffsMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(UffsMcpServer::new_lazy_with_stats(
                    spawn_args.to_vec(),
                    Arc::clone(&stats_for_factory),
                ))
            },
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );
    let app_state = AppState {
        auth_token: config.auth_token.as_deref().map(Into::into),
        boot: Instant::now(),
        stats,
    };

    // Protected MCP routes — behind auth middleware when a token is set.
    let mcp_router = Router::new().nest_service("/mcp", mcp_service).layer(
        axum::middleware::from_fn_with_state(app_state.clone(), auth_middleware),
    );

    Router::new()
        .merge(mcp_router)
        .route("/health", get(health))
        .route("/status", get(status))
        .with_state(app_state)
}

/// Start the HTTP gateway and serve until shutdown.
///
/// # Errors
///
/// Returns an error if binding fails.
pub async fn run_gateway(config: HttpGatewayConfig) -> anyhow::Result<()> {
    let addr = config.bind_addr;
    let router = build_router(&config);

    tracing::info!(%addr, "UFFS MCP HTTP gateway listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("HTTP gateway shut down");
    Ok(())
}

// ── Handlers ────────────────────────────────────────────────────────────────

/// `GET /health` — always returns `200 OK`.
async fn health() -> &'static str {
    "ok"
}

/// `GET /status` — returns server uptime and config summary.
async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let uptime_secs = state.boot.elapsed().as_secs();
    let mut json = serde_json::json!({
        "status": "running",
        "uptime_secs": uptime_secs,
        "auth_enabled": state.auth_token.is_some(),
        "version": env!("CARGO_PKG_VERSION"),
    });
    // Merge MCP stats into the response.
    if let Some(obj) = json.as_object_mut() {
        obj.insert("mcp_stats".to_owned(), state.stats.to_json());
    }
    Json(json)
}

// ── Auth middleware ─────────────────────────────────────────────────────────

/// Bearer token authentication middleware.
///
/// When `AppState::auth_token` is `Some`, validates the `Authorization`
/// header.  Returns `401 Unauthorized` on mismatch.  When no token is
/// configured, all requests pass through.
async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let Some(expected) = &state.auth_token else {
        // No auth configured — pass through.
        return Ok(next.run(request).await);
    };

    let header_val = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|hv| hv.to_str().ok());

    match header_val {
        Some(val)
            if val
                .strip_prefix("Bearer ")
                .is_some_and(|tok| tok == expected.as_ref()) =>
        {
            Ok(next.run(request).await)
        }
        _ => {
            tracing::warn!("Unauthorized MCP request (invalid or missing bearer token)");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

// ── Graceful shutdown ───────────────────────────────────────────────────────

/// Wait for `SIGINT` or `SIGTERM` (Unix) / `Ctrl-C` (all platforms).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _res = ctrl_c => {}
            () = async {
                if let Some(ref mut sig) = sigterm { sig.recv().await; }
                else { core::future::pending::<()>().await; }
            } => {}
        }
    }

    #[cfg(not(unix))]
    {
        // ctrl_c() returns Result<(), io::Error>; we don't care which
        // failure mode the OS reports — receiving any signal is enough
        // to start shutting the HTTP server down.
        let _ctrl_c_result: std::io::Result<()> = ctrl_c.await;
    }

    tracing::info!("Shutdown signal received");
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower_service::Service as _;

    use super::*;

    /// Helper: build a router with a known auth token for testing.
    fn test_router(token: Option<&str>) -> Router {
        let config = HttpGatewayConfig {
            bind_addr: ([127, 0, 0, 1], 0).into(),
            auth_token: token.map(ToOwned::to_owned),
            daemon_spawn_args: vec![],
        };
        build_router(&config)
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let mut app = test_router(None);
        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn status_returns_json() {
        let mut app = test_router(None);
        let req = Request::get("/status").body(Body::empty()).unwrap();
        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json.get("status").and_then(|val| val.as_str()),
            Some("running")
        );
        assert!(
            json.get("uptime_secs")
                .is_some_and(serde_json::Value::is_number)
        );
    }

    #[tokio::test]
    async fn auth_rejects_missing_token() {
        let mut app = test_router(Some("secret-token-42"));

        // POST /mcp without Authorization header → 401
        let req = Request::post("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            ))
            .unwrap();
        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_token() {
        let mut app = test_router(Some("secret-token-42"));

        let req = Request::post("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Bearer wrong-token")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            ))
            .unwrap();
        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_passes_correct_token() {
        let mut app = test_router(Some("secret-token-42"));

        let req = Request::post("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Bearer secret-token-42")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#,
            ))
            .unwrap();
        let resp = app.call(req).await.unwrap();
        // Should NOT be 401 — the auth layer passed.
        // May be 200 (SSE) or other MCP response, but never 401 or 406.
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_ne!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn no_auth_passes_everything() {
        let mut app = test_router(None);

        let req = Request::post("/mcp")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#,
            ))
            .unwrap();
        let resp = app.call(req).await.unwrap();
        // No auth required — should reach the MCP layer.
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_ne!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    }

    #[tokio::test]
    async fn health_bypasses_auth() {
        let mut app = test_router(Some("secret-token-42"));

        // /health should return 200 even without auth
        let req = Request::get("/health").body(Body::empty()).unwrap();
        let resp = app.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
