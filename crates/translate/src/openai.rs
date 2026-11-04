//! OpenAI-compatible chat completions adapter.
//! Works with api.openai.com and any self-hosted gateway exposing
//! `POST {base_url}/chat/completions`.
//!
//! Segments are sent as a JSON array in the user message and the model is asked
//! for `{"segments":[{"id":"..","text":".."}]}`. Failed requests (transport,
//! HTTP status, malformed content) are retried once before the error surfaces.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    Limits, Segment, TranslateRequest, TranslateResponse, TranslatedSegment, Translator, Usage,
};

const MAX_BATCH_CHARS: usize = 12_000;
const MAX_ATTEMPTS: usize = 2;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

const SYSTEM_PROMPT: &str = concat!(
    "You are a translation engine. The user message is a JSON array of segments, ",
    "each with an \"id\" (string) and a \"text\" (string). Translate every text into ",
    "the target language, preserving meaning, names and tone. Respond with ONLY a ",
    "JSON object of the form {\"segments\":[{\"id\":\"<same id>\",\"text\":\"<translation>\"}]} ",
    "containing one entry per input segment, in the same order. Do not add ",
    "commentary, explanations or markdown fences."
);

pub struct OpenAiTranslator {
    base_url: String,
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAiTranslator {
    pub fn new(base_url: String, api_key: String, model: String) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/').to_owned();
        let api_key = api_key.trim().to_owned();
        let model = model.trim().to_owned();

        if base_url.is_empty() {
            bail!("openai: base URL must not be empty");
        }
        if api_key.is_empty() {
            bail!("openai: API key must not be empty");
        }
        if model.is_empty() {
            bail!("openai: model must not be empty");
        }

        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(15))
            .build()
            .context("openai: failed to build HTTP client")?;

