//! A small MCP client over Streamable HTTP: the calls Alexa+ makes to an add-on's server
//! (`tools/list`, `tools/call`), each as one JSON-RPC POST with the user's bearer token.
//!
//! The deployed server is stateless and answers with plain JSON; a server with sessions may
//! answer with a server-sent-event stream instead, so both are read.

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{header, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone)]
pub struct McpClient {
    url: String,
    http: reqwest::Client,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

/// What one tool call returned: the spoken text, and the structured content a screen draws.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolOutput {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
}

/// The server refused the token (expired or missing): the caller should refresh and retry,
/// or ask the user to link their account again.
#[derive(Debug)]
pub struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the MCP server refused the token")
    }
}

impl std::error::Error for Unauthorized {}

impl McpClient {
    pub fn new(url: String, http: reqwest::Client) -> Self {
        Self { url, http }
    }

    async fn rpc(&self, token: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(&self.url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{method} to {}", self.url))?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Unauthorized.into());
        }
        let is_sse = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = resp.text().await?;
        if !status.is_success() {
            bail!(
                "{method}: HTTP {status}: {}",
                body.chars().take(300).collect::<String>()
            );
        }
        let message = if is_sse {
            last_sse_message(&body)?
        } else {
            serde_json::from_str(&body)?
        };
        if let Some(err) = message.get("error") {
            bail!(
                "{method}: {}",
                err.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
            );
        }
        message
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("{method}: no result in the response"))
    }

    pub async fn list_tools(&self, token: Option<&str>) -> Result<Vec<ToolDef>> {
        let result = self.rpc(token, "tools/list", json!({})).await?;
        Ok(serde_json::from_value(
            result.get("tools").cloned().unwrap_or_default(),
        )?)
    }

    pub async fn call_tool(
        &self,
        token: Option<&str>,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput> {
        let result = self
            .rpc(
                token,
                "tools/call",
                json!({"name": name, "arguments": args}),
            )
            .await?;
        Ok(parse_call_result(&result))
    }
}

fn last_sse_message(body: &str) -> Result<Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .rfind(|m| m.get("result").is_some() || m.get("error").is_some())
        .ok_or_else(|| anyhow!("no JSON-RPC response in the event stream"))
}

pub fn parse_call_result(result: &Value) -> ToolOutput {
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    ToolOutput {
        text,
        structured: result.get("structuredContent").cloned(),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_answer_from_an_event_stream() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        assert_eq!(
            last_sse_message(body).unwrap()["result"],
            json!({"tools": []})
        );
    }

    #[test]
    fn keeps_text_and_structured_content() {
        let out = parse_call_result(&json!({
            "content": [{"type": "text", "text": "Race 7 at Flemington"}],
            "structuredContent": {"card": {"race_number": 7}},
        }));
        assert_eq!(out.text, "Race 7 at Flemington");
        assert_eq!(out.structured.unwrap()["card"]["race_number"], 7);
        assert!(!out.is_error);
    }
}
