//! Make a Streamable HTTP [MCP] server an OAuth 2.1 protected resource, with an
//! [Amazon Cognito] user pool as the authorization server.
//!
//! MCP clients (MCP Inspector, Claude, Alexa+) find out how to sign in from the server itself:
//! a `401` carries a `WWW-Authenticate` header pointing at the server's protected-resource
//! metadata ([RFC 9728]), which names an authorization server whose metadata ([RFC 8414]) lists
//! the authorize and token endpoints and PKCE `S256`. Cognito publishes OpenID discovery only,
//! without PKCE methods, so it can't be named directly. This crate fills the gap:
//!
//! - it checks Cognito access tokens on the MCP endpoint (signature against the pool's JWKS,
//!   issuer, expiry, `token_use`, and an optional allow-list of app clients);
//! - it enforces two tiers of scope: a **service** scope that may discover (`initialize`,
//!   `tools/list`, `ping`, notifications) and a **user** scope required for `tools/call`, the
//!   shape Alexa+ uses (`client_credentials` for discovery, authorization code + PKCE for a
//!   signed-in user's tool calls);
//! - it answers `401` / `403` with the challenge MCP clients expect, a `403` naming the scope
//!   to step up to;
//! - it serves `/.well-known/oauth-protected-resource` (and the path-suffixed form) and
//!   `/.well-known/oauth-authorization-server`, pointing at Cognito's endpoints;
//! - it attaches a [`Caller`] to each request, so tools know who is asking.
//!
//! It works with any axum router and fits [rmcp]'s `StreamableHttpService`, including
//! stateless servers on AWS Lambda.
//!
//! ```no_run
//! use mcp_cognito_auth::CognitoAuth;
//!
//! # fn mcp_service() -> axum::routing::MethodRouter { axum::routing::post(|| async { "" }) }
//! # async fn run() {
//! let auth = CognitoAuth::builder(
//!     "https://cognito-idp.ap-southeast-2.amazonaws.com/ap-southeast-2_AbCdEf123",
//!     "https://my-pool.auth.ap-southeast-2.amazoncognito.com",
//! )
//! .clients(["service-client-id", "user-client-id"])
//! .scopes("myapp/mcp:service", "myapp/mcp:tools")
//! .resource_name("My MCP server")
//! .build();
//!
//! // Only the routes passed to `protect` need a token; the metadata routes it adds are public.
//! let mcp = axum::Router::new().route("/mcp", mcp_service());
//! let app = auth.protect(mcp).route("/healthz", axum::routing::get(|| async { "ok" }));
//! # let _ = app;
//! # }
//! ```
//!
//! [MCP]: https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization
//! [Amazon Cognito]: https://docs.aws.amazon.com/cognito/latest/developerguide/cognito-user-pools.html
//! [RFC 9728]: https://www.rfc-editor.org/rfc/rfc9728
//! [RFC 8414]: https://www.rfc-editor.org/rfc/rfc8414
//! [rmcp]: https://crates.io/crates/rmcp

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

pub use jsonwebtoken::jwk;

/// Who is calling: attached to every request that passes the token check.
///
/// Read it from the request's extensions. In an rmcp tool, the HTTP request parts are in the
/// request context:
///
/// ```ignore
/// let caller = ctx
///     .extensions
///     .get::<http::request::Parts>()
///     .and_then(mcp_cognito_auth::Caller::from_parts);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The token's subject: the user's id for a signed-in user, the client id for a
    /// `client_credentials` token. Stable across sessions, so a good key for per-user memory.
    pub subject: String,
    /// The app client the token was issued to.
    pub client_id: String,
    /// The user name, for user tokens.
    pub username: Option<String>,
    /// The granted scopes, as the token lists them.
    pub scopes: Vec<String>,
    /// Whether the token carries the user scope, i.e. it acts for a signed-in user.
    pub is_user: bool,
}

impl Caller {
    /// The caller attached to a request by [`CognitoAuth::protect`], if any.
    pub fn from_parts(parts: &http::request::Parts) -> Option<&Caller> {
        parts.extensions.get::<Caller>()
    }
}

/// A configuration error from [`CognitoAuth::from_env`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// Builds a [`CognitoAuth`]. Start from [`CognitoAuth::builder`].
#[derive(Clone, Debug)]
pub struct Builder {
    issuer: String,
    domain: String,
    clients: Vec<String>,
    service_scope: String,
    user_scope: String,
    user_methods: Vec<String>,
    public_url: Option<String>,
    resource_path: String,
    resource_name: Option<String>,
    resource_documentation: Option<String>,
    max_body: usize,
    jwks_retry: Duration,
}

