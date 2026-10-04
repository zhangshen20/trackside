//! Per-tool metrics as CloudWatch Embedded Metric Format (EMF) lines.
//!
//! On Lambda (or locally with `TRACKSIDE_METRICS=1`) every tool call prints one JSON line on
//! stdout. CloudWatch Logs reads the `_aws` block and turns the line into metrics in the
//! `Trackside` namespace, with no agent and no `PutMetricData` call:
//!
//! - per call, dimensions `Tool` and `Outcome` (found, not_found, did_you_mean, error):
//!   `LatencyMs` and `Calls` = 1, plus `BedrockMs` when `explain_race` asked Bedrock and
//!   `BedrockFallback` = 1 (with a `Reason` property) when it fell back to its template;
//! - once per process, at start-up: `SnapshotLoadMs` and `ColdStart` = 1.
//!
//! The Lambda request id rides along as a property so a slow line can be matched to its log.
//! A line never carries the listener's subject, a token, a horse's name or any argument: the
//! only free text is the tool's name, and only for tools the server has.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::CallToolResponse;
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};

pub const NAMESPACE: &str = "Trackside";

/// Where finished metric lines go.
pub trait Sink: Send + Sync {
    fn emit(&self, line: String);
}

/// CloudWatch reads EMF from the function's log stream, which is stdout.
pub struct StdoutEmf;

impl Sink for StdoutEmf {
    fn emit(&self, line: String) {
        use std::io::Write;
        // One write per line, so a log line from another thread can't land inside it.
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
    }
}

/// Keeps every line, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct InMemory(Mutex<Vec<String>>);

#[cfg(test)]
impl InMemory {
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Sink for InMemory {
    fn emit(&self, line: String) {
        self.0.lock().unwrap().push(line);
    }
}

/// The sink to use: stdout EMF on Lambda or with `TRACKSIDE_METRICS=1`, otherwise none.
pub fn from_env(on_lambda: bool) -> Option<Arc<dyn Sink>> {
    let wanted = on_lambda || std::env::var("TRACKSIDE_METRICS").is_ok_and(|v| v == "1");
    wanted.then(|| Arc::new(StdoutEmf) as Arc<dyn Sink>)
}

/// How a tool call ended, as a dimension value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Found,
    NotFound,
    DidYouMean,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Found => "found",
            Outcome::NotFound => "not_found",
            Outcome::DidYouMean => "did_you_mean",
            Outcome::Error => "error",
        }
    }
}

/// Reads the outcome off a tool's answer: an error or `isError` is an error; structured
/// content offering `did_you_mean` names is a did-you-mean; `found: false` is not found;
/// anything else answered.
pub fn classify(result: &Result<CallToolResponse, McpError>) -> Outcome {
    let Ok(CallToolResponse::Complete(result)) = result else {
        return match result {
            Err(_) => Outcome::Error,
            Ok(_) => Outcome::Found,
        };
    };
    if result.is_error == Some(true) {
        return Outcome::Error;
    }
    let structured = result.structured_content.as_ref();
    if structured.and_then(|s| s.get("did_you_mean")).is_some() {
        return Outcome::DidYouMean;
    }
    if structured.and_then(|s| s.get("found")) == Some(&Value::Bool(false)) {
        return Outcome::NotFound;
    }
    Outcome::Found
}

/// Why `explain_race` used its template instead of Bedrock's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// Bedrock took longer than the voice budget.
    Timeout,
    /// The call failed (throttled, no model access, network).
    Error,
    /// Bedrock answered, but empty or in words Trackside never says.
    Rejected,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::Timeout => "timeout",
            FallbackReason::Error => "error",
            FallbackReason::Rejected => "rejected",
        }
    }
}

