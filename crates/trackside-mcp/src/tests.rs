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
use crate::clock::FixedClock;
use crate::memory::{InMemory, Memory, Profile};
use trackside_core::{Fixture, FixtureStore, RaceCard, SectionalHighlight};

/// Every test runs on Wednesday 14 October 2026, Melbourne time: three days before the
/// fixture's Caulfield Cup card and after its Flemington results, so "today", "since you
/// last checked" and "runs on Saturday" come out the same whatever day the tests run.
const TEST_NOW: &str = "2026-10-13T22:00:00Z";

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
    app_at(with_auth, memory, TEST_NOW).await
}

/// The app on the demo fixture with its clock stopped at `now` (RFC 3339, UTC).
async fn app_at(with_auth: bool, memory: Arc<dyn Memory>, now: &str) -> axum::Router {
    app_with(with_auth, Arc::new(demo_store()), memory, now).await
}

/// The demo fixture the server runs on locally.
fn demo_store() -> FixtureStore {
    FixtureStore::load(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/demo.json"
    ))
    .unwrap()
}

/// Two meetings on one Saturday whose venues share a word: Warwick Farm in Sydney and
/// Warwick in Queensland. Race 1 at Warwick Farm is a field with no form to speak of.
fn two_warwicks() -> FixtureStore {
    let fixture: Fixture = serde_json::from_value(json!({
        "meetings": [
            { "date": "2026-10-17", "state": "NSW", "venue": "Warwick Farm", "races": [
                { "race_number": 1, "name": "The Galaxy", "start_local": "12:30", "distance_m": 1100, "runners": [
                    { "number": 1, "horse": "First Timer", "jockey": "A. Rider", "trainer": "B. Yard", "barrier": 3, "last10": "" },
                    { "number": 2, "horse": "Trial Only", "jockey": "C. Hoop", "trainer": "D. Barn", "barrier": 5, "last10": "x" }
                ] }
            ] },
            { "date": "2026-10-17", "state": "QLD", "venue": "Warwick", "races": [
                { "race_number": 2, "name": "", "start_local": "13:05", "distance_m": 1200, "runners": [
                    { "number": 1, "horse": "Country Mile", "jockey": "E. Rider", "trainer": "F. Yard", "barrier": 1, "last10": "32" }
                ] }
            ] }
        ]
    }))
    .unwrap();
    FixtureStore::from_fixture(fixture)
}

async fn app_with(
    with_auth: bool,
    store: Arc<FixtureStore>,
    memory: Arc<dyn Memory>,
    now: &str,
) -> axum::Router {
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
    let clock = Arc::new(FixedClock(now.parse().unwrap()));
    // As on Lambda: stateless, and no localhost-only Host check.
    build_app(
        store,
        memory,
        None,
        clock,
        auth,
        true,
        true,
        Default::default(),
    )
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

/// Whether `text` has `word` as a whole word: "bet" in "a bet", not in "better" or "alphabet".
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 13);
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 13);
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

/// A last check is a whole day, and the race that day may have had no result yet when the
/// listener asked, so the catch-up starts from that day, not the day after.
#[tokio::test]
async fn stable_catches_up_on_a_run_from_the_day_it_last_checked() {
    let memory = Arc::new(InMemory::default());
    let profile = Profile {
        horses: vec!["Demo Miler".into()],
        last_checked: Some("2026-09-26".parse().unwrap()),
        ..Default::default()
    };
    memory.save("local", &profile).await.unwrap();
    let app = app_with_memory(false, memory).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({"date": "2026-09-27"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.starts_with("Since you last checked on Saturday 26 September: Demo Miler ran 2nd of 12 at Flemington on Saturday 26 September"),
        "{text}"
    );
    assert_eq!(v["result"]["structuredContent"]["catch_up"][0]["finish"], 2);
}

#[test]
fn a_race_name_that_starts_with_the_keeps_one_article() {
    let race = |name: &str| RaceCard {
        name: name.into(),
        distance_m: Some(1100),
        grade: "Group 1".into(),
        ..Default::default()
    };
    assert_eq!(
        crate::tools::named_race(&race("The Galaxy")),
        "The Galaxy, a Group 1 race over 1100 metres"
    );
    assert_eq!(
        crate::tools::named_race(&race("the galaxy")),
        "the galaxy, a Group 1 race over 1100 metres"
    );
    assert_eq!(
        crate::tools::named_race(&race("Caulfield Cup")),
        "the Caulfield Cup, a Group 1 race over 1100 metres"
    );
    // A name that merely begins with those letters still takes the article.
    assert_eq!(
        crate::tools::named_race(&race("Theodore Stakes")),
        "the Theodore Stakes, a Group 1 race over 1100 metres"
    );
}

#[tokio::test]
async fn a_venue_word_two_meetings_answer_to_is_asked_about() {
    let app = app_with(
        false,
        Arc::new(two_warwicks()),
        Arc::new(InMemory::default()),
        TEST_NOW,
    )
    .await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "get_race_card",
            json!({"venue": "Warwick", "race_number": 1, "date": "2026-10-17"}),
        )),
    )
    .await;
    assert_eq!(v["result"]["structuredContent"]["found"], false, "{v}");
    assert_eq!(
        v["result"]["structuredContent"]["did_you_mean"],
        json!(["Warwick Farm", "Warwick"])
    );
    assert!(
        spoken(&v).ends_with("Did you mean Warwick Farm or Warwick?"),
        "{}",
        spoken(&v)
    );
    // Said in full, Warwick Farm is meant, though its name contains the other venue's.
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "get_race_card",
            json!({"venue": "Warwick Farm", "race_number": 1, "date": "2026-10-17"}),
        )),
    )
    .await;
    assert!(
        spoken(&v).starts_with("Race 1 at Warwick Farm is The Galaxy, a race over 1100 metres. 2 runners, jumping at 12:30 pm."),
        "{}",
        spoken(&v)
    );
}

