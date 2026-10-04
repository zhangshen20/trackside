//! The conversation loop: a Claude model on Amazon Bedrock (Converse API) plays the Alexa+
//! side, choosing Trackside's MCP tools and turning their answers into one short spoken reply.
//!
//! Each turn sends the conversation and the MCP server's own tool list to Bedrock. When the
//! model asks for a tool, the simulator calls it on the MCP server as the signed-in user and
//! hands the result back, until the model answers in words.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ConverseOutput, InferenceConfiguration, Message, StopReason,
    SystemContentBlock, Tool, ToolConfiguration, ToolInputSchema, ToolResultBlock,
    ToolResultContentBlock, ToolResultStatus, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::{Document, Number};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::{ToolDef, ToolOutput};

/// Tool rounds allowed per user turn before the simulator stops and says so.
const MAX_ROUNDS: usize = 5;

/// One model turn: the assistant message (kept whole, reasoning included, so it can be sent
/// back unchanged) and why the model stopped.
pub struct ModelTurn {
    pub message: Message,
    pub stop: StopReason,
}

#[async_trait]
pub trait Model: Send + Sync {
    async fn turn(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<ModelTurn>;
}

/// Where the loop's tool calls go: the MCP server, as the signed-in user.
#[async_trait]
pub trait ToolRunner: Send + Sync {
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput>;
}

/// A message in the browser's view of the conversation: text only.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Utterance {
    pub role: String,
    pub text: String,
}

/// One tool call, as the page shows it: the screen card and the trace line.
#[derive(Clone, Debug, Serialize)]
pub struct ToolTrace {
    pub tool: String,
    pub input: Value,
    #[serde(flatten)]
    pub output: ToolOutput,
}

#[derive(Debug, Serialize)]
pub struct Reply {
    pub text: String,
    pub tools: Vec<ToolTrace>,
}

pub fn system_prompt(today: &str) -> String {
    format!(
        "You are the voice of a simulated Alexa+ device, answering racing questions through the \
Trackside add-on, a form guide for Australian thoroughbred racing. Today is {today} in Melbourne.

Everything you say is read aloud. Answer in one to three short sentences of plain speech: no \
lists, markdown, emoji or URLs. Lead with the answer. Use only what Trackside's tools return; \
if they don't cover something, say so briefly rather than guessing. Keep the source the tool \
names, for example \"according to Racing Australia\".

Use the tools whenever the question is about meetings, fields, horses, results, jockeys, \
trainers, the user's followed horses or the Spring Carnival. Resolve relative dates such as \
\"Saturday\" or \"yesterday\" to YYYY-MM-DD from the dates listed above: \"last Saturday\" is the most \
recent Saturday before today, and \"this Saturday\" is today if today is Saturday, otherwise \
the next one. When a question names a race without a \
number, look at the day's meetings first.

Trackside is a fan companion with no betting. Never give odds, prices, tips, bets or \
predictions of who will win, even if asked; say that Trackside doesn't do betting and offer \
the form instead. Describe past form only: never call a runner a chance, a contender or a \
favourite. A screen shows the details, so you don't need to read out every runner."
    )
}

/// Runs one user turn to completion.
pub async fn respond(
    model: &dyn Model,
    tools: &dyn ToolRunner,
    tool_defs: &[ToolDef],
    system: &str,
    history: &[Utterance],
) -> Result<Reply> {
    let mut messages = to_messages(history)?;
    let mut traces = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let turn = model.turn(system, &messages, tool_defs).await?;
        let uses: Vec<ToolUseBlock> = turn
            .message
            .content()
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        let text = spoken_text(&turn.message);
        if turn.stop != StopReason::ToolUse || uses.is_empty() {
            if text.is_empty() {
                bail!("the model stopped without an answer ({:?})", turn.stop);
            }
            return Ok(Reply {
                text,
                tools: traces,
            });
        }
        messages.push(turn.message);
        let mut results = Vec::with_capacity(uses.len());
        for u in uses {
            let input = to_json(u.input());
            let output = match tools.call(u.name(), input.clone()).await {
                Ok(out) => out,
                Err(e) if e.is::<crate::mcp::Unauthorized>() => return Err(e),
                Err(e) => ToolOutput {
                    text: format!("The tool failed: {e}"),
                    structured: None,
                    is_error: true,
                },
            };
            results.push(ContentBlock::ToolResult(
                ToolResultBlock::builder()
                    .tool_use_id(u.tool_use_id())
                    .content(ToolResultContentBlock::Text(if output.text.is_empty() {
                        "(no text)".into()
                    } else {
                        output.text.clone()
                    }))
                    .status(if output.is_error {
                        ToolResultStatus::Error
                    } else {
                        ToolResultStatus::Success
                    })
                    .build()?,
            ));
            traces.push(ToolTrace {
                tool: u.name().to_string(),
                input,
                output,
            });
        }
        messages.push(
            Message::builder()
                .role(ConversationRole::User)
                .set_content(Some(results))
                .build()?,
        );
    }
    Ok(Reply {
        text: "Sorry, that one took too many steps. Could you ask it a simpler way?".into(),
        tools: traces,
    })
}

