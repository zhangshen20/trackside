//! The HTTP surface end to end: challenges, metadata, scope tiers, JWKS fetching, and an rmcp
//! Streamable HTTP server whose tools read the caller.

use std::sync::OnceLock;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use base64::Engine;
use jsonwebtoken::{jwk::JwkSet, EncodingKey, Header};
use mcp_cognito_auth::{Caller, CognitoAuth};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::*,
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler,
};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::{json, Value};
use tower::ServiceExt;

const ISSUER: &str = "https://cognito-idp.ap-southeast-2.amazonaws.com/ap-southeast-2_test";
const DOMAIN: &str = "https://test.auth.ap-southeast-2.amazoncognito.com";
const HOST: &str = "abc.execute-api.ap-southeast-2.amazonaws.com";

/// A throwaway RSA key made once per test run, and its JWKS.
fn key() -> &'static (EncodingKey, JwkSet) {
    static KEY: OnceLock<(EncodingKey, JwkSet)> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = private.to_pkcs1_pem(Default::default()).unwrap();
        let b64 = |v: Vec<u8>| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let jwks = serde_json::from_value(json!({ "keys": [{
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": "test",
            "n": b64(private.n().to_bytes_be()), "e": b64(private.e().to_bytes_be()),
        }]}))
        .unwrap();
        (EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(), jwks)
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn sign(claims: Value) -> String {
    let mut head = Header::new(jsonwebtoken::Algorithm::RS256);
    head.kid = Some("test".into());
    jsonwebtoken::encode(&head, &claims, &key().0).unwrap()
}

/// A Cognito-shaped access token.
fn token(scope: &str, client: &str, sub: &str) -> String {
    sign(json!({
        "iss": ISSUER, "sub": sub, "token_use": "access", "client_id": client,
        "scope": scope, "exp": now() + 600, "username": format!("{sub}-name"),
    }))
}

// A small MCP server: one tool that says who is asking.
#[derive(Clone)]
struct Echo;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GreetArgs {
    /// Who to greet.
    name: String,
}

#[tool_router]
impl Echo {
    #[tool(description = "Say who is calling")]
    async fn whoami(&self, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
        let caller = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(Caller::from_parts)
            .map(|c| format!("{} via {}", c.subject, c.client_id))
            .unwrap_or_else(|| "nobody".into());
        Ok(CallToolResult::success(vec![ContentBlock::text(caller)]))
    }

    #[tool(description = "Greet someone")]
    async fn greet(
        &self,
        Parameters(args): Parameters<GreetArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "G'day, {}",
            args.name
        ))]))
    }
}

#[tool_handler]
impl ServerHandler for Echo {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn auth() -> CognitoAuth {
    CognitoAuth::builder(ISSUER, DOMAIN)
        .clients(["alexa", "service"])
        .scopes("echo/mcp:service", "echo/mcp:tools")
        .resource_name("Echo")
        .build()
}

/// Stateless JSON responses, as on AWS Lambda.
fn mcp_router() -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .disable_allowed_hosts();
    let service =
        StreamableHttpService::new(|| Ok(Echo), LocalSessionManager::default().into(), config);
    Router::new().nest_service("/mcp", service)
}