        Ok(Self {
            base_url,
            api_key,
            model,
            http,
        })
    }

    /// One HTTP round trip plus response parsing. The caller owns retries.
    async fn translate_once(
        &self,
        segments: &[&Segment],
        source_lang: &str,
        target_lang: &str,
    ) -> Result<(Vec<String>, Option<u64>)> {
        let payload_segments = segments
            .iter()
            .map(|segment| json!({ "id": segment.id, "text": segment.text }))
            .collect::<Vec<_>>();
        let user_content = serde_json::to_string(&payload_segments)
            .context("openai: failed to serialize segments")?;
        let system_content = format!(
            "{SYSTEM_PROMPT}\nSource language: {source_lang}\nTarget language: {target_lang}"
        );

        let body = json!({
            "model": self.model,
            "temperature": 0.0,
            "messages": [
                { "role": "system", "content": system_content },
                { "role": "user", "content": user_content },
            ],
        });

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("openai: request failed: {e}"))?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| anyhow!("openai: failed to read response body: {e}"))?;

        if !status.is_success() {
            bail!("openai: HTTP {}: {}", status.as_u16(), snippet(&raw));
        }

        let completion: ChatCompletion = serde_json::from_str(&raw)
            .map_err(|e| anyhow!("openai: invalid JSON response ({e}): {}", snippet(&raw)))?;

        let choice =
            completion.choices.into_iter().next().ok_or_else(|| {
                anyhow!("openai: response contained no choices: {}", snippet(&raw))
            })?;
        let content = choice.message.content.ok_or_else(|| {
            anyhow!(
                "openai: choice contained no message content: {}",
                snippet(&raw)
            )
        })?;

        let json_text = extract_json(&content);
        let parsed: SegmentPayload = serde_json::from_str(json_text).map_err(|e| {
            anyhow!(
                "openai: model response was not segment JSON ({e}): {}",
                snippet(&content)
            )
        })?;

        let texts = align(segments, parsed.segments)?;
        Ok((texts, token_count(completion.usage)))
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletion {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SegmentPayload {
    #[serde(default)]
    segments: Vec<RawSegment>,
}

#[derive(Debug, Deserialize)]
struct RawSegment {
    #[serde(default)]
    id: Value,
    text: String,
}

#[async_trait]
impl Translator for OpenAiTranslator {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn limits(&self) -> Limits {
        Limits {
            // Conservative prompt budget; context windows are much larger, but
            // smaller batches keep latency and truncation risk low.
            max_batch_chars: MAX_BATCH_CHARS,
            qps: 5.0,
            max_concurrency: 4,
        }
    }

    async fn translate(&self, request: TranslateRequest) -> Result<TranslateResponse> {
        if request.segments.is_empty() {
            return Ok(TranslateResponse {
                segments: Vec::new(),
                usage: Usage::default(),
            });
        }

        let chars = request
            .segments
            .iter()
            .map(|segment| segment.text.chars().count())
            .sum();

        let target_lang = request.target_lang.trim();
        if target_lang.is_empty() {
            bail!("openai: target language must not be empty");
        }
        let source_lang = if request.source_lang.trim().is_empty() {
            "auto"
        } else {
            request.source_lang.trim()
        };

        let mut translated: Vec<Option<String>> = vec![None; request.segments.len()];
        for (index, segment) in request.segments.iter().enumerate() {
            if segment.text.is_empty() {
                translated[index] = Some(String::new());
            }
        }

        let mut tokens_total: Option<u64> = None;
        for batch in plan_batches(&request.segments) {
            let batch_segments = batch
                .iter()
                .map(|&index| &request.segments[index])
                .collect::<Vec<_>>();

            let mut last_error: Option<anyhow::Error> = None;
            let mut outcome: Option<(Vec<String>, Option<u64>)> = None;
            for attempt in 0..MAX_ATTEMPTS {
                match self
                    .translate_once(&batch_segments, source_lang, target_lang)
                    .await
                {
                    Ok(value) => {
                        outcome = Some(value);
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            provider = "openai",
                            attempt = attempt + 1,
                            error = %error,
                            "translation request failed"
                        );
                        last_error = Some(error);
                    }
                }
            }

            let (texts, tokens) = outcome.ok_or_else(|| {
                last_error.unwrap_or_else(|| anyhow!("openai: request failed without an error"))
            })?;
            if let Some(tokens) = tokens {
                tokens_total = Some(tokens_total.unwrap_or(0).saturating_add(tokens));
            }

            for (index, text) in batch.into_iter().zip(texts) {
                translated[index] = Some(text);
            }
        }

        let mut segments = Vec::with_capacity(request.segments.len());
        for (index, segment) in request.segments.iter().enumerate() {
            let text = translated[index]
                .take()
                .ok_or_else(|| anyhow!("openai: missing translation for segment {}", segment.id))?;
            segments.push(TranslatedSegment {
                id: segment.id.clone(),
                text,
            });
        }

        Ok(TranslateResponse {
            segments,
            usage: Usage {
                chars,
                tokens: tokens_total,
            },
        })
    }
}

/// Splits segment indices into requests of at most [`MAX_BATCH_CHARS`] characters.
/// A single oversized segment is sent on its own rather than truncated.
fn plan_batches(segments: &[Segment]) -> Vec<Vec<usize>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut chars = 0usize;

    for (index, segment) in segments.iter().enumerate() {
        if segment.text.is_empty() {
            continue;
        }
        let length = segment.text.chars().count();
        if !current.is_empty() && chars + length > MAX_BATCH_CHARS {
            batches.push(std::mem::take(&mut current));
            chars = 0;
        }
        chars += length;
        current.push(index);
    }

    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn raw_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Matches returned segments to the requested ones by id, falling back to
/// positional order when the model echoed valid JSON but changed/lost the ids.
fn align(expected: &[&Segment], raw: Vec<RawSegment>) -> Result<Vec<String>> {
    if raw.len() != expected.len() {
        bail!(
            "openai: model returned {} segment(s) for {} input(s)",
            raw.len(),
            expected.len()
        );
    }

    let by_id: HashMap<String, String> = raw
        .iter()
        .filter_map(|segment| raw_id(&segment.id).map(|id| (id, segment.text.clone())))
        .collect();

    let mut missing = Vec::new();
    let mut ordered = Vec::with_capacity(expected.len());
    for segment in expected {
        match by_id.get(&segment.id) {
            Some(text) => ordered.push(text.clone()),
            None => missing.push(segment.id.clone()),
        }
    }
    if missing.is_empty() {
        return Ok(ordered);
    }

    tracing::debug!(
        provider = "openai",
        missing = ?missing,
        "model did not echo segment ids; falling back to positional order"
    );
    Ok(raw.into_iter().map(|segment| segment.text).collect())
}