impl Builder {
    /// App client ids whose tokens are accepted. Empty (the default) accepts any client of the
    /// pool; name them in production.
    pub fn clients<I, S>(mut self, clients: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.clients = clients.into_iter().map(Into::into).collect();
        self
    }

    /// The discovery scope and the user scope, as the authorization server names them.
    ///
    /// Cognito prefixes custom scopes with the resource server's identifier
    /// (`myapp/mcp:tools`). Name them that way here, since clients copy these names from the
    /// metadata into their authorization requests. A granted `myapp/mcp:tools` also satisfies
    /// a configured bare `mcp:tools`. Default `mcp:service` and `mcp:tools`.
    pub fn scopes(mut self, service: impl Into<String>, user: impl Into<String>) -> Self {
        self.service_scope = service.into();
        self.user_scope = user.into();
        self
    }

    /// JSON-RPC methods that act for a signed-in user and need the user scope. Default
    /// `tools/call`. Everything else needs the service scope (or the user scope).
    pub fn user_methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.user_methods = methods.into_iter().map(Into::into).collect();
        self
    }

    /// The server's public base URL, e.g. `https://mcp.example.com`. Without it the base URL
    /// comes from each request's `Host` and `X-Forwarded-Proto` headers (https unless the host
    /// is local), which is right behind API Gateway, a Lambda function URL or a custom domain
    /// mapping.
    pub fn public_url(mut self, url: impl Into<String>) -> Self {
        self.public_url = Some(url.into().trim_end_matches('/').to_string());
        self
    }

    /// The MCP endpoint's path, used for the `resource` in the metadata and the path-suffixed
    /// metadata route. Default `/mcp`.
    pub fn resource_path(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        self.resource_path = format!("/{}", path.trim_matches('/'));
        self
    }

    /// A human-readable name for the server, shown by some clients on the consent screen.
    pub fn resource_name(mut self, name: impl Into<String>) -> Self {
        self.resource_name = Some(name.into());
        self
    }

    /// A link to the server's documentation, published in the metadata.
    pub fn resource_documentation(mut self, url: impl Into<String>) -> Self {
        self.resource_documentation = Some(url.into());
        self
    }

    /// The largest request body read to decide which scope a call needs. Default 1 MiB.
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    /// How long to wait before refetching the pool's JWKS after an unknown key id. Default 60 s.
    pub fn jwks_retry(mut self, wait: Duration) -> Self {
        self.jwks_retry = wait;
        self
    }

    /// Finishes the configuration.
    pub fn build(self) -> CognitoAuth {
        CognitoAuth(Arc::new(Inner {
            jwks_url: format!("{}/.well-known/jwks.json", self.issuer),
            config: self,
            keys: Default::default(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("an HTTP client with default TLS settings"),
        }))
    }
}

/// Token checks and OAuth metadata for an MCP endpoint, backed by a Cognito user pool.
///
/// Cheap to clone; clones share the cached signing keys.
#[derive(Clone)]
pub struct CognitoAuth(Arc<Inner>);

struct Inner {
    config: Builder,
    jwks_url: String,
    keys: RwLock<Keys>,
    http: reqwest::Client,
}

#[derive(Default)]
struct Keys {
    by_kid: HashMap<String, DecodingKey>,
    fetched: Option<Instant>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    token_use: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    scope: String,
}

impl fmt::Debug for CognitoAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CognitoAuth")
            .field("config", &self.0.config)
            .finish_non_exhaustive()
    }
}

impl CognitoAuth {
    /// Starts a configuration.
    ///
    /// - `issuer`: the user pool's issuer, `https://cognito-idp.<region>.amazonaws.com/<pool id>`
    /// - `domain`: the pool's OAuth domain, `https://<prefix>.auth.<region>.amazoncognito.com`
    ///   or a custom domain
    pub fn builder(issuer: impl Into<String>, domain: impl Into<String>) -> Builder {
        Builder {
            issuer: issuer.into().trim_end_matches('/').to_string(),
            domain: domain.into().trim_end_matches('/').to_string(),
            clients: Vec::new(),
            service_scope: "mcp:service".into(),
            user_scope: "mcp:tools".into(),
            user_methods: vec!["tools/call".into()],
            public_url: None,
            resource_path: "/mcp".into(),
            resource_name: None,
            resource_documentation: None,
            max_body: 1 << 20,
            jwks_retry: Duration::from_secs(60),
        }
    }