#[tokio::test]
async fn explanations_leave_out_form_nobody_has() {
    let app = app_with(
        false,
        Arc::new(two_warwicks()),
        Arc::new(InMemory::default()),
        TEST_NOW,
    )
    .await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "explain_race",
            json!({"venue": "Warwick Farm", "race_number": 1, "date": "2026-10-17"}),
        )),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.starts_with("The Galaxy: A race over 1100 metres. Track is not yet rated. Source:"),
        "{text}"
    );
    assert!(
        !text.contains("belongs to") && !text.contains("0 wins"),
        "{text}"
    );
    let structured = &v["result"]["structuredContent"];
    assert_eq!(
        structured["strongest_recent_form"],
        json!([]),
        "{structured}"
    );
    assert_eq!(structured["race"], "The Galaxy");
    assert!(structured.get("contenders").is_none(), "{structured}");
}

/// On the Wednesday before the Caulfield Cup, following a horse says when it runs next, and
/// the stable report names each horse's next run instead of only its last start.
#[tokio::test]
async fn the_stable_looks_ahead_to_each_horses_next_run() {
    let app = app(false).await;
    let (_, _, followed) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Sample Stayer"}))),
    )
    .await;
    let text = spoken(&followed);
    assert!(
        text.ends_with("Sample Stayer runs on Saturday in the Caulfield Cup, race 8 at Caulfield at 5 pm, barrier 4, with J. Example up; ask me after the race and I'll tell you how it went."),
        "{text}"
    );
    assert_eq!(
        followed["result"]["structuredContent"]["next"]["date"],
        "2026-10-17"
    );
    // A scratching is said as one, and nobody is asked to come back after the race.
    let (_, _, scratched) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Late Change"}))),
    )
    .await;
    let text = spoken(&scratched);
    assert!(
        text.ends_with("Late Change has been scratched from the Caulfield Cup, race 8 at Caulfield on Saturday."),
        "{text}"
    );
    // A horse with nothing in the fields keeps its last start.
    send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Spare Part"}))),
    )
    .await;
    let (_, _, report) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({}))),
    )
    .await;
    let text = spoken(&report);
    assert!(
        text.starts_with("Sample Stayer runs on Saturday in the Caulfield Cup, race 8 at Caulfield at 5 pm, barrier 4, with J. Example up. Late Change has been scratched from the Caulfield Cup, race 8 at Caulfield on Saturday. Spare Part isn't engaged on 14 Oct or in any field through 18 Oct; last start it ran 5th at Sandown on 3 Oct."),
        "{text}"
    );
    let upcoming = &report["result"]["structuredContent"]["upcoming"];
    assert_eq!(upcoming.as_array().map(Vec::len), Some(2), "{upcoming}");
    assert_eq!(upcoming[0]["horse"], "Sample Stayer");
    assert_eq!(upcoming[0]["race_number"], 8);
    assert_eq!(upcoming[1]["scratched"], true);
    // On the day itself the engagement is said as before, and the scratching too.
    let (_, _, day) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({"date": "2026-10-17"}))),
    )
    .await;
    let text = spoken(&day);
    assert!(
        text.contains("Late Change has been scratched from race 8 at Caulfield (Caulfield Cup)"),
        "{text}"
    );
    assert!(
        text.contains("Spare Part isn't engaged on 17 Oct or in any field through 21 Oct"),
        "{text}"
    );
}

