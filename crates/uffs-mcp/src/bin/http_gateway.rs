// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs-mcp-http` — Streamable HTTP gateway for the UFFS MCP server.
//!
//! Usage:
//! ```text
//! uffs-mcp-http [--bind 127.0.0.1:8080] [--auth-token SECRET]
//! ```
//!
//! This binary starts an HTTP server that exposes the same MCP server
//! implementation as `uffs-mcp` (stdio) but over HTTP Streamable transport.
//! Multiple concurrent MCP sessions are supported, each with its own
//! daemon connection.

// Silence unused-crate-dependency warnings for workspace deps.
use core::net::SocketAddr;

use anyhow as _;
use axum as _;
use clap as _;
use rmcp as _;
use schemars as _;
use serde as _;
use serde_json as _;
use thiserror as _;
use tower_service as _;
use tracing_appender as _;
use uffs_client as _;
use uffs_mft as _;
use uffs_security as _;

/// CLI arguments for the HTTP gateway.
#[derive(Clone, Debug)]
struct Args {
    /// TCP address to bind (default: `127.0.0.1:8080`).
    bind: SocketAddr,
    /// Optional bearer token for authenticating MCP requests.
    auth_token: Option<String>,
    /// Extra args forwarded to `uffs --daemon run` on auto-start.
    #[expect(
        clippy::struct_field_names,
        reason = "clarity: distinguishes from other arg types"
    )]
    daemon_args: Vec<String>,
}

impl Args {
    /// Parse CLI args (minimal parser — no clap dependency for the gateway).
    #[expect(
        clippy::print_stderr,
        reason = "CLI binary — stderr is the correct output for help/errors"
    )]
    fn parse() -> Self {
        let mut bind: SocketAddr = ([127, 0, 0, 1], 8080).into();
        let mut auth_token: Option<String> = None;
        let mut extra_daemon_args = Vec::new();

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind" | "-b" => {
                    if let Some(val) = args.next() {
                        bind = val.parse().unwrap_or_else(|err| {
                            eprintln!("error: invalid --bind address '{val}': {err}");
                            std::process::exit(1);
                        });
                    }
                }
                "--auth-token" | "-t" => {
                    auth_token = args.next();
                }
                "--data-dir" => {
                    // Forward --data-dir to daemon spawn args.
                    extra_daemon_args.push("--data-dir".to_owned());
                    if let Some(val) = args.next() {
                        extra_daemon_args.push(val);
                    }
                }
                "--help" | "-h" => {
                    eprintln!("Usage: uffs-mcp-http [OPTIONS]");
                    eprintln!();
                    eprintln!("Options:");
                    eprintln!("  -b, --bind <ADDR>         Bind address [default: 127.0.0.1:8080]");
                    eprintln!("  -t, --auth-token <TOKEN>  Bearer token for /mcp auth");
                    eprintln!("      --data-dir <DIR>      Data directory (forwarded to daemon)");
                    eprintln!("  -h, --help                Show this help");
                    std::process::exit(0);
                }
                other => {
                    eprintln!("warning: unknown argument '{other}' (ignored)");
                }
            }
        }

        // Also check env vars as fallback.
        if auth_token.is_none() {
            auth_token = std::env::var("UFFS_MCP_AUTH_TOKEN").ok();
        }

        Self {
            bind,
            auth_token,
            daemon_args: extra_daemon_args,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    uffs_version::handle_version!("uffs-mcp-http");
    // Initialise tracing to stderr (stdout is NOT used by HTTP mode,
    // but keeping the convention consistent with the stdio binary).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    tracing::info!(
        bind = %args.bind,
        auth = args.auth_token.is_some(),
        "Starting UFFS MCP HTTP gateway"
    );

    let config = uffs_mcp::http::HttpGatewayConfig {
        bind_addr: args.bind,
        auth_token: args.auth_token,
        daemon_spawn_args: args
            .daemon_args
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect(),
    };

    uffs_mcp::http::run_gateway(config).await
}
