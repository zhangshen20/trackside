//! Trackside MCP server: Streamable HTTP (MCP 2025-11-25) on axum.
//!
//! `TRACKSIDE_FIXTURE` selects the JSON fixture to serve (default `fixtures/demo.json`);
//! `TRACKSIDE_BIND` the listen address (default `127.0.0.1:8000`). The endpoint is `/mcp`.
//! OAuth 2.1 (required by Alexa+) is the next milestone; until then the server is open
//! and must only be bound locally or behind a tunnel.

mod tools;

use std::sync::Arc;

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tower_http::cors::CorsLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use tools::Trackside;
use trackside_core::{FixtureStore, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let fixture =
        std::env::var("TRACKSIDE_FIXTURE").unwrap_or_else(|_| "fixtures/demo.json".into());
    let bind = std::env::var("TRACKSIDE_BIND").unwrap_or_else(|_| "127.0.0.1:8000".into());
    let store: Arc<dyn Store> = Arc::new(FixtureStore::load(&fixture)?);
    tracing::info!(%fixture, %bind, "trackside starting");

    let ct = tokio_util::sync::CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(Trackside::new(store.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .nest_service("/mcp", service)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await?;
    Ok(())
}