/// A 5 pm Caulfield race on Cup day is 4 pm in Brisbane, 2 pm in Perth and 4:30 pm in
/// Adelaide once daylight saving has started; Sydney keeps Melbourne's clock.
#[tokio::test]
async fn listeners_hear_start_times_in_their_own_clock() {
    let app = app(false).await;
    let card = || {
        call(
            "get_race_card",
            json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"}),
        )
    };
    let home = |state: &str| call("set_home_state", json!({"state": state}));
    let (_, _, v) = send(&app, "POST", "/mcp", None, Some(card())).await;
    assert!(
        spoken(&v).contains("3 runners, jumping at 5 pm."),
        "{}",
        spoken(&v)
    );
    assert!(v["result"]["structuredContent"]["jump"]["home_time"].is_null());
    for (state, words, hhmm) in [
        ("QLD", "4 pm Queensland time, 5 pm at the track", "16:00"),
        ("WA", "2 pm Perth time, 5 pm at the track", "14:00"),
        ("SA", "4:30 pm Adelaide time, 5 pm at the track", "16:30"),
    ] {
        send(&app, "POST", "/mcp", None, Some(home(state))).await;
        let (_, _, v) = send(&app, "POST", "/mcp", None, Some(card())).await;
        let text = spoken(&v);
        assert!(
            text.contains(&format!("jumping at {words}.")),
            "{state}: {text}"
        );
        assert_eq!(v["result"]["structuredContent"]["jump"]["home_time"], hhmm);
        assert_eq!(
            v["result"]["structuredContent"]["jump"]["venue_time"],
            "17:00"
        );
    }
    send(&app, "POST", "/mcp", None, Some(home("NSW"))).await;
    let (_, _, v) = send(&app, "POST", "/mcp", None, Some(card())).await;
    assert!(spoken(&v).contains("jumping at 5 pm."), "{}", spoken(&v));
    assert!(v["result"]["structuredContent"]["jump"]["home_time"].is_null());

    // Meetings and the stable follow suit for a Queensland listener.
    send(&app, "POST", "/mcp", None, Some(home("QLD"))).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("list_meetings", json!({"date": "2026-10-17"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("Eagle Farm in QLD: 1 race, track Good 4, first race 12:10 pm.")
            && text.contains("Elsewhere: Caulfield."),
        "{text}"
    );
    let meetings = &v["result"]["structuredContent"]["meetings"];
    let caulfield = meetings
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["venue"] == "Caulfield")
        .unwrap();
    assert_eq!(caulfield["races"][0]["start_home"], "16:00");
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Sample Stayer"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("race 8 at Caulfield at 4 pm Queensland time, barrier 4"),
        "{text}"
    );
    assert_eq!(
        v["result"]["structuredContent"]["next"]["start_home"],
        "16:00"
    );
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("my_stable", json!({}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("at 4 pm Queensland time, barrier 4"),
        "{text}"
    );
    assert_eq!(
        v["result"]["structuredContent"]["upcoming"][0]["start_home"],
        "16:00"
    );
}

/// On the day, a race within three hours is placed against the clock.
#[tokio::test]
async fn todays_race_says_how_far_off_the_jump_is() {
    let card = || {
        call(
            "get_race_card",
            json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"}),
        )
    };
    // 3:35 pm in Brisbane on Cup day: the 5 pm Caulfield race is 25 minutes away.
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-17T05:35:00Z").await;
    send(
        &router,
        "POST",
        "/mcp",
        None,
        Some(call("set_home_state", json!({"state": "QLD"}))),
    )
    .await;
    let (_, _, v) = send(&router, "POST", "/mcp", None, Some(card())).await;
    let text = spoken(&v);
    assert!(
        text.contains(
            "jumping at 4 pm Queensland time, 5 pm at the track, due to jump in about 25 minutes."
        ),
        "{text}"
    );
    assert_eq!(
        v["result"]["structuredContent"]["jump"]["minutes_until"],
        25
    );
    // An hour after the jump.
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-17T07:00:00Z").await;
    let (_, _, v) = send(&router, "POST", "/mcp", None, Some(card())).await;
    let text = spoken(&v);
    assert!(
        text.contains("jumping at 5 pm, which jumped about an hour ago; ask me for the result."),
        "{text}"
    );
    // Earlier the same day, more than three hours out, nothing is said about the clock.
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-16T22:00:00Z").await;
    let (_, _, v) = send(&router, "POST", "/mcp", None, Some(card())).await;
    assert!(spoken(&v).contains("jumping at 5 pm."), "{}", spoken(&v));
    // The day before, nothing is said about the clock.
    let router = app(false).await;
    let (_, _, v) = send(&router, "POST", "/mcp", None, Some(card())).await;
    assert!(spoken(&v).contains("jumping at 5 pm."), "{}", spoken(&v));
}

#[tokio::test]
async fn an_empty_past_day_is_not_said_to_be_pending() {
    let app = app(false).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("list_meetings", json!({"date": "2026-10-05"}))),
    )
    .await;
    assert!(
        spoken(&v).starts_with("I don't have any meetings on file for Monday 5 October."),
        "{}",
        spoken(&v)
    );
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("list_meetings", json!({"date": "2026-10-20"}))),
    )
    .await;
    assert!(
        spoken(&v).starts_with("I don't have any meetings on Tuesday 20 October yet."),
        "{}",
        spoken(&v)
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

#[tokio::test]
async fn screen_tools_point_at_the_mcp_app() {
    let app = app(false).await;
    let init = rpc(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    let (_, _, v) = send(&app, "POST", "/mcp", None, Some(init)).await;
    let caps = &v["result"]["capabilities"];
    assert_eq!(
        caps["extensions"]["io.modelcontextprotocol/ui"]["mimeTypes"],
        json!(["text/html;profile=mcp-app"]),
        "{v}"
    );
    assert!(caps["resources"].is_object(), "{v}");

    let (_, _, list) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc("tools/list", json!({}))),
    )
    .await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let with_app: Vec<_> = tools
        .iter()
        .filter(|t| t["_meta"]["ui"]["resourceUri"] == "ui://trackside/race-card.html")
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "get_race_card",
        "race_result",
        "horse_form",
        "explain_race",
        "my_stable",
    ] {
        assert!(with_app.contains(&name), "{name} has no app: {with_app:?}");
    }
    // Every tool carries a title and behaviour hints, and its input schema uses only what
    // every host's model reads: no type arrays, formats, bounds or `$schema`.
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(t["title"].is_string(), "{name} has no title");
        assert!(
            t["annotations"]["readOnlyHint"].is_boolean(),
            "{name} has no annotations"
        );
        let schema = &t["inputSchema"];
        assert!(schema.get("$schema").is_none(), "{name}: {schema}");
        for (field, def) in schema["properties"].as_object().unwrap() {
            assert!(def["type"].is_string(), "{name}.{field}: {def}");
            assert!(
                def.get("format").is_none() && def.get("minimum").is_none(),
                "{name}.{field}: {def}"
            );
        }
    }
    assert_eq!(
        tools
            .iter()
            .filter(|t| t["annotations"]["readOnlyHint"] == true)
            .count(),
        8,
        "read-only tools"
    );

    let (_, _, res) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc("resources/list", json!({}))),
    )
    .await;
    assert_eq!(
        res["result"]["resources"][0]["uri"],
        "ui://trackside/race-card.html"
    );
    let (_, _, read) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(rpc(
            "resources/read",
            json!({"uri": "ui://trackside/race-card.html"}),
        )),
    )
    .await;
    let contents = &read["result"]["contents"][0];
    assert_eq!(contents["mimeType"], "text/html;profile=mcp-app");
    let html = contents["text"].as_str().unwrap();
    assert!(html.contains("ui/initialize") && html.contains("tools/call"));
    // The view draws what tools return: it must never reach the network itself, load anything
    // from outside, or show prices or bookmakers.
    let lower = html.to_lowercase();
    for banned in [
        "fetch(",
        "xmlhttprequest",
        "http://",
        "https://",
        "import(",
        "sendbeacon",
        "websocket",
        "eventsource",
        "<img",
        "<link",
        "betting",
        "wager",
        "bookmaker",
        "sportsbet",
        "ladbrokes",
        "bet365",
        "pointsbet",
    ] {
        assert!(!lower.contains(banned), "the app contains {banned}");
    }
    // Short words are checked whole, so "better" and "alphabet" don't trip them.
    for word in ["odds", "bet", "neds"] {
        assert!(!has_word(&lower, word), "the app mentions {word}");
    }
}

