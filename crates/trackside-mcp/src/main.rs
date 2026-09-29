//! Trackside MCP server: Streamable HTTP (MCP 2025-11-25) on axum.
//!
//! Data: `TRACKSIDE_SNAPSHOT` names the snapshot to serve, a local path or `s3://bucket/key`,
//! gzipped or plain JSON (build one with `trackside-snapshot`). Without it the server serves
//! `TRACKSIDE_FIXTURE` (default `fixtures/demo.json`).
//!
//! Runtime: on AWS Lambda (behind API Gateway) the server runs stateless with plain JSON
//! responses, since each request may land on a different instance. Locally it listens on
//! `TRACKSIDE_BIND` (default `127.0.0.1:8000`) with sessions; `TRACKSIDE_STATELESS=1` gives the
//! Lambda behaviour locally. The endpoint is `/mcp`; `/healthz` answers "ok".
//!
//! OAuth 2.1 (required by Alexa+) is the next milestone; until then the deployed endpoint is
//! open, read-only and rate limited at the API.

mod tools;

use std::io::Read;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use tools::{Stable, Trackside};
use trackside_core::{FixtureStore, Store};

async fn load_store() -> Result<FixtureStore> {
    let Ok(source) = std::env::var("TRACKSIDE_SNAPSHOT") else {
        let fixture =
            std::env::var("TRACKSIDE_FIXTURE").unwrap_or_else(|_| "fixtures/demo.json".into());
        tracing::info!(%fixture, "serving fixture");
        return FixtureStore::load(&fixture);
    };
    let bytes = match source.strip_prefix("s3://") {
        Some(rest) => {
            let (bucket, key) = rest
                .split_once('/')
                .context("TRACKSIDE_SNAPSHOT must look like s3://bucket/key")?;
            let config = aws_config::load_from_env().await;
            aws_sdk_s3::Client::new(&config)
                .get_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .with_context(|| format!("reading {source}"))?
                .body
                .collect()
                .await?
                .into_bytes()
                .to_vec()
        }
        None => std::fs::read(&source).with_context(|| format!("reading {source}"))?,
    };
    // Gzip starts with 1f 8b; S3 may or may not have decoded it already.
    let json = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice()).read_to_end(&mut out)?;
        out
    } else {
        bytes
    };
    let store = FixtureStore::from_json(&json)?;
    tracing::info!(%source, bytes = json.len(), "serving snapshot");
    Ok(store)
}

#[tokio::main]
async fn main() -> Result<()> {
    let on_lambda = std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok();
    // CloudWatch stamps each line itself, so Lambda logs skip the time and the colours.
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(on_lambda.then(|| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
        }))
        .with((!on_lambda).then(tracing_subscriber::fmt::layer))
        .init();

    let store: Arc<dyn Store> = Arc::new(load_store().await?);
    let stable: Stable = Default::default();
    let stateless = on_lambda || std::env::var("TRACKSIDE_STATELESS").is_ok_and(|v| v == "1");

    let ct = tokio_util::sync::CancellationToken::new();
    let mut config =
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token());
    if stateless {
        config = config
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None);
    }
    config = match std::env::var("TRACKSIDE_ALLOWED_HOSTS") {
        Ok(hosts) => config.with_allowed_hosts(hosts.split(',').map(|h| h.trim().to_string())),
        // Host checks guard a server on localhost against DNS rebinding. Behind API Gateway
        // the host is the API's own domain, so the check adds nothing there.
        Err(_) if on_lambda => config.disable_allowed_hosts(),
        Err(_) => config,
    };
    let service = StreamableHttpService::new(
        move || Ok(Trackside::new(store.clone(), stable.clone())),
        LocalSessionManager::default().into(),
        config,
    );

    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .nest_service("/mcp", service)
        .layer(CorsLayer::permissive());

    if on_lambda {
        return lambda_http::run(app)
            .await
            .map_err(|e| anyhow::anyhow!("lambda runtime: {e}"));
    }

    let bind = std::env::var("TRACKSIDE_BIND").unwrap_or_else(|_| "127.0.0.1:8000".into());
    tracing::info!(%bind, stateless, "trackside listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await?;
    Ok(())
}
