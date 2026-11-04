//! Deterministic provider used by tests and offline demos.

use anyhow::Result;
use async_trait::async_trait;

use crate::{Limits, TranslateRequest, TranslateResponse, TranslatedSegment, Translator, Usage};

#[derive(Default)]
pub struct MockTranslator;

#[async_trait]
impl Translator for MockTranslator {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn limits(&self) -> Limits {
        Limits {
            max_batch_chars: 100_000,
            qps: 100.0,
            max_concurrency: 8,
        }
    }

    async fn translate(&self, request: TranslateRequest) -> Result<TranslateResponse> {
        let chars = request
            .segments
            .iter()
            .map(|s| s.text.chars().count())
            .sum();
        let segments = request
            .segments
            .into_iter()
            .map(|s| TranslatedSegment {
                id: s.id,
                text: format!("[{}] {}", request.target_lang, s.text),
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
