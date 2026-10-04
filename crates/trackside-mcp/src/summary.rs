//! Spoken race explanations from Amazon Bedrock.
//!
//! `explain_race` gathers the facts itself (race, conditions, field, recent form) and, when
//! `TRACKSIDE_BEDROCK_MODEL` is set, asks a Bedrock model to turn them into two or three
//! sentences a newcomer can follow by ear. The model only rewords facts it is given; it gets
//! no prices, and an answer that drifts into betting language, or a slow or failed call, falls
//! back to the template sentence the tool builds anyway. Without the variable the tool never
//! calls Bedrock.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ConverseOutput, InferenceConfiguration, Message,
    SystemContentBlock,
};

const SYSTEM: &str = "You explain Australian thoroughbred races to newcomers, for a voice assistant that reads your answer aloud. \
Use only the facts in the JSON you are given; never invent form, history or people. \
Write at most three short sentences, under 70 words, of plain speech: what the race is and why it matters, then which runners bring the strongest recent form and why. \
No markdown, lists, symbols or abbreviations; say numbers as a person would. \
Never mention odds, prices, betting, wagering, tips, bookmakers or who will win. \
Describe past form only: never call a runner a chance, a contender or a favourite, or say how it might go. \
where_they_usually_settle says where runners have settled in past races; you may say that, but never predict how this race will be run.";

/// Words that mean the model has drifted into betting talk or into rating a runner's
/// prospects; the answer is dropped.
const BANNED: &[&str] = &[
    "odds",
    "bet",
    "bets",
    "betting",
    "wager",
    "wagering",
    "tip",
    "tips",
    "punt",
    "punters",
    "bookmaker",
    "bookmakers",
    "favourite",
    "favorite",
    "each-way",
    "value",
    "chance",
    "chances",
    "contender",
    "contenders",
    "likely",
];

/// Phrases that predict how a race will go. The pace facts the model is given describe past
/// runs only, and so must its answer.
const BANNED_PHRASES: &[&str] = &[
    "will lead",
    "should lead",
    "will win",
    "should win",
    "likely to",
    "expect",
    "hard to beat",
    "hard to run down",
    "the one to beat",
];

pub struct Summariser {
    client: aws_sdk_bedrockruntime::Client,
    model: String,
    timeout: Duration,
}

impl Summariser {
    /// `TRACKSIDE_BEDROCK_MODEL` names the model or inference profile, e.g.
    /// `au.anthropic.claude-haiku-4-5-20251001-v1:0`. Unset means no Bedrock.
    pub async fn from_env() -> Option<Self> {
        let model = std::env::var("TRACKSIDE_BEDROCK_MODEL")
            .ok()
            .filter(|m| !m.is_empty())?;
        let config = aws_config::load_from_env().await;
        tracing::info!(%model, "Bedrock race explanations on");
        Some(Self {
            client: aws_sdk_bedrockruntime::Client::new(&config),
            model,
            // Voice has little patience; the template answer is always ready.
            timeout: Duration::from_secs(6),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Two or three spoken sentences from `facts`, or an error when Bedrock is slow, fails or
    /// answers with betting language.
    pub async fn explain(&self, facts: &serde_json::Value) -> Result<String> {
        let prompt = format!(
            "Explain this race for a racing newcomer.\n\n{}",
            serde_json::to_string_pretty(facts)?
        );
        let call = self
            .client
            .converse()
            .model_id(&self.model)
            .system(SystemContentBlock::Text(SYSTEM.into()))
            .messages(
                Message::builder()
                    .role(ConversationRole::User)
                    .content(ContentBlock::Text(prompt))
                    .build()?,
            )
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(300)
                    .temperature(0.3)
                    .build(),
            )
            .send();
        let out = tokio::time::timeout(self.timeout, call)
            .await
            .context("Bedrock took too long")??;
        let Some(ConverseOutput::Message(message)) = out.output else {
            bail!("Bedrock returned no message");
        };
        let text = message
            .content
            .iter()
            .filter_map(|b| b.as_text().ok())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        clean(&text)
    }
}

/// The model's text as one spoken paragraph, or an error when it is empty, talks betting or
/// predicts the race.
pub fn clean(text: &str) -> Result<String> {
    let spoken = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['*', '#', '`'], "");
    if spoken.is_empty() {
        bail!("empty explanation");
    }
    let lower = spoken.to_lowercase();
    if let Some(word) = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .find(|w| BANNED.contains(w))
    {
        bail!("explanation used the word {word:?}");
    }
    if let Some(phrase) = BANNED_PHRASES.iter().find(|p| lower.contains(*p)) {
        bail!("explanation used the phrase {phrase:?}");
    }
    if spoken.contains('$') {
        bail!("explanation mentioned money");
    }
    Ok(spoken)
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn keeps_plain_speech() {
        let s = clean("  The Caulfield Guineas is a **Group 1** race.\n\nWatch  Extragalactic. ")
            .unwrap();
        assert_eq!(
            s,
            "The Caulfield Guineas is a Group 1 race. Watch Extragalactic."
        );
    }

    #[test]
    fn drops_betting_talk() {
        assert!(clean("Extragalactic is the favourite.").is_err());
        assert!(clean("Good value each-way.").is_err());
        assert!(clean("Worth $3 million.").is_err());
        assert!(clean("").is_err());
        // Rating a runner's prospects, or saying how the race will be run, is dropped too.
        assert!(clean("Sample Stayer is a leading chance.").is_err());
        assert!(clean("Placeholder Prince will lead and should be hard to run down.").is_err());
        assert!(clean("Expect Demo Miler to settle back.").is_err());
        // "better" and "tipping point" share letters, not words.
        assert!(clean("It raced better on a softer track.").is_ok());
        // Past runs may be described.
        assert!(
            clean("Placeholder Prince led at the 800 in each of its last three starts.").is_ok()
        );
    }
}
