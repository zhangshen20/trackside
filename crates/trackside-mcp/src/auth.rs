//! OAuth 2.1 resource-server checks for the MCP endpoint, in the shape Alexa+ expects.
//!
//! Alexa+ uses two tiers of token, both issued by an Amazon Cognito user pool:
//!
//! - `client_credentials` with the service scope, for discovery (`initialize`, `tools/list`,
//!   `ping`, notifications) without a user;
//! - authorization code + PKCE (S256) with the tools scope, for tool calls made for a
//!   signed-in user.
//!
//! Unauthenticated requests get `401` with a `WWW-Authenticate` header pointing at this
//! server's protected-resource metadata (RFC 9728). Cognito publishes OpenID metadata only, so
//! the server also publishes RFC 8414 authorization-server metadata that points at Cognito's
//! authorize and token endpoints and lists S256.
//!
//! Configuration (all from the environment; auth is off when `TRACKSIDE_AUTH_ISSUER` is unset):
//!
//! - `TRACKSIDE_AUTH_ISSUER`: the user pool issuer, `https://cognito-idp.<region>.amazonaws.com/<pool id>`
//! - `TRACKSIDE_AUTH_DOMAIN`: the pool's OAuth domain, `https://<prefix>.auth.<region>.amazoncognito.com`
//! - `TRACKSIDE_AUTH_CLIENTS`: comma-separated app client ids whose tokens are accepted
//! - `TRACKSIDE_SCOPE_SERVICE`, `TRACKSIDE_SCOPE_TOOLS`: scope names, default `mcp:service`
//!   and `mcp:tools`. Cognito prefixes custom scopes with the resource server
//!   (`trackside/mcp:tools`); either form matches.
//! - `TRACKSIDE_PUBLIC_URL`: the public base URL, when the Host header isn't it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

/// Who is calling, attached to the request for the tools to read.
#[derive(Clone, Debug)]
pub struct Caller {
    /// The token's subject: a user id for signed-in users, the client id for service tokens.
    pub subject: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Service,
    Tools,
}

pub struct Auth {
    issuer: String,
    domain: String,
    clients: Vec<String>,
    service_scope: String,
    tools_scope: String,
    public_url: Option<String>,
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
    scope: String,
}

impl Auth {
    /// Reads the configuration; `None` when `TRACKSIDE_AUTH_ISSUER` is unset.
    pub fn from_env() -> anyhow::Result<Option<Arc<Self>>> {
        let Ok(issuer) = std::env::var("TRACKSIDE_AUTH_ISSUER") else {
            return Ok(None);
        };
        let domain = std::env::var("TRACKSIDE_AUTH_DOMAIN").map_err(|_| {
            anyhow::anyhow!("TRACKSIDE_AUTH_DOMAIN is required with TRACKSIDE_AUTH_ISSUER")
        })?;
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        Ok(Some(Arc::new(Self::new(
            issuer,
            domain,
            env("TRACKSIDE_AUTH_CLIENTS", "")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            env("TRACKSIDE_SCOPE_SERVICE", "mcp:service"),
            env("TRACKSIDE_SCOPE_TOOLS", "mcp:tools"),
            std::env::var("TRACKSIDE_PUBLIC_URL").ok(),
        ))))
    }