fn token_count(usage: Option<ChatUsage>) -> Option<u64> {
    usage.and_then(|usage| {
        usage
            .total_tokens
            .or_else(|| match (usage.prompt_tokens, usage.completion_tokens) {
                (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
                _ => None,
            })
    })
}

/// Accepts plain JSON as well as common model wrappers such as ```json fences.
fn extract_json(content: &str) -> &str {
    let trimmed = content.trim();
    let unwrapped = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("JSON"))
            .unwrap_or(rest);
        let rest = rest.trim_start();
        match rest.rfind("```") {
            Some(end) => rest[..end].trim(),
            None => rest.trim(),
        }
    } else {
        trimmed
    };

    match (unwrapped.find('{'), unwrapped.rfind('}')) {
        (Some(start), Some(end)) if end > start => &unwrapped[start..=end],
        _ => unwrapped,
    }
}

fn snippet(body: &str) -> String {
    const LIMIT: usize = 300;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<empty body>".to_owned();
    }
    let mut out: String = trimmed.chars().take(LIMIT).collect();
    if trimmed.chars().count() > LIMIT {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn request(segments: Vec<Segment>) -> TranslateRequest {
        TranslateRequest {
            segments,
            source_lang: "en".to_owned(),
            target_lang: "zh".to_owned(),
            glossary: Vec::new(),
        }
    }

    fn segments() -> Vec<Segment> {
        vec![
            Segment {
                id: "s1".to_owned(),
                text: "Hello".to_owned(),
            },
            Segment {
                id: "s2".to_owned(),
                text: "World".to_owned(),
            },
        ]
    }

    #[tokio::test]
    async fn translates_segments_and_reads_usage() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .body_contains("\"model\":\"test-model\"")
                .body_contains(r#"\"id\":\"s1\",\"text\":\"Hello\""#);
            then.status(200).json_body(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "```json\n{\"segments\":[{\"id\":\"s1\",\"text\":\"你好\"},{\"id\":\"s2\",\"text\":\"世界\"}]}\n```"
                    }
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            }));
        });

        let translator = OpenAiTranslator::new(
            format!("{}/v1", server.base_url()),
            "test-key".to_owned(),
            "test-model".to_owned(),
        )
        .unwrap();

        let empty = translator
            .translate(request(Vec::new()))
            .await
            .expect("empty request must not call the API");
        assert!(empty.segments.is_empty());
        assert_eq!(empty.usage.tokens, None);

        let response = translator.translate(request(segments())).await.unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments.len(), 2);
        assert_eq!(response.segments[0].id, "s1");
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].id, "s2");
        assert_eq!(response.segments[1].text, "世界");
        assert_eq!(response.usage.chars, 10);
        assert_eq!(response.usage.tokens, Some(15));
    }

    #[tokio::test]
    async fn retries_once_then_reports_http_error() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(500).body("upstream boom");
        });

        let translator = OpenAiTranslator::new(
            format!("{}/v1", server.base_url()),
            "test-key".to_owned(),
            "test-model".to_owned(),
        )
        .unwrap();
        let error = translator
            .translate(request(segments()))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(2);
        assert!(error.contains("openai"), "unexpected error: {error}");
        assert!(error.contains("500"), "unexpected error: {error}");
        assert!(error.contains("upstream boom"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn retries_once_then_reports_malformed_content() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{ "message": { "role": "assistant", "content": "I cannot help with that." } }]
            }));
        });

        let translator = OpenAiTranslator::new(
            format!("{}/v1", server.base_url()),
            "test-key".to_owned(),
            "test-model".to_owned(),
        )
        .unwrap();
        let error = translator
            .translate(request(segments()))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(2);
        assert!(error.contains("openai"), "unexpected error: {error}");
        assert!(error.contains("segment JSON"), "unexpected error: {error}");
    }
}