fn spoken_text(message: &Message) -> String {
    message
        .content()
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.trim()),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The browser keeps only the words of earlier turns; tool calls are redone as needed.
fn to_messages(history: &[Utterance]) -> Result<Vec<Message>> {
    let mut out: Vec<Message> = Vec::new();
    for u in history.iter().filter(|u| !u.text.trim().is_empty()) {
        let role = match u.role.as_str() {
            "user" => ConversationRole::User,
            "assistant" => ConversationRole::Assistant,
            other => bail!("unknown role {other}"),
        };
        // Bedrock wants turns to alternate; merge any repeats.
        if let Some(last) = out.last_mut() {
            if *last.role() == role {
                let mut content = last.content().to_vec();
                content.push(ContentBlock::Text(u.text.clone()));
                *last = Message::builder()
                    .role(role)
                    .set_content(Some(content))
                    .build()?;
                continue;
            }
        }
        out.push(
            Message::builder()
                .role(role)
                .content(ContentBlock::Text(u.text.clone()))
                .build()?,
        );
    }
    // A conversation must open and close with the user.
    while out
        .first()
        .is_some_and(|m| *m.role() != ConversationRole::User)
    {
        out.remove(0);
    }
    if out.last().map(|m| m.role()) != Some(&ConversationRole::User) {
        bail!("the conversation must end with something the user said");
    }
    Ok(out)
}

pub struct Bedrock {
    pub client: aws_sdk_bedrockruntime::Client,
    pub model_id: String,
    /// Extra request fields for the model, e.g. `{"output_config": {"effort": "low"}}`.
    pub extra: Option<Value>,
}

#[async_trait]
impl Model for Bedrock {
    async fn turn(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<ModelTurn> {
        let mut config = ToolConfiguration::builder();
        for t in tools {
            config = config.tools(Tool::ToolSpec(
                ToolSpecification::builder()
                    .name(&t.name)
                    .description(&t.description)
                    .input_schema(ToolInputSchema::Json(to_document(&tool_schema(
                        &t.input_schema,
                    ))))
                    .build()?,
            ));
        }
        let request = |extra: Option<&Value>| {
            let mut req = self
                .client
                .converse()
                .model_id(&self.model_id)
                .system(SystemContentBlock::Text(system.to_string()))
                .set_messages(Some(messages.to_vec()))
                .inference_config(InferenceConfiguration::builder().max_tokens(4096).build());
            if !tools.is_empty() {
                req = req.tool_config(config.clone().build().expect("tools are set"));
            }
            if let Some(extra) = extra {
                req = req.additional_model_request_fields(to_document(extra));
            }
            req
        };
        let resp = match request(self.extra.as_ref()).send().await {
            Ok(resp) => resp,
            // A model that doesn't take the extra fields: note it in the log and go without.
            Err(e) if self.extra.is_some() && is_validation(&e) => {
                tracing::warn!(error = %bedrock_error(&e), "retrying without extra model fields");
                request(None)
                    .send()
                    .await
                    .map_err(|e| anyhow!("Bedrock: {}", bedrock_error(&e)))?
            }
            Err(e) => return Err(anyhow!("Bedrock: {}", bedrock_error(&e))),
        };
        let stop = resp.stop_reason().clone();
        match resp.output {
            Some(ConverseOutput::Message(message)) => Ok(ModelTurn { message, stop }),
            _ => bail!("Bedrock returned no message"),
        }
    }
}

/// "ValidationException: Operation not allowed" rather than the SDK's whole response dump.
fn bedrock_error<R>(
    e: &aws_sdk_bedrockruntime::error::SdkError<
        aws_sdk_bedrockruntime::operation::converse::ConverseError,
        R,
    >,
) -> String {
    use aws_sdk_bedrockruntime::error::ProvideErrorMetadata;
    match e.as_service_error() {
        Some(s) => format!(
            "{}: {}",
            s.code().unwrap_or("error"),
            s.message().unwrap_or("no message")
        ),
        None => e.to_string(),
    }
}

fn is_validation<R>(
    e: &aws_sdk_bedrockruntime::error::SdkError<
        aws_sdk_bedrockruntime::operation::converse::ConverseError,
        R,
    >,
) -> bool {
    e.as_service_error()
        .is_some_and(|s| s.is_validation_exception())
}

/// Bedrock wants an object schema; MCP servers may leave out `type` or `properties`.
fn tool_schema(schema: &Value) -> Value {
    let mut s = match schema {
        Value::Object(m) => Value::Object(m.clone()),
        _ => serde_json::json!({}),
    };
    let obj = s.as_object_mut().expect("object");
    obj.entry("type").or_insert_with(|| "object".into());
    obj.entry("properties")
        .or_insert_with(|| serde_json::json!({}));
    obj.remove("$schema");
    s
}

pub fn to_document(v: &Value) -> Document {
    match v {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => Document::Number(if let Some(u) = n.as_u64() {
            Number::PosInt(u)
        } else if let Some(i) = n.as_i64() {
            Number::NegInt(i)
        } else {
            Number::Float(n.as_f64().unwrap_or_default())
        }),
        Value::String(s) => Document::String(s.clone()),
        Value::Array(a) => Document::Array(a.iter().map(to_document).collect()),
        Value::Object(o) => Document::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), to_document(v)))
                .collect::<HashMap<_, _>>(),
        ),
    }
}

