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
    "each with an \"id\" and a \"text\". Translate every `text` into the target ",
    "language. Never echo the input unchanged: every segment must receive an actual ",
    "translation, including tags, short labels and proper nouns. Render proper nouns ",
    "and brand names following the conventions of the target language. Respond with ",
    "ONLY a JSON object of the form ",
    "{\"segments\":[{\"id\":\"<same id>\",\"text\":\"<translation>\"}]} containing one ",
    "entry per input segment, in the same order. Do not add commentary, explanations ",
    "or markdown fences."
);

/// Appended to the system prompt for one retry when the model echoed the input
/// instead of translating it.
const ECHO_RETRY_INSTRUCTION: &str = concat!(
    "Your previous answer copied the input text without translating it. That is not ",
    "acceptable. Translate every `text` now: do NOT echo the input, even for tags, ",
    "short labels or names. Only leave a proper noun unchanged when that is the ",
    "correct form in the target language."
);

/// HTTP-level failure carrying its status code so the retry loop can tell
/// permanent client errors (4xx other than 429) from transient failures.
#[derive(Debug)]
struct HttpError {
    status: reqwest::StatusCode,
    message: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "openai: HTTP {}: {}", self.status.as_u16(), self.message)
    }
}

impl std::error::Error for HttpError {}

/// Whether a failed request is worth retrying. HTTP 4xx (except 429) is a
/// permanent client error — retrying cannot help; everything else (transport
/// errors, 429 rate limiting, 5xx, malformed content) is worth one retry.
fn is_retryable(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<HttpError>() {
        Some(http) => {
            http.status.is_server_error()
                || http.status == reqwest::StatusCode::TOO_MANY_REQUESTS
        }
        None => true,
    }
}

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
        system_content: &str,
    ) -> Result<(Vec<String>, Option<u64>)> {
        let payload_segments = segments
            .iter()
            .map(|segment| json!({ "id": segment.id, "text": segment.text }))
            .collect::<Vec<_>>();
        let user_content = serde_json::to_string(&payload_segments)
            .context("openai: failed to serialize segments")?;

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
            return Err(HttpError {
                status,
                message: snippet(&raw),
            }
            .into());
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

        let texts = parse_content(&content, segments)?;
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
        self.translate_with_prompt(request, None).await
    }
}