/// One EMF line: dimension values, metrics and plain properties. Values are fixed strings
/// and numbers chosen by the server, never what a listener said.
#[derive(Clone, Debug, Default)]
pub struct Record {
    dimensions: Vec<(&'static str, String)>,
    metrics: Vec<(&'static str, f64, &'static str)>,
    properties: Vec<(&'static str, String)>,
}

impl Record {
    /// A tool call: `Tool` and `Outcome` dimensions, `LatencyMs` and `Calls`.
    pub fn tool_call(tool: &str, outcome: Outcome, latency: Duration) -> Self {
        Self::default()
            .dimension("Tool", tool)
            .dimension("Outcome", outcome.as_str())
            .metric("LatencyMs", millis(latency), "Milliseconds")
            .metric("Calls", 1.0, "Count")
    }

    /// The process start: how long the snapshot took to load, and that this was a cold start.
    pub fn init(snapshot_load: Duration) -> Self {
        Self::default()
            .metric("SnapshotLoadMs", millis(snapshot_load), "Milliseconds")
            .metric("ColdStart", 1.0, "Count")
    }

    pub fn dimension(mut self, name: &'static str, value: &str) -> Self {
        self.dimensions.push((name, value.to_string()));
        self
    }

    pub fn metric(mut self, name: &'static str, value: f64, unit: &'static str) -> Self {
        self.metrics.push((name, value, unit));
        self
    }

    pub fn property(mut self, name: &'static str, value: &str) -> Self {
        self.properties.push((name, value.to_string()));
        self
    }

    /// Joins the notes gathered by [`noting`] onto a tool-call record.
    pub fn merge(mut self, notes: Record) -> Self {
        self.metrics.extend(notes.metrics);
        self.properties.extend(notes.properties);
        self
    }

    /// Adds what the call noted along the way (Bedrock's latency, a fallback).
    fn with_notes(mut self, notes: &Notes) -> Self {
        if let Some(ms) = notes.bedrock_ms {
            self = self.metric("BedrockMs", ms, "Milliseconds");
        }
        if let Some(reason) = notes.fallback {
            self = self
                .metric("BedrockFallback", 1.0, "Count")
                .property("Reason", reason.as_str());
        }
        self
    }

    /// The EMF JSON for this record, stamped `timestamp_ms` (Unix epoch milliseconds).
    pub fn to_emf(&self, timestamp_ms: i64) -> String {
        let dimension_set: Vec<&str> = self.dimensions.iter().map(|(k, _)| *k).collect();
        let metrics: Vec<Value> = self
            .metrics
            .iter()
            .map(|(name, _, unit)| json!({ "Name": name, "Unit": unit }))
            .collect();
        let mut line = Map::new();
        line.insert(
            "_aws".into(),
            json!({
                "Timestamp": timestamp_ms,
                "CloudWatchMetrics": [{
                    "Namespace": NAMESPACE,
                    "Dimensions": [dimension_set],
                    "Metrics": metrics,
                }],
            }),
        );
        for (k, v) in &self.dimensions {
            line.insert((*k).into(), json!(v));
        }
        for (k, v, _) in &self.metrics {
            line.insert((*k).into(), json!(v));
        }
        for (k, v) in &self.properties {
            line.insert((*k).into(), json!(v));
        }
        Value::Object(line).to_string()
    }

    pub fn emit(&self, sink: &dyn Sink) {
        sink.emit(self.to_emf(chrono::Utc::now().timestamp_millis()));
    }
}

fn millis(d: Duration) -> f64 {
    // Two decimals: a warm call is well under a millisecond on a laptop.
    (d.as_secs_f64() * 100_000.0).round() / 100.0
}

/// What code deep inside a tool call reports for that call's line.
#[derive(Clone, Debug, Default)]
struct Notes {
    bedrock_ms: Option<f64>,
    fallback: Option<FallbackReason>,
}

tokio::task_local! {
    static NOTES: Arc<Mutex<Notes>>;
}

/// Runs `call` (a tool call) and returns its answer with what it noted on the way, so the
/// summariser can report Bedrock without knowing about sinks.
pub async fn noting<F: std::future::Future>(call: F) -> (F::Output, Record) {
    let notes = Arc::new(Mutex::new(Notes::default()));
    let out = NOTES.scope(notes.clone(), call).await;
    let notes = notes.lock().map(|n| n.clone()).unwrap_or_default();
    (out, Record::default().with_notes(&notes))
}

fn note(f: impl FnOnce(&mut Notes)) {
    let _ = NOTES.try_with(|n| {
        if let Ok(mut n) = n.lock() {
            f(&mut n);
        }
    });
}

/// How long Bedrock took to answer, for this call's line.
pub fn note_bedrock(latency: Duration) {
    note(|n| n.bedrock_ms = Some(millis(latency)));
}

/// That this call fell back to the template, and why.
pub fn note_fallback(reason: FallbackReason) {
    note(|n| n.fallback = Some(reason));
}

/// The Lambda request id of the HTTP request a tool call came in on, when there is one.
pub fn request_id(parts: Option<&http::request::Parts>) -> Option<String> {
    parts?
        .extensions
        .get::<lambda_http::Context>()
        .map(|c| c.request_id.clone())
        .filter(|id| !id.is_empty())
}

/// What `/healthz` reports about the data being served.
#[derive(Clone, Debug, Default)]
pub struct SnapshotInfo {
    /// The last date the snapshot has a meeting on.
    pub snapshot_date: Option<chrono::NaiveDate>,
    pub meetings: usize,
}

impl SnapshotInfo {
    pub fn of(fixture: &trackside_core::Fixture) -> Self {
        Self {
            snapshot_date: fixture.meetings.iter().map(|m| m.date).max(),
            meetings: fixture.meetings.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolResult, ContentBlock};

    fn answered(structured: Value) -> Result<CallToolResponse, McpError> {
        let mut r = CallToolResult::success(vec![ContentBlock::text("x")]);
        r.structured_content = Some(structured);
        Ok(r.into())
    }

    #[test]
    fn classifies_outcomes() {
        assert_eq!(
            classify(&answered(json!({ "found": true }))),
            Outcome::Found
        );
        assert_eq!(
            classify(&answered(json!({ "meetings": [] }))),
            Outcome::Found
        );
        assert_eq!(
            classify(&answered(json!({ "found": false }))),
            Outcome::NotFound
        );
        assert_eq!(
            classify(&answered(
                json!({ "found": false, "did_you_mean": ["Warwick Farm"] })
            )),
            Outcome::DidYouMean
        );
        assert_eq!(
            classify(&Err(McpError::internal_error("x", None))),
            Outcome::Error
        );
        let failed: Result<CallToolResponse, McpError> =
            Ok(CallToolResult::error(vec![ContentBlock::text("x")]).into());
        assert_eq!(classify(&failed), Outcome::Error);
    }

    #[test]
    fn emf_shape() {
        let line = Record::tool_call("horse_form", Outcome::Found, Duration::from_micros(1234))
            .merge(
                Record::default()
                    .metric("BedrockFallback", 1.0, "Count")
                    .property("Reason", "timeout"),
            )
            .property("RequestId", "abc")
            .to_emf(1_700_000_000_000);
        let v: Value = serde_json::from_str(&line).unwrap();
        let cw = &v["_aws"]["CloudWatchMetrics"][0];
        assert_eq!(cw["Namespace"], "Trackside");
        assert_eq!(cw["Dimensions"], json!([["Tool", "Outcome"]]));
        let names: Vec<_> = cw["Metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["Name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["LatencyMs", "Calls", "BedrockFallback"]);
        assert_eq!(v["_aws"]["Timestamp"], 1_700_000_000_000i64);
        assert_eq!(v["Tool"], "horse_form");
        assert_eq!(v["Outcome"], "found");
        assert_eq!(v["LatencyMs"].as_f64(), Some(1.23));
        assert_eq!(v["Calls"].as_f64(), Some(1.0));
        assert_eq!(v["Reason"], "timeout");
        assert_eq!(v["RequestId"], "abc");
    }

    #[test]
    fn init_line_has_no_dimensions() {
        let v: Value =
            serde_json::from_str(&Record::init(Duration::from_millis(40)).to_emf(0)).unwrap();
        assert_eq!(v["_aws"]["CloudWatchMetrics"][0]["Dimensions"], json!([[]]));
        assert_eq!(v["SnapshotLoadMs"].as_f64(), Some(40.0));
        assert_eq!(v["ColdStart"].as_f64(), Some(1.0));
    }

    #[tokio::test]
    async fn notes_reach_the_record() {
        let ((), notes) = noting(async {
            note_bedrock(Duration::from_millis(5));
            note_fallback(FallbackReason::Rejected);
        })
        .await;
        let v: Value = serde_json::from_str(&notes.to_emf(0)).unwrap();
        assert_eq!(v["BedrockMs"].as_f64(), Some(5.0));
        assert_eq!(v["BedrockFallback"].as_f64(), Some(1.0));
        assert_eq!(v["Reason"], "rejected");
        // Outside a call, noting is a no-op rather than a panic.
        note_fallback(FallbackReason::Error);
    }
}
