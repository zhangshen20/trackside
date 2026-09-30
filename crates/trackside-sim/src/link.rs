//! Account linking, the way Alexa+ links a user to an add-on: the browser signs in on
//! Cognito's page (authorization code + PKCE, S256), and the simulator's server exchanges the
//! code with the confidential client's secret. The resulting tokens live in HttpOnly cookies
//! scoped to `/sim`, and every MCP tool call carries the user's access token.

use anyhow::{bail, Context, Result};
use axum::http::{header, HeaderMap, HeaderValue};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const SCOPES: &str = "openid email trackside/mcp:service trackside/mcp:tools";

const ACCESS: &str = "ts_at";
const REFRESH: &str = "ts_rt";
const WHO: &str = "ts_who";
const PKCE: &str = "ts_pkce";

pub struct Link {
    /// Cognito's OAuth domain, `https://<prefix>.auth.<region>.amazoncognito.com`.
    pub domain: String,
    pub client_id: String,
    pub client_secret: String,
    /// The public base URL, when the Host header isn't it.
    pub public_url: Option<String>,
    pub http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
}

/// The tokens the browser holds, read from its cookies.
#[derive(Debug, Default)]
pub struct Session {
    pub access: Option<String>,
    pub refresh: Option<String>,
    pub who: Option<String>,
    pkce: Option<String>,
}

impl Session {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut s = Session::default();
        for value in headers.get_all(header::COOKIE) {
            let Ok(value) = value.to_str() else { continue };
            for pair in value.split(';') {
                let Some((k, v)) = pair.trim().split_once('=') else {
                    continue;
                };
                let v = Some(v.to_string()).filter(|v| !v.is_empty());
                match k {
                    ACCESS => s.access = v,
                    REFRESH => s.refresh = v,
                    WHO => s.who = v,
                    PKCE => s.pkce = v,
                    _ => {}
                }
            }
        }
        s
    }
}

/// `https://host` from the request, or `http://` for a local run.
pub fn base_url(headers: &HeaderMap, configured: Option<&str>) -> String {
    if let Some(url) = configured {
        return url.trim_end_matches('/').to_string();
    }
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let local = host.starts_with("localhost") || host.starts_with("127.0.0.1");
    format!("{}://{host}", if local { "http" } else { "https" })
}

fn cookie(name: &str, value: &str, max_age: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/sim; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie values are URL-safe")
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The email in an ID token, for "Linked as …". The token came straight from Cognito over
/// TLS in the code exchange, so it is read, not verified.
fn email_of(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims
        .get("email")?
        .as_str()
        .map(|e| e.replace([';', ',', ' '], ""))
}

impl Link {
    fn redirect_uri(&self, base: &str) -> String {
        format!("{base}/sim/callback")
    }

    /// Where to send the browser to sign in, and the cookie that remembers the PKCE verifier.
    pub fn start(&self, base: &str) -> (String, HeaderValue) {
        let verifier = random_token();
        let state = random_token();
        let url = format!(
            "{}/oauth2/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            self.domain,
            enc(&self.client_id),
            enc(&self.redirect_uri(base)),
            enc(SCOPES),
            state,
            challenge(&verifier),
        );
        (
            url,
            cookie(
                PKCE,
                &format!("{state}.{verifier}"),
                600,
                base.starts_with("https"),
            ),
        )
    }

    /// Swaps the code for tokens; returns the cookies to set.
    pub async fn finish(
        &self,
        base: &str,
        session: &Session,
        code: &str,
        state: &str,
    ) -> Result<Vec<HeaderValue>> {
        let (want_state, verifier) = session
            .pkce
            .as_deref()
            .and_then(|p| p.split_once('.'))
            .context("the sign-in expired; start again")?;
        if want_state != state {
            bail!("the sign-in state didn't match; start again");
        }
        let tokens = self
            .token(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect_uri(base)),
                ("code_verifier", verifier),
            ])
            .await?;
        let secure = base.starts_with("https");
        let mut cookies = self.token_cookies(&tokens, secure);
        cookies.push(cookie(PKCE, "", 0, secure));
        Ok(cookies)
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens> {
        self.token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    pub fn token_cookies(&self, tokens: &Tokens, secure: bool) -> Vec<HeaderValue> {
        let mut out = vec![cookie(ACCESS, &tokens.access_token, 3600, secure)];
        if let Some(rt) = &tokens.refresh_token {
            out.push(cookie(REFRESH, rt, 30 * 24 * 3600, secure));
        }
        if let Some(email) = tokens.id_token.as_deref().and_then(email_of) {
            out.push(cookie(WHO, &email, 30 * 24 * 3600, secure));
        }
        out
    }

    pub fn clear_cookies(secure: bool) -> Vec<HeaderValue> {
        [ACCESS, REFRESH, WHO, PKCE]
            .iter()
            .map(|n| cookie(n, "", 0, secure))
            .collect()
    }

    /// Unlinking revokes the refresh token, so the link can't be reused.
    pub async fn revoke(&self, refresh_token: &str) {
        let _ = self
            .http
            .post(format!("{}/oauth2/revoke", self.domain))
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", refresh_token)])
            .send()
            .await;
    }

    async fn token(&self, form: &[(&str, &str)]) -> Result<Tokens> {
        let resp = self
            .http
            .post(format!("{}/oauth2/token", self.domain))
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(form)
            .send()
            .await
            .context("reaching Cognito's token endpoint")?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!(
                "Cognito refused the token request ({status}): {}",
                body.chars().take(200).collect::<String>()
            );
        }
        Ok(serde_json::from_str(&body)?)
    }
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        // Appendix B of RFC 7636.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn reads_its_cookies() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("other=1; ts_at=abc.def; ts_who=fan@example.com; ts_rt="),
        );
        let s = Session::from_headers(&h);
        assert_eq!(s.access.as_deref(), Some("abc.def"));
        assert_eq!(s.who.as_deref(), Some("fan@example.com"));
        assert!(s.refresh.is_none());
    }

    #[test]
    fn sign_in_url_asks_for_s256() {
        let link = Link {
            domain: "https://auth.example".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            public_url: None,
            http: reqwest::Client::new(),
        };
        let (url, cookie) = link.start("https://mcp.example");
        assert!(url.starts_with(
            "https://auth.example/oauth2/authorize?response_type=code&client_id=client"
        ));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fmcp.example%2Fsim%2Fcallback"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(
            "scope=openid%20email%20trackside%2Fmcp%3Aservice%20trackside%2Fmcp%3Atools"
        ));
        let c = cookie.to_str().unwrap();
        assert!(c.starts_with("ts_pkce=") && c.contains("HttpOnly") && c.contains("Secure"));
    }

    #[test]
    fn local_runs_use_http() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8001"));
        assert_eq!(base_url(&h, None), "http://127.0.0.1:8001");
        h.insert(
            header::HOST,
            HeaderValue::from_static("mcp.racingaidataset.com.au"),
        );
        assert_eq!(base_url(&h, None), "https://mcp.racingaidataset.com.au");
    }
}
