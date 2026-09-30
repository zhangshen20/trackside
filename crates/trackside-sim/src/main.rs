//! Trackside simulator: a web page that plays the Alexa+ side of a conversation with the
//! Trackside add-on, for testing and demos where a real Alexa+ device isn't available (the
//! Alexa+ MCP toolkit is US-only).
//!
//! The page takes speech (or typing), and this server runs the turn: a Claude model on Amazon
//! Bedrock picks Trackside's tools, the server calls them on the MCP server over Streamable
//! HTTP with the signed-in user's token, and the answer comes back as speech plus a screen
//! card built from the tools' structured content.
//!
//! Routes, all under `/sim` so the simulator can share the MCP server's hostname:
//! `GET /sim` (the page), `GET /sim/link` and `GET /sim/callback` (account linking),
//! `GET /sim/api/session`, `POST /sim/api/chat`, `POST /sim/api/unlink`.
//!
//! Configuration (environment):
//!
//! - `TRACKSIDE_SIM_MCP_URL`: the MCP endpoint (default `http://127.0.0.1:8000/mcp`)
//! - `TRACKSIDE_SIM_MODEL`: Bedrock model or inference profile (default `au.anthropic.claude-opus-5-5`)
//! - `TRACKSIDE_SIM_MODEL_FIELDS`: extra model request fields as JSON (default
//!   `{"output_config":{"effort":"low"}}`, since a voice answer should come back quickly);
//!   `none` sends none
//! - `TRACKSIDE_SIM_TODAY`: the date the assistant treats as today (default: today in
//!   Melbourne), for demos on a snapshot of past racing
//! - `TRACKSIDE_SIM_AUTH_DOMAIN`, `TRACKSIDE_SIM_CLIENT_ID`, `TRACKSIDE_SIM_CLIENT_SECRET`:
//!   the Cognito sign-in client for account linking. Unset, the simulator calls the MCP
//!   server without a token, which suits a local server without auth.
//! - `TRACKSIDE_SIM_PUBLIC_URL`: the public base URL, when the Host header isn't it
//! - `TRACKSIDE_SIM_BIND`: local listen address (default `127.0.0.1:8001`)

mod agent;
mod link;
mod mcp;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use agent::{Bedrock, Model, ToolRunner, Utterance};
use link::{Link, Session};
use mcp::{McpClient, ToolDef, ToolOutput, Unauthorized};

const PAGE: &str = include_str!("../static/index.html");

struct App {
    mcp: McpClient,
    model: Box<dyn Model>,
    link: Option<Link>,
    today: Option<String>,
    tools: RwLock<Option<Vec<ToolDef>>>,
}

type Shared = Arc<App>;

#[tokio::main]
async fn main() -> Result<()> {
    let on_lambda = std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok();
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

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let aws = aws_config::load_from_env().await;
    let extra = match env("TRACKSIDE_SIM_MODEL_FIELDS").as_deref() {
        Some("none") => None,
        Some(json) => Some(serde_json::from_str(json)?),
        None => Some(json!({"output_config": {"effort": "low"}})),
    };
    let model = Bedrock {
        client: aws_sdk_bedrockruntime::Client::new(&aws),
        model_id: env("TRACKSIDE_SIM_MODEL")
            .unwrap_or_else(|| "au.anthropic.claude-opus-5-5".into()),
        extra,
    };
    let link = match (
        env("TRACKSIDE_SIM_AUTH_DOMAIN"),
        env("TRACKSIDE_SIM_CLIENT_ID"),
        env("TRACKSIDE_SIM_CLIENT_SECRET"),
    ) {
        (Some(domain), Some(client_id), Some(client_secret)) => Some(Link {
            domain: domain.trim_end_matches('/').to_string(),
            client_id,
            client_secret,
            public_url: env("TRACKSIDE_SIM_PUBLIC_URL"),
            http: http.clone(),
        }),
        _ => None,
    };
    let mcp_url =
        env("TRACKSIDE_SIM_MCP_URL").unwrap_or_else(|| "http://127.0.0.1:8000/mcp".into());
    tracing::info!(%mcp_url, model = %model.model_id, linking = link.is_some(), "trackside simulator");
    let app = router(Arc::new(App {
        mcp: McpClient::new(mcp_url, http),
        model: Box::new(model),
        link,
        today: env("TRACKSIDE_SIM_TODAY"),
        tools: RwLock::new(None),
    }));

    if on_lambda {
        return lambda_http::run(app)
            .await
            .map_err(|e| anyhow::anyhow!("lambda runtime: {e}"));
    }
    let bind = env("TRACKSIDE_SIM_BIND").unwrap_or_else(|| "127.0.0.1:8001".into());
    tracing::info!("open http://{bind}/sim");
    axum::serve(tokio::net::TcpListener::bind(&bind).await?, app).await?;
    Ok(())
}

fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/sim") }))
        .route("/sim", get(|| async { Html(PAGE) }))
        .route("/sim/", get(|| async { Redirect::temporary("/sim") }))
        .route("/sim/link", get(start_link))
        .route("/sim/callback", get(finish_link))
        .route("/sim/api/session", get(session))
        .route("/sim/api/unlink", post(unlink))
        .route("/sim/api/chat", post(chat))
        .with_state(app)
}

fn error(status: StatusCode, code: &str, message: impl std::fmt::Display) -> Response {
    (
        status,
        Json(json!({"error": code, "message": message.to_string()})),
    )
        .into_response()
}