#[tokio::test]
async fn results_say_how_the_race_was_run() {
    let app = app(false).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "race_result",
            json!({"venue": "Flemington", "race_number": 7, "date": "2026-09-26"}),
        )),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("How it was run: Sample Stayer came from 5th at the 800 and ran its last 600 in 34.9 seconds, according to racing.com sectional timing. Demo Miler ran the fastest last 600, 34.6 seconds, from 9th at the 800 to finish 2nd, according to racing.com sectional timing."),
        "{text}"
    );
    assert_eq!(v["result"]["structuredContent"]["run"][2]["pos_800"], 1);
}

/// Two runs clocked at the same time are equal-fastest, whichever one the feed names, and
/// the sectional source is given with the times, not with a placegetter's position at the
/// 800, which is Racing Australia's.
#[test]
fn equal_last_600_times_are_equal_fastest() {
    use crate::tools::{how_it_was_run, RunLine};
    let line = |position, horse: &str, pos_800, last_600_s, story: Option<&str>| RunLine {
        position,
        horse: horse.into(),
        pos_800,
        pos_400: None,
        last_600_s: Some(last_600_s),
        story: story.map(Into::into),
    };
    let run = [
        line(1, "Sample Stayer", Some(2), 34.6, None),
        line(
            2,
            "Demo Miler",
            Some(9),
            34.6,
            Some("came from 9th at the 800"),
        ),
        line(
            3,
            "Placeholder Prince",
            Some(10),
            35.4,
            Some("came from 10th at the 800"),
        ),
    ];
    let fastest = SectionalHighlight {
        horse: "Demo Miler".into(),
        last_600_s: 34.6,
        source: "racing.com sectional timing".into(),
    };
    assert_eq!(
        how_it_was_run(&run, Some(&fastest)),
        " How it was run: Sample Stayer ran the equal-fastest last 600 in the race, 34.6 seconds, according to racing.com sectional timing. Demo Miler ran the equal-fastest last 600, 34.6 seconds, from 9th at the 800 to finish 2nd, according to racing.com sectional timing. Placeholder Prince came from 10th at the 800 to run 3rd."
    );
    // One clear fastest time is still just the fastest.
    let fastest = SectionalHighlight {
        last_600_s: 34.4,
        ..fastest
    };
    let mut run = run;
    run[1].last_600_s = Some(34.4);
    assert!(
        how_it_was_run(&run, Some(&fastest))
            .contains("Demo Miler ran the fastest last 600, 34.4 seconds, from 9th at the 800 to finish 2nd, according to"),
    );
}

