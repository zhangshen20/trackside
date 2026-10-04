//! End-to-end checks of the HTTP surface: OAuth challenges, metadata, scopes, and the spoken
//! answers over the demo fixture.

use std::sync::{Arc, OnceLock};

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use base64::Engine;
use jsonwebtoken::{jwk::JwkSet, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::auth::Auth;
use crate::build_app;
use crate::memory::{InMemory, Memory, Profile};
use trackside_core::FixtureStore;

const ISSUER: &str = "https://cognito-idp.ap-southeast-2.amazonaws.com/ap-southeast-2_test";

/// A throwaway RSA key made per test run, and its JWKS.
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

fn token(scope: &str, client: &str, sub: &str) -> String {
    let mut head = Header::new(jsonwebtoken::Algorithm::RS256);
    head.kid = Some("test".into());
    let exp = chrono::Utc::now().timestamp() + 600;
    let claims = json!({ "iss": ISSUER, "sub": sub, "token_use": "access", "client_id": client, "scope": scope, "exp": exp });
    jsonwebtoken::encode(&head, &claims, &key().0).unwrap()
}

async fn app(with_auth: bool) -> axum::Router {
    app_with_memory(with_auth, Arc::new(InMemory::default())).await
}

async fn app_with_memory(with_auth: bool, memory: Arc<dyn Memory>) -> axum::Router {
    let store = Arc::new(
        FixtureStore::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/demo.json"
        ))
        .unwrap(),
    );
    let auth = if with_auth {
        let auth = Arc::new(Auth::new(
            ISSUER.into(),
            "https://trackside-test.auth.ap-southeast-2.amazoncognito.com".into(),
            vec!["alexa".into(), "service".into()],
            "mcp:service".into(),
            "mcp:tools".into(),
            None,
        ));
        auth.set_keys(key().1.clone()).await;
        Some(auth)
    } else {
        None
    };
    // As on Lambda: stateless, and no localhost-only Host check.
    build_app(store, memory, None, auth, true, true, Default::default())
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "abc.execute-api.ap-southeast-2.amazonaws.com")
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

fn spoken(v: &Value) -> String {
    v["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn no_token_gets_401_pointing_at_metadata() {
    let app = app(true).await;
    let (status, headers, _) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let challenge = headers[header::WWW_AUTHENTICATE].to_str().unwrap();
    assert!(challenge.contains("resource_metadata=\"https://abc.execute-api.ap-southeast-2.amazonaws.com/.well-known/oauth-protected-resource\""), "{challenge}");

    let (status, _, _) = send(
        &app,
        "POST",
        "/mcp",
        Some("not-a-jwt"),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &app,
        "POST",
        "/mcp",
        Some(&token("trackside/mcp:tools", "stranger", "u1")),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "unknown client");
}

#[tokio::test]
async fn metadata_is_public_and_lists_s256() {
    let app = app(true).await;
    let (status, _, prm) = send(
        &app,
        "GET",
        "/.well-known/oauth-protected-resource",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        prm["resource"],
        "https://abc.execute-api.ap-southeast-2.amazonaws.com/mcp"
    );
    let base = prm["authorization_servers"][0].as_str().unwrap();
    assert_eq!(base, "https://abc.execute-api.ap-southeast-2.amazonaws.com");
    let (status, _, asm) = send(
        &app,
        "GET",
        "/.well-known/oauth-authorization-server",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(asm["issuer"], base);
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));
    assert!(asm["authorization_endpoint"]
        .as_str()
        .unwrap()
        .ends_with("amazoncognito.com/oauth2/authorize"));
}

#[tokio::test]
async fn service_token_discovers_but_cannot_call_tools() {
    let app = app(true).await;
    let service = token("trackside/mcp:service", "service", "service");
    let (status, _, init) = send(&app, "POST", "/mcp", Some(&service), Some(rpc("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(init["result"]["serverInfo"]["name"], "trackside");
    let (status, _, list) = send(
        &app,
        "POST",
        "/mcp",
        Some(&service),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 12);
    let (status, headers, _) = send(
        &app,
        "POST",
        "/mcp",
        Some(&service),
        Some(call("carnival_guide", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(headers[header::WWW_AUTHENTICATE]
        .to_str()
        .unwrap()
        .contains("insufficient_scope"));
}

#[tokio::test]
async fn user_token_calls_tools_and_keeps_its_own_stable() {
    let app = app(true).await;
    let ann = token(
        "openid trackside/mcp:service trackside/mcp:tools",
        "alexa",
        "ann",
    );
    let bob = token("trackside/mcp:tools", "alexa", "bob");
    let (status, _, list) = send(
        &app,
        "POST",
        "/mcp",
        Some(&bob),
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a user token may also discover: {list}"
    );

    let (_, _, followed) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(call("follow_horse", json!({"horse": "sample stayer"}))),
    )
    .await;
    assert_eq!(
        followed["result"]["structuredContent"]["stable"],
        json!(["Sample Stayer"])
    );
    let (_, _, unknown) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(call("follow_horse", json!({"horse": "Not A Real Horse"}))),
    )
    .await;
    assert_eq!(unknown["result"]["structuredContent"]["found"], false);
    let (_, _, bobs) = send(
        &app,
        "POST",
        "/mcp",
        Some(&bob),
        Some(call("my_stable", json!({"date": "2026-10-17"}))),
    )
    .await;
    assert!(
        spoken(&bobs).starts_with("You aren't following"),
        "{}",
        spoken(&bobs)
    );
    let (_, _, anns) = send(
        &app,
        "POST",
        "/mcp",
        Some(&ann),
        Some(call("my_stable", json!({"date": "2026-10-17"}))),
    )
    .await;
    assert!(
        spoken(&anns).starts_with("Sample Stayer runs in race 8 at Caulfield"),
        "a first look at a future card has nothing to catch up on: {}",
        spoken(&anns)
    );
}

#[tokio::test]
async fn open_server_answers_without_tokens() {
    let app = app(false).await;
    let (status, _, list) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 12);
    let (status, _, _) = send(
        &app,
        "GET",
        "/.well-known/oauth-protected-resource",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn spoken_answers_have_no_gaps() {
    let app = app(false).await;
    let (_, _, card) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "get_race_card",
            json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"}),
        )),
    )
    .await;
    let text = spoken(&card);
    assert!(text.starts_with("Race 8 at Caulfield is the Caulfield Cup, a Group 1 race over 2400 metres, worth $5 million."), "{text}");
    assert!(!text.contains("  "), "{text}");
    let (_, _, form) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("horse_form", json!({"horse": "Sample Stayer"}))),
    )
    .await;
    let text = spoken(&form);
    assert!(!text.contains("  ") && !text.contains("0 from 0"), "{text}");
}