async fn app() -> Router {
    let auth = auth();
    auth.set_jwks(&key().1).await;
    auth.protect(mcp_router())
        .route("/healthz", axum::routing::get(|| async { "ok" }))
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn rpc(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

fn call(name: &str, args: Value) -> Value {
    rpc("tools/call", json!({ "name": name, "arguments": args }))
}

fn challenge(headers: &HeaderMap) -> &str {
    headers[header::WWW_AUTHENTICATE].to_str().unwrap()
}

#[tokio::test]
async fn no_token_gets_401_pointing_at_metadata() {
    let app = app().await;
    let (status, headers, body) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        challenge(&headers),
        format!(
            "Bearer resource_metadata=\"https://{HOST}/.well-known/oauth-protected-resource/mcp\""
        )
    );
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn bad_tokens_are_refused() {
    let app = app().await;
    let list = || Some(rpc("tools/list", json!({})));
    let id_token = sign(json!({
        "iss": ISSUER, "sub": "u", "token_use": "id", "client_id": "alexa",
        "scope": "echo/mcp:tools", "exp": now() + 600,
    }));
    let expired = sign(json!({
        "iss": ISSUER, "sub": "u", "token_use": "access", "client_id": "alexa",
        "scope": "echo/mcp:tools", "exp": now() - 3600,
    }));
    let other_pool = sign(json!({
        "iss": "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_other", "sub": "u",
        "token_use": "access", "client_id": "alexa", "scope": "echo/mcp:tools", "exp": now() + 600,
    }));
    let cases = [
        ("not-a-jwt".to_string(), "malformed token"),
        (token("echo/mcp:tools", "stranger", "u"), "unknown client"),
        (id_token, "not an access token"),
        (expired, "invalid or expired token"),
        (other_pool, "invalid or expired token"),
    ];
    for (t, why) in cases {
        let (status, headers, body) = send(&app, "POST", "/mcp", Some(&t), list()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{why}");
        assert!(
            challenge(&headers).contains("error=\"invalid_token\""),
            "{why}"
        );
        assert!(
            body["error_description"].as_str().unwrap().contains(why),
            "{body}"
        );
    }
}

#[tokio::test]
async fn service_token_discovers_but_cannot_call_tools() {
    let app = app().await;
    let service = token("echo/mcp:service", "service", "service");
    let init = rpc(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    let (status, _, body) = send(&app, "POST", "/mcp", Some(&service), Some(init)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["result"]["serverInfo"].is_object(), "{body}");
    let (status, _, list) = send(
        &app,
        "POST",
        "/mcp",
        Some(&service),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);

    let (status, headers, _) = send(
        &app,
        "POST",
        "/mcp",
        Some(&service),
        Some(call("whoami", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let c = challenge(&headers);
    assert!(c.contains("error=\"insufficient_scope\""), "{c}");
    assert!(c.contains("scope=\"echo/mcp:tools\""), "{c}");

    // A batch that hides a tool call behind a list still needs the user scope.
    let batch = json!([rpc("tools/list", json!({})), call("whoami", json!({}))]);
    let (status, _, _) = send(&app, "POST", "/mcp", Some(&service), Some(batch)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn user_token_calls_tools_and_the_tool_sees_the_caller() {
    let app = app().await;
    let ann = token("openid email echo/mcp:tools", "alexa", "ann");
    let (status, _, list) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a user token may also discover: {list}"
    );
    let (status, _, who) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(call("whoami", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(who["result"]["content"][0]["text"], "ann via alexa");
    let (_, _, hi) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(call("greet", json!({"name": "Bob"}))),
    )
    .await;
    assert_eq!(hi["result"]["content"][0]["text"], "G'day, Bob");
}

#[tokio::test]
async fn metadata_is_public_and_points_at_cognito() {
    let app = app().await;
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let (status, _, prm) = send(&app, "GET", path, None, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(prm["resource"], format!("https://{HOST}/mcp"));
        assert_eq!(
            prm["authorization_servers"],
            json!([format!("https://{HOST}")])
        );
        assert_eq!(
            prm["scopes_supported"],
            json!(["echo/mcp:service", "echo/mcp:tools"])
        );
        assert_eq!(prm["resource_name"], "Echo");
    }
    let (status, _, asm) = send(
        &app,
        "GET",
        "/.well-known/oauth-authorization-server",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(asm["issuer"], format!("https://{HOST}"));
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        asm["authorization_endpoint"],
        format!("{DOMAIN}/oauth2/authorize")
    );
    assert_eq!(asm["token_endpoint"], format!("{DOMAIN}/oauth2/token"));
    assert_eq!(asm["jwks_uri"], format!("{ISSUER}/.well-known/jwks.json"));

    let (status, _, _) = send(&app, "GET", "/healthz", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "routes merged after protect stay public"
    );
}

#[tokio::test]
async fn a_custom_resource_path_moves_the_metadata() {
    let auth = CognitoAuth::builder(ISSUER, DOMAIN)
        .resource_path("api/mcp/")
        .public_url("https://mcp.example.com/")
        .build();
    let app: Router = auth.metadata_router();
    let (status, _, prm) = send(
        &app,
        "GET",
        "/.well-known/oauth-protected-resource/api/mcp",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prm["resource"], "https://mcp.example.com/api/mcp");
    assert_eq!(
        prm["authorization_servers"],
        json!(["https://mcp.example.com"])
    );
}

#[tokio::test]
async fn signing_keys_are_fetched_from_the_pool() {
    // Serve the JWKS as Cognito does, under the issuer.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}/pool", listener.local_addr().unwrap());
    let jwks = serde_json::to_value(&key().1).unwrap();
    let pool = Router::new().route(
        "/pool/.well-known/jwks.json",
        axum::routing::get(move || async move { axum::Json(jwks) }),
    );
    tokio::spawn(async move { axum::serve(listener, pool).await.unwrap() });

    let auth = CognitoAuth::builder(&issuer, DOMAIN)
        .jwks_retry(Duration::from_millis(0))
        .build();
    let app = auth.protect(mcp_router());
    let t = sign(json!({
        "iss": issuer, "sub": "ann", "token_use": "access", "client_id": "any",
        "scope": "mcp:tools", "exp": now() + 600,
    }));
    let (status, _, who) = send(
        &app,
        "POST",
        "/mcp",
        Some(&t),
        Some(call("whoami", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert_eq!(who["result"]["content"][0]["text"], "ann via any");
}

#[test]
fn from_env_is_off_without_an_issuer() {
    // The only test touching these variables, so no races with other tests.
    std::env::remove_var("MCP_AUTH_ISSUER");
    assert!(CognitoAuth::from_env().unwrap().is_none());
    std::env::set_var("MCP_AUTH_ISSUER", ISSUER);
    std::env::remove_var("MCP_AUTH_DOMAIN");
    assert!(CognitoAuth::from_env().is_err());
    std::env::set_var("MCP_AUTH_DOMAIN", DOMAIN);
    std::env::set_var("MCP_AUTH_SCOPE_USER", "x/mcp:tools");
    let auth = CognitoAuth::from_env().unwrap().unwrap();
    let prm = auth.protected_resource_metadata(&HeaderMap::new());
    assert_eq!(
        prm["scopes_supported"],
        json!(["mcp:service", "x/mcp:tools"])
    );
    for k in ["MCP_AUTH_ISSUER", "MCP_AUTH_DOMAIN", "MCP_AUTH_SCOPE_USER"] {
        std::env::remove_var(k);
    }
}