fn with_cookies(mut resp: Response, cookies: Vec<HeaderValue>) -> Response {
    for c in cookies {
        resp.headers_mut().append(header::SET_COOKIE, c);
    }
    resp
}

async fn session(State(app): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let s = Session::from_headers(&headers);
    Json(json!({
        "linking": app.link.is_some(),
        "linked": app.link.is_none() || s.access.is_some() || s.refresh.is_some(),
        "who": s.who,
    }))
}

async fn start_link(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let Some(link) = &app.link else {
        return Redirect::to("/sim").into_response();
    };
    let base = link::base_url(&headers, link.public_url.as_deref());
    let (url, cookie) = link.start(&base);
    with_cookies(Redirect::to(&url).into_response(), vec![cookie])
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn finish_link(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let Some(link) = &app.link else {
        return Redirect::to("/sim").into_response();
    };
    let base = link::base_url(&headers, link.public_url.as_deref());
    let (Some(code), Some(state)) = (q.code, q.state) else {
        let why = q.error.unwrap_or_else(|| "no code".into());
        return Redirect::to(&format!(
            "/sim?link_error={}",
            why.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        ))
        .into_response();
    };
    match link
        .finish(&base, &Session::from_headers(&headers), &code, &state)
        .await
    {
        Ok(cookies) => with_cookies(Redirect::to("/sim").into_response(), cookies),
        Err(e) => {
            tracing::warn!(error = %e, "account linking failed");
            Redirect::to("/sim?link_error=failed").into_response()
        }
    }
}

async fn unlink(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let s = Session::from_headers(&headers);
    let secure = app
        .link
        .as_ref()
        .is_some_and(|l| link::base_url(&headers, l.public_url.as_deref()).starts_with("https"));
    if let (Some(link), Some(rt)) = (&app.link, &s.refresh) {
        link.revoke(rt).await;
    }
    with_cookies(
        Json(json!({"linked": false})).into_response(),
        Link::clear_cookies(secure),
    )
}

#[derive(Deserialize)]
struct ChatRequest {
    history: Vec<Utterance>,
}

/// Tool calls for one chat turn, as one user. A refreshed token is kept for the rest of the
/// turn and handed back to the browser as a cookie.
struct UserTools<'a> {
    app: &'a App,
    token: Mutex<Option<String>>,
    refresh: Option<String>,
    refreshed: Mutex<Option<link::Tokens>>,
}

impl UserTools<'_> {
    async fn try_refresh(&self) -> Result<bool> {
        let (Some(link), Some(rt)) = (&self.app.link, &self.refresh) else {
            return Ok(false);
        };
        if self.refreshed.lock().await.is_some() {
            return Ok(false);
        }
        let tokens = link.refresh(rt).await?;
        *self.token.lock().await = Some(tokens.access_token.clone());
        *self.refreshed.lock().await = Some(tokens);
        Ok(true)
    }

    async fn with_token<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: Fn(Option<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let token = self.token.lock().await.clone();
        match f(token).await {
            Err(e) if e.is::<Unauthorized>() && self.try_refresh().await.unwrap_or(false) => {
                f(self.token.lock().await.clone()).await
            }
            other => other,
        }
    }

    async fn tool_defs(&self) -> Result<Vec<ToolDef>> {
        if let Some(tools) = self.app.tools.read().await.clone() {
            return Ok(tools);
        }
        let tools = self
            .with_token(|t| async move { self.app.mcp.list_tools(t.as_deref()).await })
            .await?;
        *self.app.tools.write().await = Some(tools.clone());
        Ok(tools)
    }
}

#[async_trait]
impl ToolRunner for UserTools<'_> {
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput> {
        self.with_token(|t| {
            let args = args.clone();
            async move { self.app.mcp.call_tool(t.as_deref(), name, args).await }
        })
        .await
    }
}

fn today(app: &App) -> String {
    use chrono::NaiveDate;
    let date = app
        .today
        .as_deref()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| {
            Utc::now()
                .with_timezone(&chrono_tz::Australia::Melbourne)
                .date_naive()
        });
    date.format("%A %-d %B %Y (%Y-%m-%d)").to_string()
}

async fn chat(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let s = Session::from_headers(&headers);
    if app.link.is_some() && s.access.is_none() && s.refresh.is_none() {
        return error(
            StatusCode::UNAUTHORIZED,
            "link",
            "Link your Trackside account first.",
        );
    }
    let tools = UserTools {
        app: &app,
        token: Mutex::new(s.access.clone()),
        refresh: s.refresh.clone(),
        refreshed: Mutex::new(None),
    };
    let secure = app
        .link
        .as_ref()
        .is_some_and(|l| link::base_url(&headers, l.public_url.as_deref()).starts_with("https"));
    let result = async {
        let defs = tools.tool_defs().await?;
        agent::respond(
            app.model.as_ref(),
            &tools,
            &defs,
            &agent::system_prompt(&today(&app)),
            &req.history,
        )
        .await
    }
    .await;
    let cookies = match (&app.link, tools.refreshed.lock().await.as_ref()) {
        (Some(link), Some(t)) => link.token_cookies(t, secure),
        _ => vec![],
    };
    match result {
        Ok(reply) => with_cookies(Json(reply).into_response(), cookies),
        Err(e) if e.is::<Unauthorized>() => with_cookies(
            error(
                StatusCode::UNAUTHORIZED,
                "link",
                "Your Trackside link has expired. Link your account again.",
            ),
            Link::clear_cookies(secure),
        ),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "chat turn failed");
            error(StatusCode::BAD_GATEWAY, "failed", format!("{e:#}"))
        }
    }
}