#[tokio::test]
async fn form_and_explanations_say_where_horses_settle() {
    let app = app(false).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("horse_form", json!({"horse": "Demo Miler"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("In its races it usually settles back in the field and runs on."),
        "{text}"
    );
    assert!(
        text.contains("it ran 2nd of 12, 0.8 lengths off the winner, from 9th at the 800"),
        "{text}"
    );
    assert_eq!(
        v["result"]["structuredContent"]["run_style"]["style"],
        "back"
    );

    let race = json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"});
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("explain_race", race.clone())),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.contains("On past runs, Placeholder Prince usually leads, and Demo Miler usually settles back in the field."),
        "{text}"
    );
    assert!(
        !text.contains("1 wins") && !text.contains("ones to watch"),
        "{text}"
    );
    // Form is ranked by recent wins; the sentence is about runs that happened, never gaps.
    assert!(
        text.contains("The strongest recent form belongs to Sample Stayer (3 wins in its last 5 starts), Placeholder Prince (3 wins in its last 5 starts) and Demo Miler (1 win in its last 5 starts)."),
        "{text}"
    );
    assert!(
        !text.contains("0 wins in its last 0") && !text.contains("belongs to ."),
        "{text}"
    );
    let structured = &v["result"]["structuredContent"];
    assert_eq!(
        structured["strongest_recent_form"],
        json!(["Sample Stayer", "Placeholder Prince", "Demo Miler"]),
        "{structured}"
    );
    assert_eq!(structured["race"], "Caulfield Cup");
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("get_race_card", race)),
    )
    .await;
    assert_eq!(
        v["result"]["structuredContent"]["run_styles"]["Sample Stayer"],
        "midfield"
    );
}

#[tokio::test]
async fn names_are_found_the_way_they_sound() {
    let app = app(false).await;
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("horse_form", json!({"horse": "Sample Stair"}))),
    )
    .await;
    let text = spoken(&v);
    assert!(
        text.starts_with("Taking Sample Stair as Sample Stayer. Sample Stayer, trained by"),
        "{text}"
    );
    assert_eq!(v["result"]["structuredContent"]["heard_as"], "Sample Stair");

    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("follow_horse", json!({"horse": "Demo Myler"}))),
    )
    .await;
    assert!(
        spoken(&v).contains("Following Demo Miler."),
        "{}",
        spoken(&v)
    );
    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("unfollow_horse", json!({"horse": "demo mila"}))),
    )
    .await;
    assert!(
        spoken(&v).starts_with("Stopped following Demo Miler."),
        "{}",
        spoken(&v)
    );

    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "get_race_card",
            json!({"venue": "Cofield", "race_number": 8, "date": "2026-10-17"}),
        )),
    )
    .await;
    assert!(
        spoken(&v).starts_with("Race 8 at Caulfield is the Caulfield Cup"),
        "{}",
        spoken(&v)
    );

    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call(
            "jockey_or_trainer_stats",
            json!({"name": "Example", "role": "jockey"}),
        )),
    )
    .await;
    assert!(
        spoken(&v).starts_with("Taking Example as J. Example. Jockey J. Example: 1 start: 1 win"),
        "{}",
        spoken(&v)
    );

    let (_, _, v) = send(
        &app,
        "POST",
        "/mcp",
        None,
        Some(call("horse_form", json!({"horse": "Phar Lap"}))),
    )
    .await;
    assert_eq!(v["result"]["structuredContent"]["found"], false);
}

#[test]
fn start_times_are_said_the_way_a_person_says_them() {
    for (written, said) in [
        ("4:25PM", "4:25 pm"),
        ("5:00PM", "5 pm"),
        ("11:55AM", "11:55 am"),
        ("12:05PM", "12:05 pm"),
        ("15:40", "3:40 pm"),
        ("17:00", "5 pm"),
        ("12:00", "12 pm"),
        ("0:30", "12:30 am"),
        ("9:15", "9:15 am"),
        ("TBA", "TBA"),
        ("", ""),
    ] {
        assert_eq!(crate::tools::spoken_time(written), said, "{written}");
    }
}

// ---- telemetry ----

/// The demo app with metric lines going to `sink`, serving `store`.
fn app_metered(
    store: Arc<dyn trackside_core::Store>,
    sink: Arc<crate::telemetry::InMemory>,
    snapshot: crate::telemetry::SnapshotInfo,
) -> axum::Router {
    metered(store, Arc::new(InMemory::default()), sink, snapshot)
}

/// The same, remembering listeners in `memory`.
fn app_metered_with(
    store: Arc<dyn trackside_core::Store>,
    memory: Arc<dyn Memory>,
    sink: Arc<crate::telemetry::InMemory>,
) -> axum::Router {
    metered(store, memory, sink, Default::default())
}