impl OpenAiTranslator {
    /// Translate with extra field-specific instructions (e.g. a per-field prompt
    /// template from the app settings).
    ///
    /// `instructions` is prepended to the fixed system prompt; the JSON segment
    /// contract stays last so the model always returns parsable segment JSON.
    pub async fn translate_with_prompt(
        &self,
        request: TranslateRequest,
        instructions: Option<&str>,
    ) -> Result<TranslateResponse> {
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

        let system_content = build_system_prompt(source_lang, target_lang, instructions);
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
            let mut error_attempts = 0usize;
            let mut echo_retried = false;
            let mut attempt_system = system_content.clone();

            while error_attempts < MAX_ATTEMPTS {
                match self.translate_once(&batch_segments, &attempt_system).await {
                    Ok((texts, tokens)) => {
                        if !echo_retried && is_echo(&batch_segments, &texts) {
                            echo_retried = true;
                            tracing::warn!(
                                provider = "openai",
                                "model echoed the input unchanged; retrying once with a stronger instruction"
                            );
                            attempt_system =
                                format!("{system_content}\n\n{ECHO_RETRY_INSTRUCTION}");
                            continue;
                        }
                        outcome = Some((texts, tokens));
                        break;
                    }
                    Err(error) => {
                        // Permanent client errors (4xx other than 429) are not
                        // worth retrying: fail fast so a config mistake surfaces
                        // immediately instead of after a pointless extra request.
                        if !is_retryable(&error) {
                            return Err(error);
                        }
                        error_attempts += 1;
                        tracing::warn!(
                            provider = "openai",
                            attempt = error_attempts,
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

/// Field-specific instructions come first, the machine-readable segment
/// contract last (a prompt template that says "only output the translation"
/// must not override the JSON contract).
fn build_system_prompt(source_lang: &str, target_lang: &str, instructions: Option<&str>) -> String {
    let mut prompt = String::new();
    if let Some(extra) = instructions {
        let extra = extra.trim();
        if !extra.is_empty() {
            prompt.push_str(extra);
            prompt.push_str(
                "\n\nRegardless of the wording above, always answer with the JSON object \
                 described below.\n\n",
            );
        }
    }
    prompt.push_str(SYSTEM_PROMPT);
    prompt.push_str(&format!(
        "\nSource language: {source_lang}\nTarget language: {target_lang}"
    ));
    prompt
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

/// True when the model returned the exact input for every segment in the batch.
fn is_echo(segments: &[&Segment], texts: &[String]) -> bool {
    segments.len() == texts.len()
        && segments
            .iter()
            .zip(texts)
            .all(|(segment, text)| segment.text == *text)
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

    // A top-level JSON array (or object) is already complete; slicing it down to
    // its first/last braces would corrupt arrays like [{"id":..,"text":..}].
    if serde_json::from_str::<Value>(unwrapped).is_ok() {
        return unwrapped;
    }

    let start = [unwrapped.find('{'), unwrapped.find('[')]
        .into_iter()
        .flatten()
        .min();
    let end = [unwrapped.rfind('}'), unwrapped.rfind(']')]
        .into_iter()
        .flatten()
        .max();
    match (start, end) {
        (Some(start), Some(end)) if end > start => {
            let candidate = &unwrapped[start..=end];
            if serde_json::from_str::<Value>(candidate).is_ok() {
                return candidate;
            }
            unwrapped
        }
        _ => unwrapped,
    }
}

/// Parses the assistant message into translations, tolerating the shapes models
/// actually emit in the wild. Field-specific prompt templates and some gateways
/// ignore the requested JSON contract, so the accepted forms are, in order:
///
/// 1. the canonical `{"segments":[{"id":"..","text":".."}]}` payload;
/// 2. a top-level array of segment objects (`[{"id":"..","text":".."}]`);
/// 3. a bare JSON string (`"你好"`);
/// 4. an object carrying `text` / `translation` / `content` / `result` (or
///    `segments`) as a string or as an array of strings;
/// 5. a top-level array of strings (aligned by position);
/// 6. for a single input only, the whole cleaned content as plain text.
fn parse_content(content: &str, expected: &[&Segment]) -> Result<Vec<String>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(unparsable(content, expected.len()));
    }

    let json_text = extract_json(trimmed);

    // 1) Canonical segment payload.
    if let Ok(payload) = serde_json::from_str::<SegmentPayload>(json_text) {
        if !payload.segments.is_empty() {
            match align(expected, payload.segments) {
                Ok(texts) => return Ok(texts),
                Err(error) => tracing::debug!(
                    provider = "openai",
                    %error,
                    "segment payload did not align; trying tolerant shapes"
                ),
            }
        }
    }

    // 2) Top-level array of segment objects, e.g. the model echoed the input as
    //    [{"id":"tag:0","text":"prestigepremium"}, ...]. IDs may be strings or
    //    numbers; missing IDs fall back to positional order.
    if let Ok(raw) = serde_json::from_str::<Vec<RawSegment>>(json_text) {
        if !raw.is_empty() {
            match align(expected, raw) {
                Ok(texts) => return Ok(texts),
                Err(error) => tracing::debug!(
                    provider = "openai",
                    %error,
                    "top-level segment array did not align; trying tolerant shapes"
                ),
            }
        }
    }

    // 3-5) JSON string, object with a text-like field, or array of strings.
    if let Ok(value) = serde_json::from_str::<Value>(json_text) {
        if let Some(texts) = texts_from_value(&value, expected.len()) {
            return Ok(texts);
        }
        return Err(unparsable(trimmed, expected.len()));
    }

    // 6) Not JSON at all: with a single input the cleaned content is the
    //    translation (models often answer with just the translated string).
    if expected.len() == 1 {
        let cleaned = clean_plain_text(trimmed);
        if !cleaned.is_empty() {
            return Ok(vec![cleaned]);
        }
    }

    Err(unparsable(trimmed, expected.len()))
}

/// Extracts translations from a decoded JSON value, accepting string fields as
/// well as arrays of strings (or arrays of objects with a text-like field).
fn texts_from_value(value: &Value, expected_len: usize) -> Option<Vec<String>> {
    match value {
        Value::String(text) if expected_len == 1 => Some(vec![text.clone()]),
        Value::Array(items) if items.len() == expected_len => collect_texts(items),
        Value::Object(map) => {
            for key in ["text", "translation", "content", "result", "segments"] {
                let Some(inner) = map.get(key) else {
                    continue;
                };
                match inner {
                    Value::String(text) if expected_len == 1 => return Some(vec![text.clone()]),
                    Value::Array(items) if items.len() == expected_len => {
                        if let Some(texts) = collect_texts(items) {
                            return Some(texts);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

fn collect_texts(items: &[Value]) -> Option<Vec<String>> {
    items
        .iter()
        .map(|item| match item {
            Value::String(text) => Some(text.clone()),
            Value::Object(map) => ["text", "translation", "content", "result"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .map(str::to_owned),
            _ => None,
        })
        .collect()
}

fn unparsable(content: &str, expected_len: usize) -> anyhow::Error {
    anyhow!(
        "openai: 翻译服务返回了无法解析的内容：{}（期望 {expected_len} 段译文）",
        snippet(content)
    )
}

/// Strips markdown code fences and wrapping quotes from a plain-text answer.
fn clean_plain_text(content: &str) -> String {
    let trimmed = content.trim();
    let without_fence = if let Some(rest) = trimmed.strip_prefix("```") {
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
    strip_wrapping_quotes(without_fence).to_owned()
}

fn strip_wrapping_quotes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim();
        let mut chars = trimmed.chars();
        let (Some(first), Some(last)) = (chars.next(), chars.next_back()) else {
            return trimmed;
        };
        let matching = matches!(
            (first, last),
            ('"', '"') | ('\'', '\'') | ('“', '”') | ('‘', '’') | ('「', '」') | ('『', '』')
        );
        if matching && trimmed.len() >= first.len_utf8() + last.len_utf8() {
            text = &trimmed[first.len_utf8()..trimmed.len() - last.len_utf8()];
        } else {
            return trimmed;
        }
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

    fn single_segment_request() -> TranslateRequest {
        request(vec![Segment {
            id: "s1".to_owned(),
            text: "Hello".to_owned(),
        }])
    }

    fn translator(server: &MockServer) -> OpenAiTranslator {
        OpenAiTranslator::new(
            format!("{}/v1", server.base_url()),
            "test-key".to_owned(),
            "test-model".to_owned(),
        )
        .unwrap()
    }

    fn mock_chat<'a>(server: &'a MockServer, content: &str) -> httpmock::Mock<'a> {
        server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{ "message": { "role": "assistant", "content": content } }]
            }));
        })
    }

    fn tag_segments() -> Vec<Segment> {
        vec![
            Segment {
                id: "tag:0".to_owned(),
                text: "prestigepremium".to_owned(),
            },
            Segment {
                id: "tag:1".to_owned(),
                text: "premium".to_owned(),
            },
            Segment {
                id: "tag:2".to_owned(),
                text: "prestige".to_owned(),
            },
        ]
    }

    const ECHO_RETRY_MARKER: &str = "previous answer copied the input text";

    fn request_body(request: &HttpMockRequest) -> String {
        String::from_utf8_lossy(request.body.as_deref().unwrap_or_default()).into_owned()
    }

    fn first_attempt(request: &HttpMockRequest) -> bool {
        !request_body(request).contains(ECHO_RETRY_MARKER)
    }

    fn echo_retry_attempt(request: &HttpMockRequest) -> bool {
        request_body(request).contains(ECHO_RETRY_MARKER)
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
    async fn multi_segment_unparsable_content_reports_chinese_error() {
        let server = MockServer::start();
        let mock = mock_chat(&server, "I cannot help with that.");

        let error = translator(&server)
            .translate(request(segments()))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(2);
        assert!(error.contains("openai"), "unexpected error: {error}");
        assert!(
            error.contains("翻译服务返回了无法解析的内容"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("I cannot help with that."),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn accepts_plain_text_for_single_segment() {
        let server = MockServer::start();
        let mock = mock_chat(&server, "  你好  ");

        let response = translator(&server)
            .translate(single_segment_request())
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].id, "s1");
        assert_eq!(response.segments[0].text, "你好");
    }

    #[tokio::test]
    async fn accepts_text_field_object_for_single_segment() {
        let server = MockServer::start();
        let mock = mock_chat(&server, r#"{"text":"你好"}"#);

        let response = translator(&server)
            .translate(single_segment_request())
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].text, "你好");
    }

    #[tokio::test]
    async fn accepts_string_array_by_position() {
        let server = MockServer::start();
        let mock = mock_chat(&server, r#"["你好","世界"]"#);

        let response = translator(&server)
            .translate(request(segments()))
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].text, "世界");
    }

    #[tokio::test]
    async fn accepts_fenced_segment_json_with_surrounding_noise() {
        let server = MockServer::start();
        let mock = mock_chat(
            &server,
            "Sure, here it is:\n```json\n{\"segments\":[{\"id\":\"s1\",\"text\":\"你好\"}]}\n```",
        );

        let response = translator(&server)
            .translate(single_segment_request())
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].text, "你好");
    }

    #[tokio::test]
    async fn falls_back_to_positional_order_when_ids_are_missing() {
        let server = MockServer::start();
        let mock = mock_chat(&server, r#"{"segments":[{"text":"你好"},{"text":"世界"}]}"#);

        let response = translator(&server)
            .translate(request(segments()))
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].id, "s1");
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].id, "s2");
        assert_eq!(response.segments[1].text, "世界");
    }

