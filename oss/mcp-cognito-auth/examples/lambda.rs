//! A Streamable HTTP MCP server, protected by Cognito, that runs the same binary locally and on
//! AWS Lambda behind an API Gateway HTTP API (or a function URL).
//!
//! Locally, open (no `MCP_AUTH_ISSUER`) and with sessions:
//!
//! ```sh
//! cargo run --example lambda
//! npx @modelcontextprotocol/inspector   # connect to http://127.0.0.1:8000/mcp
//! ```
//!
//! On Lambda, build a `bootstrap` for arm64 with `cargo lambda build --release --arm64 --example
//! lambda` and deploy `examples/template.yaml` (see the README).
//!
//! What changes on Lambda, and why:
//!
//! - Each request may land on a different instance, so the server is stateless (no
//!   `Mcp-Session-Id`) and answers with plain JSON instead of an SSE stream.
//! - API Gateway's own domain arrives in `Host`, so rmcp's localhost-only DNS-rebinding check
//!   is turned off there; it still guards the local run.

use mcp_cognito_auth::{Caller, CognitoAuth};
use rmcp::{
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler,
};

#[derive(Clone)]
struct Hello;

#[tool_router]
impl Hello {
    #[tool(
        description = "Greet the signed-in user by name",
        annotations(read_only_hint = true)
    )]
    async fn hello(&self, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let who = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(Caller::from_parts)
            .map(|c| c.username.clone().unwrap_or_else(|| c.subject.clone()))
            .unwrap_or_else(|| "stranger".into());
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Hello, {who}."
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for Hello {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info({
            let mut info = Implementation::from_build_env();
            info.name = "hello".into();
            info
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let on_lambda = std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(!on_lambda)
        .init();

    let mut config = StreamableHttpServerConfig::default();
    if on_lambda {
        config = config
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .disable_allowed_hosts();
    }
    let service =
        StreamableHttpService::new(|| Ok(Hello), LocalSessionManager::default().into(), config);
    let mcp = axum::Router::new().nest_service("/mcp", service);

    let app = match CognitoAuth::from_env()? {
        Some(auth) => auth.protect(mcp),
        None => {
            tracing::warn!("MCP_AUTH_ISSUER unset: /mcp is open");
            mcp
        }
    }
    .route("/healthz", axum::routing::get(|| async { "ok" }));

    if on_lambda {
        return lambda_http::run(app).await;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8000").await?;
    tracing::info!("listening on http://127.0.0.1:8000/mcp");
    axum::serve(listener, app).await?;
    Ok(())
}
