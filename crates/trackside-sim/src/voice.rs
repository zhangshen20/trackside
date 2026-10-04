//! Spoken answers from Amazon Polly, so the simulator sounds like a voice assistant rather
//! than a browser's built-in speech. Olivia is Polly's Australian English voice; the
//! generative engine sounds most natural, and the neural one is the fallback.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use aws_sdk_polly::types::{Engine, OutputFormat, TextType};

/// Longest answer Polly is asked to read. Answers are a few sentences; this bounds a bad one.
const MAX_CHARS: usize = 1500;

pub struct Voice {
    client: aws_sdk_polly::Client,
    voice_id: String,
}

impl Voice {
    /// `None` when `TRACKSIDE_SIM_VOICE` is `off`; otherwise that voice, default `Olivia`.
    pub fn from_env(aws: &aws_config::SdkConfig, voice: Option<String>) -> Option<Self> {
        let voice_id = voice.unwrap_or_else(|| "Olivia".into());
        if voice_id.eq_ignore_ascii_case("off") {
            return None;
        }
        Some(Self {
            client: aws_sdk_polly::Client::new(aws),
            voice_id,
        })
    }

    /// MP3 audio of `text`, generative engine first, neural if that fails.
    pub async fn speak(&self, text: &str) -> Result<Vec<u8>> {
        let text = speakable(text)?;
        match self.synthesize(&text, Engine::Generative).await {
            Ok(mp3) => Ok(mp3),
            Err(e) => {
                tracing::info!(error = %format!("{e:#}"), "generative voice unavailable; using neural");
                self.synthesize(&text, Engine::Neural).await
            }
        }
    }

    async fn synthesize(&self, text: &str, engine: Engine) -> Result<Vec<u8>> {
        let call = self
            .client
            .synthesize_speech()
            .engine(engine)
            .voice_id(self.voice_id.as_str().into())
            .language_code("en-AU".into())
            .output_format(OutputFormat::Mp3)
            .text_type(TextType::Text)
            .text(text)
            .send();
        let out = tokio::time::timeout(Duration::from_secs(10), call)
            .await
            .context("Polly took too long")??;
        Ok(out.audio_stream.collect().await?.into_bytes().to_vec())
    }
}

/// The text Polly reads: trimmed, without markdown or emoji, and cut at a sentence end when
/// it is too long.
fn speakable(text: &str) -> Result<String> {
    let clean: String = text
        .chars()
        .filter(|c| {
            !matches!(c, '*' | '#' | '`' | '_')
                && (c.is_ascii() || c.is_alphanumeric() || " ’‘“”–—…".contains(*c))
        })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        bail!("nothing to say");
    }
    if clean.chars().count() <= MAX_CHARS {
        return Ok(clean);
    }
    let cut: String = clean.chars().take(MAX_CHARS).collect();
    Ok(match cut.rfind(". ") {
        Some(i) => cut[..=i].to_string(),
        None => cut,
    })
}

#[cfg(test)]
mod tests {
    use super::speakable;

    #[test]
    fn strips_markdown_and_emoji() {
        assert_eq!(
            speakable("Hi! 👋 **Oliveanotherday** won.").unwrap(),
            "Hi! Oliveanotherday won."
        );
        assert!(speakable("  👋 ").is_err());
    }

    #[test]
    fn long_answers_end_on_a_sentence() {
        let long = "One sentence here. ".repeat(200);
        let s = speakable(&long).unwrap();
        assert!(s.chars().count() <= 1500);
        assert!(s.ends_with('.'));
    }
}