#[tokio::test]
async fn stable_reports_results_for_races_already_run() {
    let app = app(false).await;
    send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Demo Miler"}))),
    )
    .await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({"date": "2026-09-26"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("Demo Miler ran 2nd in race 7 at Flemington"),
        "{text}"
    );
}

#[tokio::test]
async fn missing_result_says_when_there_was_no_meeting() {
    let app = app(false).await;
    let (_, _, out) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "race_result",
            json!({"venue": "Flemington", "race_number": 6, "date": "2026-10-17"}),
        )),
    )
    .await;
    let text = spoken(&out);
    assert!(
        text.starts_with("There was no racing at Flemington on Saturday 17 October."),
        "{text}"
    );
    assert!(text.contains("Caulfield"), "{text}");
}

#[tokio::test]
async fn stable_catches_up_on_runs_since_the_last_check() {
    let memory = Arc::new(InMemory::default());
    let profile = Profile {
        horses: vec!["Demo Miler".into(), "Sample Stayer".into()],
        last_checked: Some("2026-09-20".parse().unwrap()),
        ..Default::default()
    };
    memory.save("local", &profile).await.unwrap();
    let app = app_with_memory(false, memory.clone()).await;
    let ask = || call("my_stable", json!({"date": "2026-10-04"}));
    let (_, _, v) = send(&app, "POST", "/mcp", None, Some(ask())).await;
    let text = spoken(&v);
    assert!(
        text.starts_with("Since you last checked on Sunday 20 September: Demo Miler ran 2nd of 12 at Flemington on Saturday 26 September; Sample Stayer won at Flemington on Saturday 26 September."),
        "{text}"
    );
    assert!(!text.contains("isn't engaged"), "{text}");
    assert_eq!(v["result"]["structuredContent"]["catch_up"][0]["finish"], 2);
    // Heard once, the catch-up isn't repeated.
    let (_, _, v) = send(&app, "POST", "/mcp", None, Some(ask())).await;
    let text = spoken(&v);
    assert!(!text.contains("Since you last checked"), "{text}");
    assert!(
        text.contains(
            "Demo Miler isn't engaged on 4 Oct; last start it ran 2nd at Flemington on 26 Sep"
        ),
        "{text}"
    );
    assert_eq!(
        memory.load("local").await.unwrap().last_checked,
        Some("2026-10-04".parse().unwrap())
    );
}

#[tokio::test]
async fn listeners_can_unfollow_set_a_home_state_and_be_forgotten() {
    let app = app(false).await;
    for horse in ["Demo Miler", "Sample Stayer"] {
        send(
            &app,
            "POST",
            "/mcp",
            None,
            Some(call("follow_horse", json!({"horse": horse}))),
        )
        .await;
    }
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("unfollow_horse", json!({"horse": "demo miler"}))),
    )
    .await;
    assert_eq!(
        spoken(&v),
        "Stopped following Demo Miler. You follow 1 horse."
    );
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("unfollow_horse", json!({"horse": "Phar Lap"}))),
    )
    .await;
    assert_eq!(
        spoken(&v),
        "You weren't following Phar Lap. You follow Sample Stayer."
    );

    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("set_home_state", json!({"state": "nsw"}))),
    )
    .await;
    assert_eq!(v["result"]["structuredContent"]["home_state"], "NSW");
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("list_meetings", json!({"date": "2026-09-26"}))),
    )
    .await;
    assert!(
        spoken(&v).contains("There's no racing in NSW. Flemington in VIC"),
        "{}",
        spoken(&v)
    );
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("set_home_state", json!({"state": "Narnia"}))),
    )
    .await;
    assert!(
        v.get("error").is_some() || v["result"]["isError"] == true,
        "{v}"
    );

    send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("forget_me", json!({}))),
    )
    .await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({}))),
    )
    .await;
    assert!(
        spoken(&v).starts_with("You aren't following"),
        "{}",
        spoken(&v)
    );
}