    /// Reads the configuration from the environment; `Ok(None)` when `MCP_AUTH_ISSUER` is
    /// unset, so a server can run open locally and protected when deployed.
    ///
    /// | Variable | Meaning |
    /// |---|---|
    /// | `MCP_AUTH_ISSUER` | user pool issuer URL |
    /// | `MCP_AUTH_DOMAIN` | the pool's OAuth domain (required with the issuer) |
    /// | `MCP_AUTH_CLIENTS` | comma-separated app client ids |
    /// | `MCP_AUTH_SCOPE_SERVICE`, `MCP_AUTH_SCOPE_USER` | scope names |
    /// | `MCP_AUTH_PUBLIC_URL` | public base URL |
    /// | `MCP_AUTH_RESOURCE_PATH` | MCP endpoint path (default `/mcp`) |
    /// | `MCP_AUTH_RESOURCE_NAME` | display name |
    pub fn from_env() -> Result<Option<Self>, ConfigError> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let Some(issuer) = var("MCP_AUTH_ISSUER") else {
            return Ok(None);
        };
        let domain = var("MCP_AUTH_DOMAIN").ok_or_else(|| {
            ConfigError("MCP_AUTH_DOMAIN is required with MCP_AUTH_ISSUER".into())
        })?;
        let mut b = Self::builder(issuer, domain);
        if let Some(clients) = var("MCP_AUTH_CLIENTS") {
            b = b.clients(
                clients
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            );
        }
        let service = var("MCP_AUTH_SCOPE_SERVICE").unwrap_or_else(|| b.service_scope.clone());
        let user = var("MCP_AUTH_SCOPE_USER").unwrap_or_else(|| b.user_scope.clone());
        b = b.scopes(service, user);
        if let Some(url) = var("MCP_AUTH_PUBLIC_URL") {
            b = b.public_url(url);
        }
        if let Some(path) = var("MCP_AUTH_RESOURCE_PATH") {
            b = b.resource_path(path);
        }
        if let Some(name) = var("MCP_AUTH_RESOURCE_NAME") {
            b = b.resource_name(name);
        }
        Ok(Some(b.build()))
    }

    /// Supplies the pool's signing keys instead of fetching them, for tests and for servers
    /// that can't reach Cognito's JWKS endpoint.
    pub async fn set_jwks(&self, jwks: &JwkSet) {
        let mut keys = self.0.keys.write().await;
        keys.by_kid = decoding_keys(jwks);
        keys.fetched = Some(Instant::now());
    }

    /// Requires a valid token on every route of `router`, and adds the public metadata routes.
    ///
    /// Pass only the MCP routes; merge public routes (health checks, pages) after.
    pub fn protect<S>(&self, router: Router<S>) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        router
            .layer(axum::middleware::from_fn_with_state(
                self.clone(),
                require_token,
            ))
            .merge(self.metadata_router())
    }

    /// The public metadata routes on their own, for servers that check tokens elsewhere.
    pub fn metadata_router<S>(&self) -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        let suffixed = format!(
            "/.well-known/oauth-protected-resource{}",
            self.0.config.resource_path
        );
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                get(protected_resource),
            )
            .route(&suffixed, get(protected_resource))
            .route(
                "/.well-known/oauth-authorization-server",
                get(authorization_server),
            )
            .with_state(self.clone())
    }

    /// RFC 9728 protected-resource metadata, for a request with these headers.
    pub fn protected_resource_metadata(&self, headers: &HeaderMap) -> serde_json::Value {
        let c = &self.0.config;
        let base = self.base_url(headers);
        let mut doc = json!({
            "resource": format!("{base}{}", c.resource_path),
            "authorization_servers": [base],
            "scopes_supported": [c.service_scope, c.user_scope],
            "bearer_methods_supported": ["header"],
        });
        if let Some(name) = &c.resource_name {
            doc["resource_name"] = json!(name);
        }
        if let Some(url) = &c.resource_documentation {
            doc["resource_documentation"] = json!(url);
        }
        doc
    }

    /// RFC 8414 authorization-server metadata. The issuer is this server's base URL, matching
    /// the protected-resource metadata; the endpoints are Cognito's.
    pub fn authorization_server_metadata(&self, headers: &HeaderMap) -> serde_json::Value {
        let c = &self.0.config;
        let d = &c.domain;
        json!({
            "issuer": self.base_url(headers),
            "authorization_endpoint": format!("{d}/oauth2/authorize"),
            "token_endpoint": format!("{d}/oauth2/token"),
            "revocation_endpoint": format!("{d}/oauth2/revoke"),
            "userinfo_endpoint": format!("{d}/oauth2/userInfo"),
            "jwks_uri": self.0.jwks_url,
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
            "scopes_supported": [c.service_scope, c.user_scope],
        })
    }

    fn base_url(&self, headers: &HeaderMap) -> String {
        if let Some(url) = &self.0.config.public_url {
            return url.clone();
        }
        let header = |name: &str| headers.get(name).and_then(|h| h.to_str().ok());
        let host = header(header::HOST.as_str()).unwrap_or("localhost");
        let local =
            host.starts_with("localhost") || host.starts_with("127.") || host.starts_with("[::1]");
        let proto = header("x-forwarded-proto").unwrap_or(if local { "http" } else { "https" });
        format!("{proto}://{host}")
    }

    fn has_scope(granted: &str, wanted: &str) -> bool {
        granted
            .split_whitespace()
            .any(|s| s == wanted || s.rsplit('/').next() == Some(wanted))
    }

    /// The signing key for `kid`, refetching the JWKS at most once per `jwks_retry`.
    async fn key(&self, kid: &str) -> Option<DecodingKey> {
        {
            let keys = self.0.keys.read().await;
            if let Some(k) = keys.by_kid.get(kid) {
                return Some(k.clone());
            }
            if keys
                .fetched
                .is_some_and(|t| t.elapsed() < self.0.config.jwks_retry)
            {
                return None;
            }
        }
        let url = &self.0.jwks_url;
        let fetched = async {
            self.0
                .http
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .json::<JwkSet>()
                .await
        }
        .await;
        let mut keys = self.0.keys.write().await;
        keys.fetched = Some(Instant::now());
        match fetched {
            Ok(jwks) => keys.by_kid = decoding_keys(&jwks),
            Err(e) => tracing::warn!(%url, error = %e, "fetching signing keys"),
        }
        keys.by_kid.get(kid).cloned()
    }

    async fn verify(&self, token: &str) -> Result<Claims, &'static str> {
        let c = &self.0.config;
        let head = jsonwebtoken::decode_header(token).map_err(|_| "malformed token")?;
        if head.alg != Algorithm::RS256 {
            return Err("unexpected signing algorithm");
        }
        let kid = head.kid.ok_or("token has no key id")?;
        let key = self.key(&kid).await.ok_or("unknown signing key")?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&c.issuer]);
        // Cognito access tokens carry client_id rather than aud.
        validation.validate_aud = false;
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|_| "invalid or expired token")?
            .claims;
        if claims.token_use != "access" {
            return Err("not an access token");
        }
        if !c.clients.is_empty() && !c.clients.contains(&claims.client_id) {
            return Err("token issued to an unknown client");
        }
        Ok(claims)
    }

    /// Which scope a JSON-RPC body needs: the user scope if any message in it is a user method.
    fn scope_for(&self, body: &[u8]) -> &str {
        let c = &self.0.config;
        let is_user = |v: &serde_json::Value| {
            v.get("method")
                .and_then(|m| m.as_str())
                .is_some_and(|m| c.user_methods.iter().any(|u| u == m))
        };
        let user = match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(serde_json::Value::Array(batch)) => batch.iter().any(is_user),
            Ok(v) => is_user(&v),
            Err(_) => false,
        };
        if user {
            &c.user_scope
        } else {
            &c.service_scope
        }
    }

    fn challenge(
        &self,
        headers: &HeaderMap,
        status: StatusCode,
        error: Option<(&str, &str)>,
        scope: Option<&str>,
    ) -> Response {
        let metadata = format!(
            "{}/.well-known/oauth-protected-resource{}",
            self.base_url(headers),
            self.0.config.resource_path
        );
        let mut value = format!("Bearer resource_metadata=\"{metadata}\"");
        if let Some(scope) = scope {
            value.push_str(&format!(", scope=\"{scope}\""));
        }
        if let Some((code, description)) = error {
            value.push_str(&format!(
                ", error=\"{code}\", error_description=\"{description}\""
            ));
        }
        let (code, message) = error.unwrap_or(("unauthorized", "sign in required"));
        let mut response = (
            status,
            Json(json!({ "error": code, "error_description": message })),
        )
            .into_response();
        if let Ok(v) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(header::WWW_AUTHENTICATE, v);
        }
        response
    }
}

