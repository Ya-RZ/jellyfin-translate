//! Translation providers and the shared request/response contract.
//!
//! Every provider implements [`Translator`]. Cross-item concurrency, retries and
//! persistence live in the `core` crate; the OpenAI adapter additionally retries
//! one failed request and tolerates the JSON shapes models actually emit.

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
