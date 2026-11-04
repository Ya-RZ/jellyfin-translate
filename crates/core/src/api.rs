//! Short traits that hide the two external boundaries (the Jellyfin REST client
//! and the translation provider) behind objects the service can also fake in
//! unit tests.
//!
//! They stay crate-private: the public contract is [`crate::AppService`].
//!
//! Futures are boxed by hand rather than with `async-trait`: this crate is named
//! `core`, so the macro's `::core::...` expansion would resolve to this crate in
//! doctests instead of the standard library.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use serde_json::Value;

use jellyfin::{ItemsPage, Library};
use translate::{TranslateRequest, TranslateResponse};

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait JellyfinApi: Send + Sync {
    fn libraries<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Library>>>;
    fn items_page<'a>(
        &'a self,
        parent_id: Option<&'a str>,
        include_types: &'a [String],
        start: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<ItemsPage>>;
    fn item<'a>(&'a self, item_id: &'a str) -> BoxFuture<'a, Result<Value>>;
    fn update_item<'a>(&'a self, item_id: &'a str, dto: &'a Value) -> BoxFuture<'a, Result<()>>;
    fn image<'a>(
        &'a self,
        item_id: &'a str,
        image_type: &'a str,
        max_width: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>>;
}

impl JellyfinApi for jellyfin::Client {
    fn libraries<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Library>>> {
        Box::pin(jellyfin::Client::libraries(self))
    }

    fn items_page<'a>(
        &'a self,
        parent_id: Option<&'a str>,
        include_types: &'a [String],
        start: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<ItemsPage>> {
        Box::pin(jellyfin::Client::items_page(
            self,
            parent_id,
            include_types,
            start,
            limit,
        ))
    }

    fn item<'a>(&'a self, item_id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(jellyfin::Client::item(self, item_id))
    }

    fn update_item<'a>(&'a self, item_id: &'a str, dto: &'a Value) -> BoxFuture<'a, Result<()>> {
        Box::pin(jellyfin::Client::update_item(self, item_id, dto))
    }

    fn image<'a>(
        &'a self,
        item_id: &'a str,
        image_type: &'a str,
        max_width: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(jellyfin::Client::image(
            self, item_id, image_type, max_width,
        ))
    }
}

/// The slice of [`translate::Translator`] the service needs: field-specific
/// prompt templates are prepended to the provider's fixed system prompt.
pub(crate) trait PromptTranslator: Send + Sync {
    fn translate_with_prompt<'a>(
        &'a self,
        request: TranslateRequest,
        prompt: Option<&'a str>,
    ) -> BoxFuture<'a, Result<TranslateResponse>>;
}

impl PromptTranslator for translate::OpenAiTranslator {
    fn translate_with_prompt<'a>(
        &'a self,
        request: TranslateRequest,
        prompt: Option<&'a str>,
    ) -> BoxFuture<'a, Result<TranslateResponse>> {
        Box::pin(translate::OpenAiTranslator::translate_with_prompt(
            self, request, prompt,
        ))
    }
}