    pub fn new(
        issuer: String,
        domain: String,
        clients: Vec<String>,
        service_scope: String,
        tools_scope: String,
        public_url: Option<String>,
    ) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            domain: domain.trim_end_matches('/').to_string(),
            clients,
            service_scope,
            tools_scope,
            public_url: public_url.map(|u| u.trim_end_matches('/').to_string()),
            keys: Default::default(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
        }
    }

    /// Preload signing keys, for tests and for servers without network access to Cognito.
    #[cfg(test)]
    pub async fn set_keys(&self, jwks: JwkSet) {
        let mut keys = self.keys.write().await;
        keys.by_kid = decoding_keys(&jwks);
        keys.fetched = Some(Instant::now());
    }

    fn base_url(&self, headers: &HeaderMap) -> String {
        if let Some(url) = &self.public_url {
            return url.clone();
        }
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("localhost");
        let local = host.starts_with("localhost") || host.starts_with("127.");
        let proto = headers
            .get("x-forwarded-proto")
            .and_then(|h| h.to_str().ok())
            .unwrap_or(if local { "http" } else { "https" });
        format!("{proto}://{host}")
    }

    fn scopes(&self) -> [&str; 2] {
        [&self.service_scope, &self.tools_scope]
    }

    fn has_scope(&self, granted: &str, wanted: &str) -> bool {
        granted
            .split_whitespace()
            .any(|s| s == wanted || s.rsplit('/').next() == Some(wanted))
    }

    /// The signing key for `kid`, refetching the pool's JWKS at most once a minute.
    async fn key(&self, kid: &str) -> Option<DecodingKey> {
        {
            let keys = self.keys.read().await;
            if let Some(k) = keys.by_kid.get(kid) {
                return Some(k.clone());
            }
            if keys
                .fetched
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            {
                return None;
            }
        }
        let url = format!("{}/.well-known/jwks.json", self.issuer);
        let fetched = async { self.http.get(&url).send().await?.json::<JwkSet>().await }.await;
        let mut keys = self.keys.write().await;
        keys.fetched = Some(Instant::now());
        match fetched {
            Ok(jwks) => keys.by_kid = decoding_keys(&jwks),
            Err(e) => tracing::warn!(%url, error = %e, "fetching signing keys"),
        }
        keys.by_kid.get(kid).cloned()
    }

    async fn verify(&self, token: &str) -> Result<Claims, &'static str> {
        let head = jsonwebtoken::decode_header(token).map_err(|_| "malformed token")?;
        let kid = head.kid.ok_or("token has no key id")?;
        let key = self.key(&kid).await.ok_or("unknown signing key")?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        // Cognito access tokens carry client_id rather than aud.
        validation.validate_aud = false;
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|_| "invalid or expired token")?
            .claims;
        if claims.token_use != "access" {
            return Err("not an access token");
        }
        if !self.clients.is_empty() && !self.clients.contains(&claims.client_id) {
            return Err("token issued to an unknown client");
        }
        Ok(claims)
    }

    fn challenge(
        &self,
        headers: &HeaderMap,
        status: StatusCode,
        error: Option<(&str, &str)>,
        scope: Option<&str>,
    ) -> Response {
        let metadata = format!(
            "{}/.well-known/oauth-protected-resource",
            self.base_url(headers)
        );
        let mut value = format!("Bearer resource_metadata=\"{metadata}\"");
        if let Some((code, description)) = error {
            value.push_str(&format!(
                ", error=\"{code}\", error_description=\"{description}\""
            ));
        }
        if let Some(scope) = scope {
            value.push_str(&format!(", scope=\"{scope}\""));
        }
        let message = error.map(|(_, d)| d).unwrap_or("sign in required");
        let mut response = (
            status,
            Json(json!({ "error": error.map(|(c, _)| c).unwrap_or("unauthorized"), "error_description": message })),
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

/// Tool calls act for a user and need the tools scope; everything else is discovery.
fn needs(body: &[u8]) -> Need {
    let calls_tool =
        |v: &serde_json::Value| v.get("method").and_then(|m| m.as_str()) == Some("tools/call");
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(serde_json::Value::Array(batch)) if batch.iter().any(calls_tool) => Need::Tools,
        Ok(v) if calls_tool(&v) => Need::Tools,
        _ => Need::Service,
    }
}

/// Middleware in front of `/mcp`.
pub async fn require_token(State(auth): State<Arc<Auth>>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return auth.challenge(&parts.headers, StatusCode::UNAUTHORIZED, None, None);
    };
    let claims = match auth.verify(token).await {
        Ok(c) => c,
        Err(why) => {
            return auth.challenge(
                &parts.headers,
                StatusCode::UNAUTHORIZED,
                Some(("invalid_token", why)),
                None,
            )
        }
    };
    let Ok(bytes) = to_bytes(body, 1 << 20).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response();
    };
    let wanted = match needs(&bytes) {
        Need::Tools => auth.tools_scope.clone(),
        // A user token with the tools scope may also discover.
        Need::Service if auth.has_scope(&claims.scope, &auth.tools_scope) => {
            auth.tools_scope.clone()
        }
        Need::Service => auth.service_scope.clone(),
    };
    if !auth.has_scope(&claims.scope, &wanted) {
        return auth.challenge(
            &parts.headers,
            StatusCode::FORBIDDEN,
            Some(("insufficient_scope", "this call needs a signed-in user")),
            Some(&wanted),
        );
    }
    parts.extensions.insert(Caller {
        subject: claims.sub,
    });
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// RFC 9728 protected-resource metadata.
pub async fn protected_resource(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    let base = auth.base_url(&headers);
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "scopes_supported": auth.scopes(),
        "bearer_methods_supported": ["header"],
        "resource_name": "Trackside",
        "resource_documentation": "https://github.com/zhangshen20/trackside",
    }))
    .into_response()
}

/// RFC 8414 authorization-server metadata. The issuer is this server; the endpoints are
/// Cognito's. Cognito's own discovery document is OpenID-only and doesn't list PKCE methods.
pub async fn authorization_server(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    let base = auth.base_url(&headers);
    let d = &auth.domain;
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{d}/oauth2/authorize"),
        "token_endpoint": format!("{d}/oauth2/token"),
        "revocation_endpoint": format!("{d}/oauth2/revoke"),
        "jwks_uri": format!("{}/.well-known/jwks.json", auth.issuer),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "scopes_supported": auth.scopes(),
    }))
    .into_response()
}
