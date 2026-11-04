//! Translation providers and the shared request/response contract.
//!
//! Every provider implements [`Translator`]; the orchestration layer (retry,
//! rate limiting, caching, glossary, cost accounting) lives above it.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateRequest {
    pub segments: Vec<Segment>,
    pub source_lang: String,
    pub target_lang: String,
    /// term -> preferred translation, applied before/after the provider call.
    pub glossary: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedSegment {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub chars: usize,
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateResponse {
    pub segments: Vec<TranslatedSegment>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_batch_chars: usize,
    pub qps: f32,
    pub max_concurrency: usize,
}

#[async_trait]
pub trait Translator: Send + Sync {
    fn id(&self) -> &'static str;
    fn limits(&self) -> Limits;
    async fn translate(&self, request: TranslateRequest) -> Result<TranslateResponse>;
}

pub mod mock;
pub mod openai;

pub use mock::MockTranslator;
pub use openai::OpenAiTranslator;

/// Configuration for one provider, as stored by the GUI (secrets come from the
/// OS keychain and are injected at construction time).
///
/// Only the OpenAI-compatible provider ships for now; adding another backend
/// is a new variant plus a `Translator` implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderConfig {
    OpenAi {
        base_url: String,
        api_key: String,
        model: String,
    },
    Mock,
}

impl ProviderConfig {
    pub fn build(self) -> Result<Box<dyn Translator>> {
        Ok(match self {
            ProviderConfig::OpenAi {
                base_url,
                api_key,
                model,
            } => Box::new(OpenAiTranslator::new(base_url, api_key, model)?),
            ProviderConfig::Mock => Box::new(MockTranslator::default()),
        })
    }
}