    #[tokio::test]
    async fn accepts_top_level_object_array_aligned_by_id() {
        let server = MockServer::start();
        let mock = mock_chat(
            &server,
            r#"[{"id":"tag:2","text":"声望"},{"id":"tag:0","text":"名牌溢价"},{"id":"tag:1","text":"溢价"}]"#,
        );

        let response = translator(&server)
            .translate(request(tag_segments()))
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments.len(), 3);
        assert_eq!(response.segments[0].id, "tag:0");
        assert_eq!(response.segments[0].text, "名牌溢价");
        assert_eq!(response.segments[1].id, "tag:1");
        assert_eq!(response.segments[1].text, "溢价");
        assert_eq!(response.segments[2].id, "tag:2");
        assert_eq!(response.segments[2].text, "声望");
    }

    #[tokio::test]
    async fn accepts_top_level_object_array_without_ids_by_position() {
        let server = MockServer::start();
        let mock = mock_chat(&server, r#"[{"text":"你好"},{"text":"世界"}]"#);

        let response = translator(&server)
            .translate(request(segments()))
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].id, "s1");
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].id, "s2");
        assert_eq!(response.segments[1].text, "世界");
    }

    #[tokio::test]
    async fn retries_once_with_stronger_instruction_when_model_echoes() {
        let server = MockServer::start();
        let echo = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .matches(first_attempt);
            then.status(200).json_body(json!({
                "choices": [{ "message": {
                    "content": r#"[{"id":"tag:0","text":"prestigepremium"},{"id":"tag:1","text":"premium"},{"id":"tag:2","text":"prestige"}]"#
                }}]
            }));
        });
        let retry = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .matches(echo_retry_attempt);
            then.status(200).json_body(json!({
                "choices": [{ "message": {
                    "content": r#"[{"id":"tag:0","text":"名牌溢价"},{"id":"tag:1","text":"溢价"},{"id":"tag:2","text":"声望"}]"#
                }}]
            }));
        });

        let response = translator(&server)
            .translate(request(tag_segments()))
            .await
            .unwrap();

        echo.assert_hits(1);
        retry.assert_hits(1);
        assert_eq!(response.segments[0].text, "名牌溢价");
        assert_eq!(response.segments[1].text, "溢价");
        assert_eq!(response.segments[2].text, "声望");
    }

    #[tokio::test]
    async fn returns_echoed_text_when_retry_also_echoes() {
        let server = MockServer::start();
        let mock = mock_chat(
            &server,
            r#"{"segments":[{"id":"s1","text":"prestigepremium"}]}"#,
        );

        let response = translator(&server)
            .translate(request(vec![Segment {
                id: "s1".to_owned(),
                text: "prestigepremium".to_owned(),
            }]))
            .await
            .expect("an echoed batch must be returned instead of failing");

        mock.assert_hits(2);
        assert_eq!(response.segments.len(), 1);
        assert_eq!(response.segments[0].text, "prestigepremium");
    }

    #[tokio::test]
    async fn prepends_field_instructions_to_the_system_prompt() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/chat/completions")
                .body_contains("FIELD-INSTRUCTION: translate titles")
                .body_contains("translation engine");
            then.status(200).json_body(json!({
                "choices": [{
                    "message": {
                        "content": "{\"segments\":[{\"id\":\"s1\",\"text\":\"你好\"}]}"
                    }
                }]
            }));
        });

        let translator = OpenAiTranslator::new(
            format!("{}/v1", server.base_url()),
            "test-key".to_owned(),
            "test-model".to_owned(),
        )
        .unwrap();
        let response = translator
            .translate_with_prompt(
                request(vec![Segment {
                    id: "s1".to_owned(),
                    text: "Hello".to_owned(),
                }]),
                Some("FIELD-INSTRUCTION: translate titles"),
            )
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments[0].text, "你好");
    }
}
