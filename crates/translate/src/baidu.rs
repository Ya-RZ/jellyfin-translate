//! Baidu Translate (通用文本翻译) adapter.
//! Endpoint: https://fanyi-api.baidu.com/api/trans/vip/translate
//! Signature: md5(appid + q + salt + secret)
//!
//! The API accepts multiple lines in a single `q` value and returns one
//! `trans_result` entry per line, so a batch is sent as a newline-joined query.
//! Each API call is capped at 6000 UTF-8 bytes for `q`; this adapter additionally
//! caps a request at [`MAX_BATCH_CHARS`] characters and splits oversized segments
//! before batching them.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::Value;

use crate::{Limits, TranslateRequest, TranslateResponse, TranslatedSegment, Translator, Usage};

const DEFAULT_BASE_URL: &str = "https://fanyi-api.baidu.com";
const API_PATH: &str = "/api/trans/vip/translate";
/// Documented cap for the `q` parameter is 6000 bytes (UTF-8).
const MAX_BATCH_BYTES: usize = 6_000;
/// Conservative character cap: 3 bytes per CJK character keeps us under
/// `MAX_BATCH_BYTES` for the common case, while [`split_text`] still enforces
/// the byte limit for 4-byte characters.
const MAX_BATCH_CHARS: usize = 2_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct BaiduTranslator {
    appid: String,
    secret: String,
    base_url: String,
    http: reqwest::Client,
}

impl BaiduTranslator {
    pub fn new(appid: String, secret: String) -> Result<Self> {
        Self::with_base_url(appid, secret, DEFAULT_BASE_URL)
    }

    /// Builds an adapter pointed at an alternate host. Used by tests; the
    /// public constructor always targets the production endpoint.
    fn with_base_url(appid: String, secret: String, base_url: impl Into<String>) -> Result<Self> {
        let appid = appid.trim().to_owned();
        let secret = secret.trim().to_owned();
        let base_url = base_url.into().trim_end_matches('/').to_owned();

        if appid.is_empty() {
            bail!("baidu: appid must not be empty");
        }
        if secret.is_empty() {
            bail!("baidu: secret must not be empty");
        }
        if base_url.is_empty() {
            bail!("baidu: base URL must not be empty");
        }

        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("baidu: failed to build HTTP client")?;

        Ok(Self {
            appid,
            secret,
            base_url,
            http,
        })
    }

