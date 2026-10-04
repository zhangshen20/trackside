//! Trackside MCP server: Streamable HTTP (MCP 2025-11-25) on axum.
//!
//! Data: `TRACKSIDE_SNAPSHOT` names the snapshot to serve, a local path or `s3://bucket/key`,
//! gzipped or plain JSON (build one with `trackside-snapshot`). Without it the server serves
//! `TRACKSIDE_FIXTURE` (default `fixtures/demo.json`).
//!
//! Runtime: on AWS Lambda (behind API Gateway) the server runs stateless with plain JSON
//! responses, since each request may land on a different instance. Locally it listens on
//! `TRACKSIDE_BIND` (default `127.0.0.1:8000`) with sessions; `TRACKSIDE_STATELESS=1` gives the
//! Lambda behaviour locally. The endpoint is `/mcp`; `/healthz` answers with a small JSON
//! status: `{"ok":true,"snapshot_date":...,"meetings":N,"uptime_s":...}`.
//!
//! Auth: with `TRACKSIDE_AUTH_ISSUER` set, `/mcp` requires a Cognito access token and the
//! server publishes OAuth metadata under `/.well-known/` (see `auth.rs`). Without it the
//! endpoint is open, which is how it runs locally.
//!
//! Memory: with `TRACKSIDE_MEMORY_TABLE` set, each signed-in listener's followed horses, home
//! state and last stable check live in that DynamoDB table and survive across sessions;
//! otherwise in this process (see `memory.rs`).
//!
//! Bedrock: with `TRACKSIDE_BEDROCK_MODEL` set, `explain_race` has a Bedrock model reword its
//! facts for the ear, falling back to its template sentence (see `summary.rs`).
//!
//! Metrics: on Lambda, or with `TRACKSIDE_METRICS=1`, each tool call prints one CloudWatch
//! Embedded Metric Format line on stdout, and start-up prints one more (see `telemetry.rs`).

mod auth;
mod clock;
mod memory;
mod summary;
mod telemetry;
#[cfg(test)]
mod tests;
mod tools;

use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use auth::Auth;
use memory::Memory;
use summary::Summariser;
use telemetry::{Record, Sink, SnapshotInfo};
use tools::Trackside;
use trackside_core::{Fixture, FixtureStore, Store};

/// The store to serve, and what `/healthz` says about it.
async fn load_store() -> Result<(FixtureStore, SnapshotInfo)> {
    let Ok(source) = std::env::var("TRACKSIDE_SNAPSHOT") else {
        let fixture =
            std::env::var("TRACKSIDE_FIXTURE").unwrap_or_else(|_| "fixtures/demo.json".into());
        tracing::info!(%fixture, "serving fixture");
        let bytes =
            std::fs::read(&fixture).with_context(|| format!("reading fixture {fixture}"))?;
        return parse_store(&bytes);
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
    let loaded = parse_store(&json)?;
    tracing::info!(%source, bytes = json.len(), "serving snapshot");
    Ok(loaded)
}

fn parse_store(json: &[u8]) -> Result<(FixtureStore, SnapshotInfo)> {
    let fixture: Fixture = serde_json::from_slice(json).context("parsing fixture JSON")?;
    let info = SnapshotInfo::of(&fixture);
    Ok((FixtureStore::from_fixture(fixture), info))
}

#[tokio::main]
async fn main() -> Result<()> {
    let started = Instant::now();
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

    let metrics = telemetry::from_env(on_lambda);
    let loading = Instant::now();
    let (store, snapshot) = load_store().await?;
    if let Some(sink) = &metrics {
        Record::init(loading.elapsed()).emit(sink.as_ref());
    }
    let store: Arc<dyn Store> = Arc::new(store);
    let stateless = on_lambda || std::env::var("TRACKSIDE_STATELESS").is_ok_and(|v| v == "1");
    let auth = Auth::from_env()?;
    match &auth {
        Some(_) => tracing::info!("OAuth required on /mcp"),
        None if on_lambda => tracing::warn!("TRACKSIDE_AUTH_ISSUER unset: /mcp is open"),
        None => {}
    }
    let summariser = Summariser::from_env().await.map(Arc::new);
    let memory = memory::from_env().await;
    let ct = tokio_util::sync::CancellationToken::new();
    let app = build_app_with(
        store,
        memory,
        summariser,
        clock::from_env(),
        auth,
        stateless,
        on_lambda,
        ct.clone(),
        Ops {
            metrics,
            snapshot,
            started,
        },
    );

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

/// The operational side of the app: where metric lines go, and what `/healthz` reports.
#[derive(Clone)]
struct Ops {
    metrics: Option<Arc<dyn Sink>>,
    snapshot: SnapshotInfo,
    started: Instant,
}

impl Default for Ops {
    fn default() -> Self {
        Self {
            metrics: None,
            snapshot: SnapshotInfo::default(),
            started: Instant::now(),
        }
    }
}

/// The app with no metrics and an empty health report (the tests' default).
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
fn build_app(
    store: Arc<dyn Store>,
    memory: Arc<dyn Memory>,
    summariser: Option<Arc<Summariser>>,
    clock: Arc<dyn clock::Clock>,
    auth: Option<Arc<Auth>>,
    stateless: bool,
    on_lambda: bool,
    ct: tokio_util::sync::CancellationToken,
) -> axum::Router {
    build_app_with(
        store,
        memory,
        summariser,
        clock,
        auth,
        stateless,
        on_lambda,
        ct,
        Ops::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_app_with(
    store: Arc<dyn Store>,
    memory: Arc<dyn Memory>,
    summariser: Option<Arc<Summariser>>,
    clock: Arc<dyn clock::Clock>,
    auth: Option<Arc<Auth>>,
    stateless: bool,
    on_lambda: bool,
    ct: tokio_util::sync::CancellationToken,
    ops: Ops,
) -> axum::Router {
    let metrics = ops.metrics.clone();
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
        move || {
            Ok(Trackside::new(
                store.clone(),
                memory.clone(),
                summariser.clone(),
                clock.clone(),
            )
            .with_telemetry(metrics.clone()))
        },
        LocalSessionManager::default().into(),
        config,
    );

    let mcp = axum::Router::new().nest_service("/mcp", service);
    let app = match auth {
        None => mcp,
        Some(auth) => {
            use axum::routing::get;
            mcp.layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                auth::require_token,
            ))
            .merge(
                axum::Router::new()
                    .route(
                        "/.well-known/oauth-protected-resource",
                        get(auth::protected_resource),
                    )
                    .route(
                        "/.well-known/oauth-protected-resource/mcp",
                        get(auth::protected_resource),
                    )
                    .route(
                        "/.well-known/oauth-authorization-server",
                        get(auth::authorization_server),
                    )
                    .with_state(auth),
            )
        }
    };
    app.route("/healthz", axum::routing::get(move || healthz(ops.clone())))
        .layer(CorsLayer::permissive())
}

/// Liveness plus what is being served. Never writes a metric line: health checks would
/// drown the tool calls.
async fn healthz(ops: Ops) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "ok": true,
        "snapshot_date": ops.snapshot.snapshot_date,
        "meetings": ops.snapshot.meetings,
        "uptime_s": ops.started.elapsed().as_secs(),
    }))
}
