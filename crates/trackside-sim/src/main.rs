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
//! `GET /sim/api/session`, `POST /sim/api/chat`, `POST /sim/api/speak`, `POST /sim/api/unlink`.
//!
//! The page is also an MCP Apps host: when a tool names a `ui://` resource in its `_meta`, the
//! page shows that MCP App in a sandboxed iframe instead of its own card, and relays the App's
//! tool calls. `GET /sim/api/apps` maps tools to their Apps, `GET /sim/api/ui?uri=` reads an
//! App's HTML from the MCP server, and `POST /sim/api/tool` runs a tool call an App made.
//!
//! Configuration (environment):
//!
//! - `TRACKSIDE_SIM_MCP_URL`: the MCP endpoint (default `http://127.0.0.1:8000/mcp`)
//! - `TRACKSIDE_SIM_MODEL`: Bedrock model or inference profile (default `au.anthropic.claude-haiku-4-5-20251001-v1:0`, the
//!   model `explain_race` uses too, so one Bedrock quota covers both)
//! - `TRACKSIDE_SIM_MODEL_FIELDS`: extra model request fields as JSON, e.g.
//!   `{"output_config":{"effort":"low"}}` for a model that takes an effort setting
//! - `TRACKSIDE_SIM_TODAY`: the date the assistant treats as today (default: today in
//!   Melbourne), for demos on a snapshot of past racing
//! - `TRACKSIDE_SIM_AUTH_DOMAIN`, `TRACKSIDE_SIM_CLIENT_ID`, `TRACKSIDE_SIM_CLIENT_SECRET`:
//!   the Cognito sign-in client for account linking. Unset, the simulator calls the MCP
//!   server without a token, which suits a local server without auth.
//! - `TRACKSIDE_SIM_PUBLIC_URL`: the public base URL, when the Host header isn't it
//! - `TRACKSIDE_SIM_BIND`: local listen address (default `127.0.0.1:8001`)
//! - `TRACKSIDE_SIM_VOICE`: the Amazon Polly voice that speaks answers (default `Olivia`, en-AU);
//!   `off` leaves speech to the browser's own voice

mod agent;
mod link;
mod mcp;
mod voice;

use std::collections::HashMap;
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
    /// MCP App HTML and its resource `_meta` by `ui://` URI, read once from the MCP server.
    apps: RwLock<HashMap<String, (String, Value)>>,
    voice: Option<voice::Voice>,
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
        Some(json) => Some(serde_json::from_str(json)?),
        None => None,
    };
    let model = Bedrock {
        client: aws_sdk_bedrockruntime::Client::new(&aws),
        model_id: env("TRACKSIDE_SIM_MODEL")
            .unwrap_or_else(|| "au.anthropic.claude-haiku-4-5-20251001-v1:0".into()),
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
        apps: RwLock::new(HashMap::new()),
        voice: voice::Voice::from_env(&aws, env("TRACKSIDE_SIM_VOICE")),
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
        .route("/sim/api/speak", post(speak))
        .route("/sim/api/apps", get(apps))
        .route("/sim/api/ui", get(app_html))
        .route("/sim/api/tool", post(app_tool))
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
        // The day the assistant treats as today, and whether it was pinned for a rehearsal
        // (TRACKSIDE_SIM_TODAY) rather than read from the clock, so the screen can say so.
        "today": today_date(&app).format("%Y-%m-%d").to_string(),
        "simulated_date": app.today.is_some(),
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

impl<'a> UserTools<'a> {
    /// The linked user's tokens from the request's cookies, or a 401 asking them to link.
    #[allow(clippy::result_large_err)]
    fn for_request(app: &'a App, headers: &HeaderMap) -> Result<Self, Response> {
        let s = Session::from_headers(headers);
        if app.link.is_some() && s.access.is_none() && s.refresh.is_none() {
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "link",
                "Link your Trackside account first.",
            ));
        }
        Ok(Self {
            app,
            token: Mutex::new(s.access),
            refresh: s.refresh,
            refreshed: Mutex::new(None),
        })
    }

    /// The JSON response, carrying a refreshed token as cookies, or the error as the page
    /// expects it (401 means link again).
    async fn respond<T: serde::Serialize>(
        &self,
        headers: &HeaderMap,
        what: &str,
        result: Result<T>,
    ) -> Response {
        let secure =
            self.app.link.as_ref().is_some_and(|l| {
                link::base_url(headers, l.public_url.as_deref()).starts_with("https")
            });
        let cookies = match (&self.app.link, self.refreshed.lock().await.as_ref()) {
            (Some(link), Some(t)) => link.token_cookies(t, secure),
            _ => vec![],
        };
        match result {
            Ok(body) => with_cookies(Json(body).into_response(), cookies),
            Err(e) if e.is::<Unauthorized>() => with_cookies(
                error(
                    StatusCode::UNAUTHORIZED,
                    "link",
                    "Your Trackside link has expired. Link your account again.",
                ),
                Link::clear_cookies(secure),
            ),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "{what} failed");
                error(StatusCode::BAD_GATEWAY, "failed", format!("{e:#}"))
            }
        }
    }

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
    calendar(today_date(app))
}