    async fn call_api(&self, q: &str, from: &str, to: &str) -> Result<Vec<String>> {
        let salt = next_salt();
        let sign = format!(
            "{:x}",
            Md5::digest(format!("{}{}{}{}", self.appid, q, salt, self.secret).as_bytes())
        );
        let params = [
            ("q", q),
            ("from", from),
            ("to", to),
            ("appid", self.appid.as_str()),
            ("salt", salt.as_str()),
            ("sign", sign.as_str()),
        ];

        let url = format!("{}{}", self.base_url, API_PATH);
        let response = self
            .http
            .post(&url)
            .form(&params)
            .send()
            .await
            .map_err(|e| anyhow!("baidu: request failed: {e}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| anyhow!("baidu: failed to read response body: {e}"))?;

        if !status.is_success() {
            bail!("baidu: HTTP {}: {}", status.as_u16(), snippet(&body));
        }

        let parsed: BaiduResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow!("baidu: invalid JSON response ({e}): {}", snippet(&body)))?;

        if let Some(code) = parsed.error_code {
            let code = match code {
                Value::String(s) => s,
                other => other.to_string(),
            };
            let message = parsed
                .error_msg
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| snippet(&body));
            bail!("baidu: API error {code}: {message}");
        }

        Ok(parsed
            .trans_result
            .into_iter()
            .map(|item| item.dst)
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct BaiduResponse {
    #[serde(default)]
    trans_result: Vec<BaiduTransItem>,
    #[serde(default)]
    error_code: Option<Value>,
    #[serde(default)]
    error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BaiduTransItem {
    #[serde(default)]
    dst: String,
}

#[async_trait]
impl Translator for BaiduTranslator {
    fn id(&self) -> &'static str {
        "baidu"
    }

    fn limits(&self) -> Limits {
        Limits {
            max_batch_chars: MAX_BATCH_CHARS,
            // Standard tier is limited to 1 request per second.
            qps: 1.0,
            max_concurrency: 1,
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

        let from = if request.source_lang.trim().is_empty()
            || request.source_lang.eq_ignore_ascii_case("auto")
        {
            "auto".to_owned()
        } else {
            map_lang(&request.source_lang)
        };
        let to = map_lang(&request.target_lang);
        if to.is_empty() || to == "auto" {
            bail!("baidu: target language must not be empty or \"auto\"");
        }

        // The API is line oriented: newline is the batch separator, so embedded
        // newlines are flattened and each line is mapped back to its segment id.
        let mut pieces: Vec<(usize, String)> = Vec::new();
        for (index, segment) in request.segments.iter().enumerate() {
            let single_line = flatten(&segment.text);
            if single_line.is_empty() {
                continue;
            }
            for piece in split_text(&single_line) {
                pieces.push((index, piece));
            }
        }

        let mut translated_parts: Vec<Vec<String>> = vec![Vec::new(); request.segments.len()];
        for batch in make_batches(pieces) {
            let q = batch
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let lines = self.call_api(&q, &from, &to).await?;
            if lines.len() != batch.len() {
                bail!(
                    "baidu: expected {} translated line(s) for a {} character request, got {}",
                    batch.len(),
                    q.chars().count(),
                    lines.len()
                );
            }
            for ((index, _), line) in batch.into_iter().zip(lines) {
                translated_parts[index].push(line);
            }
        }

        let segments = request
            .segments
            .iter()
            .enumerate()
            .map(|(index, segment)| TranslatedSegment {
                id: segment.id.clone(),
                text: translated_parts[index].join(""),
            })
            .collect();

        Ok(TranslateResponse {
            segments,
            usage: Usage {
                chars,
                tokens: None,
            },
        })
    }
}

/// Maps common ISO language codes to the values accepted by Baidu. Unknown
/// values are passed through unchanged so callers can use Baidu-specific codes.
fn map_lang(code: &str) -> String {
    let normalized = code.trim().replace('_', "-").to_ascii_lowercase();
    match normalized.as_str() {
        "zh" | "zh-cn" | "zh-hans" | "zh-sg" => "zh",
        "zh-tw" | "zh-hk" | "zh-hant" | "zh-mo" => "cht",
        "ja" | "jp" => "jp",
        "ko" | "kr" => "kor",
        "fr" => "fra",
        "es" => "spa",
        "ar" => "ara",
        "vi" => "vie",
        other => other,
    }
    .to_owned()
}

/// Flattens embedded newlines so they cannot be mistaken for batch separators.
fn flatten(text: &str) -> String {
    text.replace("\r\n", " ")
        .replace('\n', " ")
        .replace('\r', " ")
}

/// Splits a single segment that exceeds one request's limits into byte-safe
/// chunks. Translations of the chunks are concatenated back under the same id.
fn split_text(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut chars = 0usize;
    let mut bytes = 0usize;

    for ch in text.chars() {
        let width = ch.len_utf8();
        if !current.is_empty() && (chars + 1 > MAX_BATCH_CHARS || bytes + width > MAX_BATCH_BYTES) {
            pieces.push(std::mem::take(&mut current));
            chars = 0;
            bytes = 0;
        }
        current.push(ch);
        chars += 1;
        bytes += width;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// Groups one-line pieces into requests respecting both the character and byte
/// limits, counting the newline separators.
fn make_batches(pieces: Vec<(usize, String)>) -> Vec<Vec<(usize, String)>> {
    let mut batches: Vec<Vec<(usize, String)>> = Vec::new();
    let mut current: Vec<(usize, String)> = Vec::new();
    let mut chars = 0usize;
    let mut bytes = 0usize;

    for piece in pieces {
        let piece_chars = piece.1.chars().count();
        let piece_bytes = piece.1.len();
        let separator = usize::from(!current.is_empty());

        if !current.is_empty()
            && (chars + separator + piece_chars > MAX_BATCH_CHARS
                || bytes + separator + piece_bytes > MAX_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut current));
            chars = 0;
            bytes = 0;
        }

        if !current.is_empty() {
            chars += 1;
            bytes += 1;
        }
        chars += piece_chars;
        bytes += piece_bytes;
        current.push(piece);
    }

    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn next_salt() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    nanos.to_string()
}

/// Keeps error messages readable while still carrying a slice of the payload.
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
    use serde_json::json;

    fn uri_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'%' if i + 2 < bytes.len() => {
                    let hex = &input[i + 1..i + 3];
                    match u8::from_str_radix(hex, 16) {
                        Ok(byte) => {
                            out.push(byte);
                            i += 3;
                        }
                        Err(_) => {
                            out.push(b'%');
                            i += 1;
                        }
                    }
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                byte => {
                    out.push(byte);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn form_value(body: &str, key: &str) -> Option<String> {
        body.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then(|| uri_decode(value))
        })
    }

    /// Matcher used by httpmock: recomputes the signature from the salt that was
    /// actually sent, so no test-only salt injection is needed.
    fn signature_is_valid(request: &HttpMockRequest) -> bool {
        let body = String::from_utf8_lossy(request.body.as_deref().unwrap_or_default());
        let (Some(appid), Some(q), Some(salt), Some(sign)) = (
            form_value(&body, "appid"),
            form_value(&body, "q"),
            form_value(&body, "salt"),
            form_value(&body, "sign"),
        ) else {
            return false;
        };
        let expected = format!(
            "{:x}",
            Md5::digest(format!("{appid}{q}{salt}test-secret").as_bytes())
        );
        appid == "test-appid" && expected == sign
    }

    fn request(segments: Vec<Segment>) -> TranslateRequest {
        TranslateRequest {
            segments,
            source_lang: "en".to_owned(),
            target_lang: "zh".to_owned(),
            glossary: Vec::new(),
        }
    }

    #[tokio::test]
    async fn translates_batch_and_signs_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path(API_PATH)
                .body_contains("q=Hello%0AWorld")
                .body_contains("from=en")
                .body_contains("to=zh")
                .matches(signature_is_valid);
            then.status(200).json_body(json!({
                "from": "en",
                "to": "zh",
                "trans_result": [
                    { "src": "Hello", "dst": "你好" },
                    { "src": "World", "dst": "世界" }
                ]
            }));
        });

        let translator = BaiduTranslator::with_base_url(
            "test-appid".to_owned(),
            "test-secret".to_owned(),
            server.base_url(),
        )
        .unwrap();

        let empty = translator
            .translate(request(Vec::new()))
            .await
            .expect("empty request must not call the API");
        assert!(empty.segments.is_empty());
        assert_eq!(empty.usage.chars, 0);

        let response = translator
            .translate(request(vec![
                Segment {
                    id: "1".to_owned(),
                    text: "Hello".to_owned(),
                },
                Segment {
                    id: "2".to_owned(),
                    text: "World".to_owned(),
                },
            ]))
            .await
            .unwrap();

        mock.assert_hits(1);
        assert_eq!(response.segments.len(), 2);
        assert_eq!(response.segments[0].id, "1");
        assert_eq!(response.segments[0].text, "你好");
        assert_eq!(response.segments[1].id, "2");
        assert_eq!(response.segments[1].text, "世界");
        assert_eq!(response.usage.chars, 10);
        assert_eq!(response.usage.tokens, None);
    }