fn metered(
    store: Arc<dyn trackside_core::Store>,
    memory: Arc<dyn Memory>,
    sink: Arc<crate::telemetry::InMemory>,
    snapshot: crate::telemetry::SnapshotInfo,
) -> axum::Router {
    crate::build_app_with(
        store,
        memory,
        None,
        Arc::new(FixedClock(TEST_NOW.parse().unwrap())),
        None,
        true,
        true,
        Default::default(),
        crate::Ops {
            metrics: Some(sink),
            snapshot,
            started: std::time::Instant::now(),
        },
    )
}

/// A tool call sent as Lambda delivers it, with the invocation's context in the request.
async fn send_on_lambda(app: &axum::Router, body: Value, request_id: &str) -> Value {
    let mut context = lambda_http::Context::default();
    context.request_id = request_id.into();
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "abc.execute-api.ap-southeast-2.amazonaws.com")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(context);
    let res = app.clone().oneshot(req).await.unwrap();
    let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn emf_lines(sink: &crate::telemetry::InMemory) -> Vec<Value> {
    sink.lines()
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn each_tool_call_writes_one_emf_line_with_its_outcome() {
    let sink = Arc::new(crate::telemetry::InMemory::default());
    let app = app_metered(Arc::new(demo_store()), sink.clone(), Default::default());

    send_on_lambda(
        &app,
        call("horse_form", json!({"horse": "Sample Stayer"})),
        "req-found",
    )
    .await;
    send_on_lambda(
        &app,
        call("horse_form", json!({"horse": "Phar Lap"})),
        "req-missing",
    )
    .await;
    send_on_lambda(
        &app,
        call("place_bet", json!({"horse": "Phar Lap"})),
        "req-x",
    )
    .await;

    let lines = emf_lines(&sink);
    assert_eq!(lines.len(), 3, "{:?}", sink.lines());
    let outcomes: Vec<_> = lines
        .iter()
        .map(|l| {
            (
                l["Tool"].as_str().unwrap(),
                l["Outcome"].as_str().unwrap(),
                l["RequestId"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("horse_form", "found", "req-found"),
            ("horse_form", "not_found", "req-missing"),
            // A tool the server doesn't have is never a dimension value.
            ("unknown", "error", "req-x"),
        ]
    );
    for (raw, line) in sink.lines().iter().zip(&lines) {
        let cw = &line["_aws"]["CloudWatchMetrics"][0];
        assert_eq!(cw["Namespace"], "Trackside");
        assert_eq!(cw["Dimensions"], json!([["Tool", "Outcome"]]));
        assert!(line["LatencyMs"].is_number(), "{raw}");
        assert_eq!(line["Calls"], 1.0);
        assert!(line["_aws"]["Timestamp"].is_i64());
        // Nothing the listener said, and nobody's identity.
        let lower = raw.to_lowercase();
        for leak in [
            "sample stayer",
            "phar lap",
            "place_bet",
            "\"sub\"",
            "local",
            "bearer",
        ] {
            assert!(!lower.contains(leak), "{leak} in {raw}");
        }
    }
}

#[tokio::test]
async fn healthz_reports_the_snapshot_and_writes_no_metric() {
    let sink = Arc::new(crate::telemetry::InMemory::default());
    let fixture: Fixture = serde_json::from_slice(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/demo.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let info = crate::telemetry::SnapshotInfo::of(&fixture);
    let app = app_metered(Arc::new(demo_store()), sink.clone(), info);
    let (status, _, v) = send(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["ok"], true);
    assert_eq!(v["snapshot_date"], "2026-10-17");
    assert_eq!(v["meetings"], fixture.meetings.len());
    assert!(v["uptime_s"].is_u64(), "{v}");
    assert!(sink.lines().is_empty(), "{:?}", sink.lines());
}

/// A store whose every read fails the way S3 and DynamoDB do, with names in the message.
struct FailingStore;

const STORE_ERROR: &str = "reading s3://bucket/key: DynamoDB ResourceNotFoundException";

#[async_trait::async_trait]
impl trackside_core::Store for FailingStore {
    async fn meetings(&self, _: chrono::NaiveDate) -> anyhow::Result<Vec<trackside_core::Meeting>> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn race_card(
        &self,
        _: chrono::NaiveDate,
        _: &str,
        _: u32,
    ) -> anyhow::Result<Option<RaceCard>> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn horse_form(&self, _: &str) -> anyhow::Result<Option<trackside_core::HorseForm>> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn race_result(
        &self,
        _: chrono::NaiveDate,
        _: &str,
        _: u32,
    ) -> anyhow::Result<Option<trackside_core::RaceResult>> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn person_stats(
        &self,
        _: &str,
        _: &str,
        _: Option<chrono::NaiveDate>,
        _: Option<chrono::NaiveDate>,
    ) -> anyhow::Result<Option<trackside_core::PersonStats>> {
        anyhow::bail!(STORE_ERROR)
    }
}

/// Listener memory that fails the way DynamoDB does.
struct FailingMemory;

#[async_trait::async_trait]
impl Memory for FailingMemory {
    async fn load(&self, _: &str) -> anyhow::Result<Profile> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn save(&self, _: &str, _: &Profile) -> anyhow::Result<()> {
        anyhow::bail!(STORE_ERROR)
    }
    async fn forget(&self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!(STORE_ERROR)
    }
    fn durable(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn a_failing_store_is_heard_as_one_plain_sentence() {
    let sink = Arc::new(crate::telemetry::InMemory::default());
    let app = app_metered(Arc::new(FailingStore), sink.clone(), Default::default());
    let tools = [
        ("list_meetings", json!({"date": "2026-10-17"})),
        ("horse_form", json!({"horse": "Sample Stayer"})),
        (
            "jockey_or_trainer_stats",
            json!({"name": "J. Example", "role": "jockey"}),
        ),
        (
            "get_race_card",
            json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"}),
        ),
        (
            "explain_race",
            json!({"venue": "Caulfield", "race_number": 8, "date": "2026-10-17"}),
        ),
    ];
    let memory_app = app_metered_with(
        Arc::new(demo_store()),
        Arc::new(FailingMemory),
        sink.clone(),
    );
    let calls = tools
        .iter()
        .map(|(tool, args)| (&app, *tool, args.clone()))
        .chain([
            (&memory_app, "my_stable", json!({})),
            (
                &memory_app,
                "follow_horse",
                json!({"horse": "Sample Stayer"}),
            ),
            (&memory_app, "forget_me", json!({})),
        ])
        .collect::<Vec<_>>();
    for (app, tool, args) in &calls {
        let v = send_on_lambda(app, call(tool, args.clone()), "req-fail").await;
        let raw = v.to_string();
        let said = v["error"]["message"]
            .as_str()
            .or(v["result"]["content"][0]["text"].as_str())
            .unwrap_or_default();
        assert_eq!(said, crate::tools::INTERNAL_SPOKEN, "{tool}: {raw}");
        for leak in [
            "s3://",
            "Dynamo",
            "bucket/key",
            "ResourceNotFound",
            STORE_ERROR,
        ] {
            assert!(!raw.contains(leak), "{tool} leaked {leak}: {raw}");
        }
        assert!(!said.contains('/'), "{said}");
        // Still JSON-RPC's internal error.
        assert_eq!(v["error"]["code"], -32603, "{raw}");
    }
    let lines = emf_lines(&sink);
    assert_eq!(lines.len(), calls.len());
    assert!(lines.iter().all(|l| l["Outcome"] == "error"), "{lines:?}");
}

// ---- next_race ----

async fn next_race_at(router: &axum::Router, args: Value) -> (String, Value) {
    let (_, _, v) = send(router, "POST", "/mcp", None, Some(call("next_race", args))).await;
    (spoken(&v), v["result"]["structuredContent"].clone())
}

fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// 4:35 pm in Melbourne on Cup day: the Eagle Farm race jumped at 12:10 pm Queensland time,
/// so the next race is the Caulfield Cup, 25 minutes away.
#[tokio::test]
async fn next_race_is_the_next_to_jump() {
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-17T05:35:00Z").await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with("The next race is race 8 at Caulfield, the Caulfield Cup over 2400 metres, due to jump in about 25 minutes, at 5 pm Melbourne time."),
        "{text}"
    );
    assert!(
        !text.contains("Eagle Farm") && !text.contains("After that"),
        "{text}"
    );
    assert!(words(&text) < 60, "{text}");
    assert_eq!(s["found"], true);
    assert_eq!(s["when"], "today");
    assert_eq!(s["next"]["venue"], "Caulfield");
    assert_eq!(s["next"]["race_number"], 8);
    assert_eq!(s["next"]["minutes_until"], 25);
    assert_eq!(s["next"]["start_utc"], "2026-10-17T06:00:00+00:00");
    assert!(s["next"]["home_time"].is_null());
    assert_eq!(s["then"], json!([]));

    // A Queensland listener hears their own clock first.
    send(
        &router,
        "POST",
        "/mcp",
        None,
        Some(call("set_home_state", json!({"state": "QLD"}))),
    )
    .await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.contains(
            "due to jump in about 25 minutes, at 4 pm Queensland time, 5 pm at the track."
        ),
        "{text}"
    );
    assert!(!text.contains("near you"), "{text}");
    assert_eq!(s["next"]["home_time"], "16:00");
    assert_eq!(s["home_state"], "QLD");
    let lower = text.to_lowercase();
    for word in [
        "odds",
        "bet",
        "tip",
        "tips",
        "chance",
        "likely",
        "contender",
    ] {
        assert!(!has_word(&lower, word), "{word}: {text}");
    }
    assert!(!text.contains('\u{2014}'), "{text}");
}

#[tokio::test]
async fn next_race_after_the_last_says_racing_is_done() {
    // 7 pm in Melbourne on Cup day; the fixture has nothing on Sunday or after.
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-17T08:00:00Z").await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with(
            "Racing is done for today, and I don't have any racing on file for tomorrow."
        ),
        "{text}"
    );
    assert_eq!(s["found"], false);
    assert!(s["next"].is_null());

    // Friday night: nothing today, the Saturday card tomorrow, first race at Eagle Farm.
    let router = app_at(false, Arc::new(InMemory::default()), "2026-10-16T10:00:00Z").await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with("There's no racing on file for today; tomorrow's first race is race 1 at Eagle Farm at 12:10 pm Queensland time."),
        "{text}"
    );
    assert_eq!(s["when"], "tomorrow");
    assert_eq!(s["then"][0]["venue"], "Caulfield");
    // A venue heard a little wrong is still found on tomorrow's card.
    let (text, _) = next_race_at(&router, json!({"venue": "Cofield"})).await;
    assert!(
        text.starts_with("There's no racing at Caulfield on file for today; tomorrow's first race is race 8 at Caulfield at 5 pm Melbourne time."),
        "{text}"
    );
}

#[tokio::test]
async fn next_race_midweek_names_the_next_day_with_racing() {
    let router = app(false).await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with("I don't have any racing on file for today or tomorrow. The next racing I have is on Saturday 17 October, starting with race 1 at Eagle Farm at 12:10 pm Queensland time."),
        "{text}"
    );
    assert_eq!(s["when"], "later");
    assert_eq!(s["next"]["date"], "2026-10-17");
    assert!(s["next"]["minutes_until"].as_i64().unwrap() > 0);
    let (text, _) = next_race_at(&router, json!({"state": "vic"})).await;
    assert!(
        text.contains(
            "on Saturday 17 October, starting with race 8 at Caulfield at 5 pm Melbourne time."
        ),
        "{text}"
    );
    let (_, _, v) = send(
        &router,
        "POST",
        "/mcp",
        None,
        Some(call("next_race", json!({"state": "Narnia"}))),
    )
    .await;
    assert!(
        v.get("error").is_some() || v["result"]["isError"] == true,
        "{v}"
    );
}