pub fn to_json(d: &Document) -> Value {
    match d {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(Number::PosInt(u)) => Value::from(*u),
        Document::Number(Number::NegInt(i)) => Value::from(*i),
        Document::Number(Number::Float(f)) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Document::String(s) => Value::String(s.clone()),
        Document::Array(a) => Value::Array(a.iter().map(to_json).collect()),
        Document::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    /// Plays back scripted assistant messages and records what it was sent.
    struct Script {
        turns: Mutex<Vec<ModelTurn>>,
        seen: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl Model for Script {
        async fn turn(&self, _: &str, messages: &[Message], _: &[ToolDef]) -> Result<ModelTurn> {
            self.seen.lock().unwrap().push(messages.len());
            Ok(self.turns.lock().unwrap().remove(0))
        }
    }

    struct Tools(Mutex<Vec<(String, Value)>>);

    #[async_trait]
    impl ToolRunner for Tools {
        async fn call(&self, name: &str, args: Value) -> Result<ToolOutput> {
            self.0.lock().unwrap().push((name.into(), args));
            Ok(ToolOutput {
                text: "Race 7 at Flemington is the Demo Stakes.".into(),
                structured: Some(json!({"card": {"race_number": 7}})),
                is_error: false,
            })
        }
    }

    fn assistant(blocks: Vec<ContentBlock>) -> Message {
        Message::builder()
            .role(ConversationRole::Assistant)
            .set_content(Some(blocks))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn calls_the_tool_then_answers() {
        let tool_use = ToolUseBlock::builder()
            .tool_use_id("t1")
            .name("get_race_card")
            .input(to_document(
                &json!({"venue": "Flemington", "race_number": 7}),
            ))
            .build()
            .unwrap();
        let model = Script {
            turns: Mutex::new(vec![
                ModelTurn {
                    message: assistant(vec![ContentBlock::ToolUse(tool_use)]),
                    stop: StopReason::ToolUse,
                },
                ModelTurn {
                    message: assistant(vec![ContentBlock::Text(
                        "It's the Demo Stakes, according to Racing Australia.".into(),
                    )]),
                    stop: StopReason::EndTurn,
                },
            ]),
            seen: Mutex::new(vec![]),
        };
        let tools = Tools(Mutex::new(vec![]));
        let history = vec![Utterance {
            role: "user".into(),
            text: "Who's in race 7 at Flemington?".into(),
        }];
        let reply = respond(&model, &tools, &[], "sys", &history).await.unwrap();

        assert_eq!(
            reply.text,
            "It's the Demo Stakes, according to Racing Australia."
        );
        assert_eq!(reply.tools.len(), 1);
        assert_eq!(reply.tools[0].tool, "get_race_card");
        assert_eq!(reply.tools[0].input["race_number"], 7);
        assert_eq!(tools.0.lock().unwrap()[0].1["venue"], "Flemington");
        // Second turn carries the question, the tool call and the tool result.
        assert_eq!(*model.seen.lock().unwrap(), vec![1, 3]);
    }

    #[test]
    fn history_starts_and_ends_with_the_user() {
        let history = vec![
            Utterance {
                role: "assistant".into(),
                text: "Hi! Ask me about racing.".into(),
            },
            Utterance {
                role: "user".into(),
                text: "What's on Saturday?".into(),
            },
            Utterance {
                role: "user".into(),
                text: "At Caulfield".into(),
            },
        ];
        let messages = to_messages(&history).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content().len(), 2);
        assert!(to_messages(&[Utterance {
            role: "assistant".into(),
            text: "x".into()
        }])
        .is_err());
    }

    #[test]
    fn documents_round_trip() {
        let v = json!({"a": [1, -2, 2.5, "x", null, true], "b": {}});
        assert_eq!(to_json(&to_document(&v)), v);
    }

    #[test]
    fn schemas_become_objects() {
        let s = tool_schema(&json!({"$schema": "x", "properties": {"horse": {"type": "string"}}}));
        assert_eq!(s["type"], "object");
        assert!(s.get("$schema").is_none());
    }
}