    #[test]
    fn splits_oversized_text_within_limits() {
        let text = "汉".repeat(MAX_BATCH_CHARS + 500);
        let pieces = split_text(&text);

        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces.concat(), text);
        for piece in &pieces {
            assert!(piece.chars().count() <= MAX_BATCH_CHARS);
            assert!(piece.len() <= MAX_BATCH_BYTES);
        }
    }

    #[tokio::test]
    async fn surfaces_api_error_code() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path(API_PATH);
            then.status(200).json_body(json!({
                "error_code": "54001",
                "error_msg": "Invalid Sign"
            }));
        });

        let translator = BaiduTranslator::with_base_url(
            "test-appid".to_owned(),
            "test-secret".to_owned(),
            server.base_url(),
        )
        .unwrap();
        let error = translator
            .translate(request(vec![Segment {
                id: "1".to_owned(),
                text: "Hello".to_owned(),
            }]))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(1);
        assert!(error.contains("baidu"), "unexpected error: {error}");
        assert!(error.contains("54001"), "unexpected error: {error}");
        assert!(error.contains("Invalid Sign"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn reports_http_status_and_body() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path(API_PATH);
            then.status(503).body("upstream unavailable");
        });

        let translator = BaiduTranslator::with_base_url(
            "test-appid".to_owned(),
            "test-secret".to_owned(),
            server.base_url(),
        )
        .unwrap();
        let error = translator
            .translate(request(vec![Segment {
                id: "1".to_owned(),
                text: "Hello".to_owned(),
            }]))
            .await
            .unwrap_err()
            .to_string();

        mock.assert_hits(1);
        assert!(error.contains("baidu"), "unexpected error: {error}");
        assert!(error.contains("503"), "unexpected error: {error}");
        assert!(
            error.contains("upstream unavailable"),
            "unexpected error: {error}"
        );
    }
}