/// The date the assistant treats as today: `TRACKSIDE_SIM_TODAY` when set, else today in
/// Melbourne.
fn today_date(app: &App) -> chrono::NaiveDate {
    app.today
        .as_deref()
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| {
            Utc::now()
                .with_timezone(&chrono_tz::Australia::Melbourne)
                .date_naive()
        })
}

/// Today plus the fortnight around it, so the model maps "last Saturday" or "on Friday" to a
/// date by lookup rather than by arithmetic, which it gets wrong.
fn calendar(today: chrono::NaiveDate) -> String {
    let days = (-7..=7)
        .filter(|&d| d != 0)
        .map(|d| {
            let day = today + chrono::Duration::days(d);
            day.format("%A %-d %B = %Y-%m-%d").to_string()
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{} ({}). The week either side: {days}",
        today.format("%A %-d %B %Y"),
        today.format("%Y-%m-%d")
    )
}

async fn chat(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Response {
    let tools = match UserTools::for_request(&app, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
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
    tools.respond(&headers, "chat turn", result).await
}

/// Which tools draw their results with an MCP App: tool name to `ui://` URI.
async fn apps(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let tools = match UserTools::for_request(&app, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let result = tools.tool_defs().await.map(|defs| {
        defs.iter()
            .filter_map(|d| Some((d.name.clone(), d.app_uri()?.to_string())))
            .collect::<HashMap<_, _>>()
    });
    tools.respond(&headers, "listing apps", result).await
}

#[derive(Deserialize)]
struct UiQuery {
    uri: String,
}

/// An MCP App's HTML and its resource `_meta` (the page builds the App's CSP from `ui.csp`),
/// read from the MCP server once and kept.
async fn app_html(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<UiQuery>,
) -> Response {
    if !q.uri.starts_with("ui://") {
        return error(StatusCode::BAD_REQUEST, "uri", "not a ui:// resource");
    }
    let tools = match UserTools::for_request(&app, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    if let Some((html, meta)) = app.apps.read().await.get(&q.uri).cloned() {
        return Json(json!({ "uri": q.uri, "html": html, "meta": meta })).into_response();
    }
    let result = tools
        .with_token(|t| {
            let uri = q.uri.clone();
            let app = app.clone();
            async move { app.mcp.read_app(t.as_deref(), &uri).await }
        })
        .await;
    if let Ok(read) = &result {
        app.apps.write().await.insert(q.uri.clone(), read.clone());
    }
    let uri = q.uri.clone();
    tools
        .respond(
            &headers,
            "reading an app",
            result.map(|(html, meta)| json!({ "uri": uri, "html": html, "meta": meta })),
        )
        .await
}

#[derive(Deserialize)]
struct AppToolCall {
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// A tool call an MCP App made through the page, run as the linked user. The App gets the
/// whole result, as MCP Apps hosts pass it on.
async fn app_tool(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<AppToolCall>,
) -> Response {
    let tools = match UserTools::for_request(&app, &headers) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    let result = tools
        .with_token(|t| {
            let (app, name, args) = (app.clone(), req.name.clone(), req.arguments.clone());
            async move { app.mcp.call_tool_raw(t.as_deref(), &name, args).await }
        })
        .await;
    tools.respond(&headers, "app tool call", result).await
}

#[derive(Deserialize)]
struct SpeakRequest {
    text: String,
}

/// The answer as MP3 from Amazon Polly, for a signed-in user. 404 when Polly is off, so the
/// page falls back to the browser's voice.
async fn speak(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<SpeakRequest>,
) -> Response {
    let s = Session::from_headers(&headers);
    if app.link.is_some() && s.access.is_none() && s.refresh.is_none() {
        return error(
            StatusCode::UNAUTHORIZED,
            "link",
            "Link your Trackside account first.",
        );
    }
    let Some(voice) = &app.voice else {
        return error(StatusCode::NOT_FOUND, "off", "Polly speech is off.");
    };
    match voice.speak(&req.text).await {
        Ok(mp3) => (
            [
                (header::CONTENT_TYPE, "audio/mpeg"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            mp3,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "Polly speech failed");
            error(StatusCode::BAD_GATEWAY, "failed", format!("{e:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::calendar;
    use chrono::NaiveDate;

    #[test]
    fn calendar_names_last_saturday() {
        let sunday = NaiveDate::from_ymd_opt(2026, 9, 27).unwrap();
        let text = calendar(sunday);
        assert!(text.starts_with("Sunday 27 September 2026 (2026-09-27)"));
        assert!(text.contains("Saturday 26 September = 2026-09-26"));
        assert!(text.contains("Saturday 3 October = 2026-10-03"));
        assert!(!text.contains("2026-09-19"));
    }
}