fn decoding_keys(jwks: &JwkSet) -> HashMap<String, DecodingKey> {
    jwks.keys
        .iter()
        .filter_map(|k| Some((k.common.key_id.clone()?, DecodingKey::from_jwk(k).ok()?)))
        .collect()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

async fn require_token(State(auth): State<CognitoAuth>, req: Request, next: Next) -> Response {
    let c = &auth.0.config;
    let (mut parts, body) = req.into_parts();
    // A 401 names no scope: clients then ask for what the metadata lists, which suits both a
    // service client and a user client. A 403 names the scope to step up to.
    let Some(token) = bearer(&parts.headers) else {
        return auth.challenge(&parts.headers, StatusCode::UNAUTHORIZED, None, None);
    };
    let claims = match auth.verify(token).await {
        Ok(claims) => claims,
        Err(why) => {
            return auth.challenge(
                &parts.headers,
                StatusCode::UNAUTHORIZED,
                Some(("invalid_token", why)),
                None,
            )
        }
    };
    let Ok(bytes) = to_bytes(body, c.max_body).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response();
    };
    let wanted = auth.scope_for(&bytes);
    // A user token may also discover.
    let is_user = CognitoAuth::has_scope(&claims.scope, &c.user_scope);
    if !(is_user || CognitoAuth::has_scope(&claims.scope, wanted)) {
        return auth.challenge(
            &parts.headers,
            StatusCode::FORBIDDEN,
            Some(("insufficient_scope", "this call needs a signed-in user")),
            Some(wanted),
        );
    }
    parts.extensions.insert(Caller {
        subject: claims.sub,
        client_id: claims.client_id,
        username: claims.username,
        scopes: claims.scope.split_whitespace().map(String::from).collect(),
        is_user,
    });
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

async fn protected_resource(State(auth): State<CognitoAuth>, headers: HeaderMap) -> Response {
    Json(auth.protected_resource_metadata(&headers)).into_response()
}

async fn authorization_server(State(auth): State<CognitoAuth>, headers: HeaderMap) -> Response {
    Json(auth.authorization_server_metadata(&headers)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "BEARER abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer ".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn prefixed_scopes_match_bare_names() {
        assert!(CognitoAuth::has_scope(
            "openid myapp/mcp:tools",
            "mcp:tools"
        ));
        assert!(CognitoAuth::has_scope("myapp/mcp:tools", "myapp/mcp:tools"));
        assert!(!CognitoAuth::has_scope("myapp/mcp:service", "mcp:tools"));
        assert!(!CognitoAuth::has_scope("mcp:toolsx", "mcp:tools"));
    }

    #[test]
    fn batches_with_a_tool_call_need_the_user_scope() {
        let auth = CognitoAuth::builder("https://i", "https://d").build();
        let call = br#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}]"#;
        assert_eq!(auth.scope_for(call), "mcp:tools");
        assert_eq!(auth.scope_for(br#"{"method":"tools/list"}"#), "mcp:service");
        assert_eq!(auth.scope_for(b""), "mcp:service");
        let custom = CognitoAuth::builder("https://i", "https://d")
            .user_methods(["tools/call", "resources/read"])
            .build();
        assert_eq!(
            custom.scope_for(br#"{"method":"resources/read"}"#),
            "mcp:tools"
        );
    }

    #[test]
    fn base_url_follows_forwarding_headers() {
        let auth = CognitoAuth::builder("https://i", "https://d").build();
        let mut h = HeaderMap::new();
        assert_eq!(auth.base_url(&h), "http://localhost");
        h.insert(
            header::HOST,
            "abc.lambda-url.us-east-1.on.aws".parse().unwrap(),
        );
        assert_eq!(auth.base_url(&h), "https://abc.lambda-url.us-east-1.on.aws");
        h.insert("x-forwarded-proto", "http".parse().unwrap());
        assert_eq!(auth.base_url(&h), "http://abc.lambda-url.us-east-1.on.aws");
        let fixed = CognitoAuth::builder("https://i", "https://d")
            .public_url("https://fixed.example/")
            .build();
        assert_eq!(fixed.base_url(&h), "https://fixed.example");
    }
}