/// A Saturday afternoon with Caulfield running ahead of Doomben, and a race with no time yet.
fn a_busy_afternoon() -> FixtureStore {
    let fixture: Fixture = serde_json::from_value(json!({
        "meetings": [
            { "date": "2026-10-17", "state": "VIC", "venue": "Caulfield", "races": [
                { "race_number": 5, "name": "Example Handicap", "start_local": "15:40", "distance_m": 1400, "runners": [] },
                { "race_number": 6, "name": "", "start_local": "TBA", "distance_m": 1600, "runners": [] }
            ] },
            { "date": "2026-10-17", "state": "QLD", "venue": "Doomben", "races": [
                { "race_number": 2, "name": "", "start_local": "15:05", "distance_m": 1200, "runners": [] }
            ] },
            { "date": "2026-10-17", "state": "NSW", "venue": "Randwick", "races": [
                { "race_number": 3, "name": "", "start_local": "15:50", "distance_m": 1100, "runners": [] }
            ] }
        ]
    }))
    .unwrap();
    FixtureStore::from_fixture(fixture)
}

#[tokio::test]
async fn next_race_near_you_comes_first() {
    // 3:15 pm in Melbourne, 2:15 pm in Brisbane.
    let router = app_with(
        false,
        Arc::new(a_busy_afternoon()),
        Arc::new(InMemory::default()),
        "2026-10-17T04:15:00Z",
    )
    .await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with("The next race is race 5 at Caulfield, the Example Handicap over 1400 metres, due to jump in about 25 minutes, at 3:40 pm Melbourne time. After that, race 3 at Randwick at 3:50 pm Sydney time."),
        "{text}"
    );
    assert!(words(&text) < 60, "{text}");
    // The TBA race can't be placed on the clock, so it isn't in the list.
    let then: Vec<_> = s["then"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["venue"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(then, ["Randwick", "Doomben"]);

    send(
        &router,
        "POST",
        "/mcp",
        None,
        Some(call("set_home_state", json!({"state": "QLD"}))),
    )
    .await;
    let (text, s) = next_race_at(&router, json!({})).await;
    assert!(
        text.starts_with("The next race near you is race 2 at Doomben, due to jump in about 50 minutes, at 3:05 pm Queensland time; the next anywhere is race 5 at Caulfield at 2:40 pm Queensland time."),
        "{text}"
    );
    assert!(words(&text) < 60, "{text}");
    assert_eq!(s["next"]["venue"], "Doomben");
    assert_eq!(s["next_anywhere"]["venue"], "Caulfield");
    assert_eq!(s["next_anywhere"]["home_time"], "14:40");
    assert_eq!(s["then"][0]["venue"], "Caulfield");

    // Asking about one state or venue answers just that.
    let (text, s) = next_race_at(&router, json!({"venue": "Randwick"})).await;
    assert!(
        text.starts_with("The next race at Randwick is race 3 at Randwick, a race over 1100 metres, due to jump in about 35 minutes, at 2:50 pm Queensland time, 3:50 pm at the track."),
        "{text}"
    );
    assert_eq!(s["then"], json!([]));
    let (_, s) = next_race_at(&router, json!({"state": "VIC"})).await;
    assert_eq!(s["next"]["venue"], "Caulfield");
}
