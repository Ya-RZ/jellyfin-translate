//! DeepL API adapter (https://developers.deepl.com/docs/api-reference/translate).
//!
//! The API accepts a JSON body with a `text` array and returns one entry in
//! `translations` per input. Free and Pro accounts use different hosts; the
//! public constructor picks the right one from the key suffix (`:fx` = free).

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::{Limits, TranslateRequest, TranslateResponse, TranslatedSegment, Translator, Usage};

const FREE_BASE_URL: &str = "https://api-free.deepl.com";
const PRO_BASE_URL: &str = "https://api.deepl.com";
const API_PATH: &str = "/v2/translate";
/// DeepL documents a 128 KiB request body limit. 5000 characters is a
/// deliberately conservative budget for the same reason as Baidu: CJK text is
/// 3 bytes per character, and callers may keep segment overhead too.
const MAX_BATCH_CHARS: usize = 5_000;
/// DeepL accepts at most 50 `text` parameters per request.
const MAX_TEXTS_PER_REQUEST: usize = 50;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct DeepLTranslator {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl DeepLTranslator {
    pub fn new(api_key: String) -> Result<Self> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            bail!("deepl: API key must not be empty");
        }
        let base_url = if api_key.ends_with(":fx") {
            FREE_BASE_URL
        } else {
            PRO_BASE_URL
        };
        Self::with_base_url(api_key, base_url)
    }

    /// Builds an adapter pointed at an alternate host. Used by tests; the
    /// public constructor selects the Free/Pro host from the key suffix.
    fn with_base_url(api_key: String, base_url: impl Into<String>) -> Result<Self> {
        let api_key = api_key.trim().to_owned();
        if api_key.is_empty() {
            bail!("deepl: API key must not be empty");
        }
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            bail!("deepl: base URL must not be empty");
        }

        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("deepl: failed to build HTTP client")?;

        Ok(Self {
            api_key,
            base_url,
            http,
        })
    }

    async fn call_api(
        &self,
        texts: &[&str],
        source: Option<&str>,
        target: &str,
    ) -> Result<Vec<String>> {
        let mut payload = json!({
            "text": texts,
            "target_lang": target,
        });
        if let Some(source) = source {
            payload["source_lang"] = json!(source);
        }

        let url = format!("{}{}", self.base_url, API_PATH);
        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("DeepL-Auth-Key {}", self.api_key))
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow!("deepl: request failed: {e}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("deepl: failed to read response body: {e}"))?;

        if !status.is_success() {
            let detail = serde_json::from_str::<DeepLError>(&body)
                .ok()
                .map(|error| error.message)
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| snippet(&body));
            bail!("deepl: HTTP {}: {detail}", status.as_u16());
        }

        let parsed: DeepLResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow!("deepl: invalid JSON response ({e}): {}", snippet(&body)))?;

        Ok(parsed
            .translations
            .into_iter()
            .map(|translation| translation.text)
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct DeepLResponse {
    #[serde(default)]
    translations: Vec<DeepLTranslation>,
}

#[derive(Debug, Deserialize)]
struct DeepLTranslation {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct DeepLError {
    #[serde(default)]
    message: String,
}

#[async_trait]
impl Translator for DeepLTranslator {
    fn id(&self) -> &'static str {
        "deepl"
    }

    fn limits(&self) -> Limits {
        Limits {
            max_batch_chars: MAX_BATCH_CHARS,
            qps: 5.0,
            max_concurrency: 2,
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

        let target = map_lang(&request.target_lang);
        if target.is_empty() || target == "AUTO" {
            bail!("deepl: target language must not be empty or \"auto\"");
        }
        let source = if request.source_lang.trim().is_empty()
            || request.source_lang.eq_ignore_ascii_case("auto")
        {
            None
        } else {
            Some(map_lang(&request.source_lang))
        };
        // DeepL rejects an empty source_lang; an explicit "auto" is not needed.
        let source = source.filter(|code| !code.is_empty() && code != "AUTO");

        let mut translated: Vec<Option<String>> = vec![None; request.segments.len()];
        for (index, segment) in request.segments.iter().enumerate() {
            if segment.text.is_empty() {
                translated[index] = Some(String::new());
            }
        }

        for batch in plan_batches(&request.segments) {
            let texts = batch
                .iter()
                .map(|&index| request.segments[index].text.as_str())
                .collect::<Vec<_>>();
            let translations = self.call_api(&texts, source.as_deref(), &target).await?;
            if translations.len() != texts.len() {
                bail!(
                    "deepl: expected {} translation(s), got {}",
                    texts.len(),
                    translations.len()
                );
            }
            for (index, text) in batch.into_iter().zip(translations) {
                translated[index] = Some(text);
            }
        }

        let mut segments = Vec::with_capacity(request.segments.len());
        for (index, segment) in request.segments.iter().enumerate() {
            let text = translated[index]
                .take()
                .ok_or_else(|| anyhow!("deepl: missing translation for segment {}", segment.id))?;
            segments.push(TranslatedSegment {
                id: segment.id.clone(),
                text,
            });
        }

        Ok(TranslateResponse {
            segments,
            usage: Usage {
                chars,
                tokens: None,
            },
        })
    }
}

/// DeepL expects upper-case language codes; region variants that have dedicated
/// target codes are mapped explicitly and everything else is upper-cased.
fn map_lang(code: &str) -> String {
    let normalized = code.trim().replace('_', "-").to_ascii_uppercase();
    match normalized.as_str() {
        "ZH-CN" | "ZH-HANS" | "ZH-SG" => "ZH".to_owned(),
        "ZH-TW" | "ZH-HK" | "ZH-HANT" | "ZH-MO" => "ZH-HANT".to_owned(),
        "PT-BR" | "PT-PT" | "EN-GB" | "EN-US" => normalized,
        _ => normalized,
    }
}

/// Groups non-empty segments into requests capped by both the character budget
/// and DeepL's 50-texts-per-request limit.
fn plan_batches(segments: &[crate::Segment]) -> Vec<Vec<usize>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut chars = 0usize;

    for (index, segment) in segments.iter().enumerate() {
        if segment.text.is_empty() {
            continue;
        }
        let length = segment.text.chars().count();
        if !current.is_empty()
            && (chars + length > MAX_BATCH_CHARS || current.len() >= MAX_TEXTS_PER_REQUEST)
        {
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
    use crate::Segment;
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
    async fn translates_text_array() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path(API_PATH)
                .header("authorization", "DeepL-Auth-Key test-key:fx")
                .json_body(json!({
                    "text": ["Hello", "World"],
                    "target_lang": "ZH",
                    "source_lang": "EN"
                }));
            then.status(200).json_body(json!({
                "translations": [
                    { "detected_source_language": "EN", "text": "你好" },
                    { "detected_source_language": "EN", "text": "世界" }
                ]
            }));
        });

        let translator =
            DeepLTranslator::with_base_url("test-key:fx".to_owned(), server.base_url()).unwrap();

        let empty = translator
            .translate(request(Vec::new()))
            .await
            .expect("empty request must not call the API");
        assert!(empty.segments.is_empty());
        assert_eq!(empty.usage.chars, 0);

        let response = translator.translate(request(segments())).await.unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments.len(), 2);
        assert_eq!(response.segments[0].id, "s1");
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].id, "s2");
        assert_eq!(response.segments[1].text, "世界");
        assert_eq!(response.usage.chars, 10);
        assert_eq!(response.usage.tokens, None);
    }

    #[tokio::test]
    async fn reports_http_status_and_message() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path(API_PATH);
            then.status(456)
                .json_body(json!({ "message": "Quota exceeded" }));
        });

        let translator =
            DeepLTranslator::with_base_url("test-key:fx".to_owned(), server.base_url()).unwrap();
        let error = translator
            .translate(request(segments()))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(1);
        assert!(error.contains("deepl"), "unexpected error: {error}");
        assert!(error.contains("456"), "unexpected error: {error}");
        assert!(
            error.contains("Quota exceeded"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn rejects_translation_count_mismatch() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path(API_PATH);
            then.status(200).json_body(json!({
                "translations": [{ "detected_source_language": "EN", "text": "你好" }]
            }));
        });

        let translator =
            DeepLTranslator::with_base_url("test-key:fx".to_owned(), server.base_url()).unwrap();
        let error = translator
            .translate(request(segments()))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(1);
        assert!(error.contains("deepl"), "unexpected error: {error}");
        assert!(error.contains("expected 2"), "unexpected error: {error}");
    }
}
