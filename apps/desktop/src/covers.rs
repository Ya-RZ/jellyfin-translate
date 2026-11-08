//! Poster and person-avatar loading for the library and details drawer.
//!
//! Images are fetched through [`AppService::cover`] / [`AppService::person_cover`]
//! on the tokio runtime, cached on disk under `<data dir>/images/` and kept in
//! memory as [`Arc<Image>`] so a repaint never refetches. The UI keeps showing
//! the hashed gradient placeholder until the bytes are ready.
//!
//! Cache file names are `<kind>_<id>_w<width>.<ext>`: the remote image tag is
//! not part of [`ItemView`] yet, so the width acts as the variant key and the
//! extension is sniffed from the response bytes (Jellyfin may answer JPEG or
//! PNG regardless of the requested format).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::core::AppService;
use gpui_kit::*;

use crate::app::AppState;

/// Source width for the poster grid.
pub(crate) const POSTER_WIDTH: u32 = 300;
/// Source width for the enlarged image in the details drawer.
pub(crate) const DRAWER_WIDTH: u32 = 500;
/// Source width for a person avatar.
pub(crate) const PERSON_WIDTH: u32 = 64;

const CACHE_EXTENSIONS: &[&str] = &["jpg", "png", "webp", "gif", "bmp"];

/// Stable in-memory key for an item's cover at a given width.
pub(crate) fn item_key(item_id: &str, width: u32) -> String {
    format!("item:{item_id}:w{width}")
}

/// Stable in-memory key for a person's avatar at a given width.
pub(crate) fn person_key(person_id: &str, width: u32) -> String {
    format!("person:{person_id}:w{width}")
}

enum CoverEntry {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

/// In-memory cover state. `Failed` is negative-cached so a missing image or an
/// offline server does not trigger a request on every repaint; the cache is
/// cleared when the connection or the mirrored library set changes.
#[derive(Default)]
pub(crate) struct CoverCache {
    entries: HashMap<String, CoverEntry>,
}

impl CoverCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn image(&self, key: &str) -> Option<Arc<Image>> {
        match self.entries.get(key) {
            Some(CoverEntry::Ready(image)) => Some(image.clone()),
            _ => None,
        }
    }

    /// Reserve a key. Returns `false` when it is already loading/ready/failed.
    fn begin(&mut self, key: &str) -> bool {
        if self.entries.contains_key(key) {
            false
        } else {
            self.entries.insert(key.to_string(), CoverEntry::Loading);
            true
        }
    }

    fn finish(&mut self, key: &str, image: Option<Arc<Image>>) {
        let entry = match image {
            Some(image) => CoverEntry::Ready(image),
            None => CoverEntry::Failed,
        };
        self.entries.insert(key.to_string(), entry);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone)]
enum CoverSource {
    Item(String),
    Person(String),
}

impl CoverSource {
    fn kind(&self) -> &'static str {
        match self {
            CoverSource::Item(_) => "item",
            CoverSource::Person(_) => "person",
        }
    }

    fn id(&self) -> &str {
        match self {
            CoverSource::Item(id) | CoverSource::Person(id) => id,
        }
    }
}

impl AppState {
    pub(crate) fn request_item_cover(&mut self, item_id: &str, width: u32, cx: &mut Context<Self>) {
        let source = CoverSource::Item(item_id.to_string());
        self.request_cover(item_key(item_id, width), source, width, cx);
    }

    pub(crate) fn request_person_cover(
        &mut self,
        person_id: &str,
        width: u32,
        cx: &mut Context<Self>,
    ) {
        let source = CoverSource::Person(person_id.to_string());
        self.request_cover(person_key(person_id, width), source, width, cx);
    }

    fn request_cover(
        &mut self,
        key: String,
        source: CoverSource,
        width: u32,
        cx: &mut Context<Self>,
    ) {
        if !self.covers.begin(&key) {
            return;
        }

        let Some(service) = self.service.clone() else {
            self.covers.finish(&key, None);
            return;
        };

        let cache_dir = service.data_dir_path().join("images");
        let permits = self.cover_permits.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<Arc<Image>>>();
        self.runtime.spawn(async move {
            let image = load_cover(service, cache_dir, source, width, permits).await;
            let _ = tx.send(image);
        });

        cx.spawn(async move |this, cx| {
            let image = rx.recv().await.unwrap_or(None);
            this.update(cx, |this, cx| {
                this.covers.finish(&key, image);
                cx.notify();
            })
            .ok();
        })
        .detach();

        cx.notify();
    }
}

async fn load_cover(
    service: Arc<AppService>,
    cache_dir: PathBuf,
    source: CoverSource,
    width: u32,
    permits: Arc<tokio::sync::Semaphore>,
) -> Option<Arc<Image>> {
    // Disk cache first: a warm cache needs no network and no permit.
    if let Some(bytes) = read_cached(&cache_dir, &source, width) {
        if let Some(image) = decode(bytes, &source, width, &cache_dir, false) {
            return Some(image);
        }
    }

    // Bound concurrent Jellyfin image requests.
    let _permit = permits.acquire_owned().await.ok()?;
    let fetched = match &source {
        CoverSource::Item(id) => service.cover(id, width).await,
        CoverSource::Person(id) => service.person_cover(id, width).await,
    };
    let bytes = match fetched {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => return None,
        Err(error) => {
            tracing::debug!(id = %source.id(), width, error = %error, "cover fetch failed");
            return None;
        }
    };

    decode(bytes, &source, width, &cache_dir, true)
}

fn read_cached(dir: &Path, source: &CoverSource, width: u32) -> Option<Vec<u8>> {
    for extension in CACHE_EXTENSIONS {
        let path = cache_path(dir, source, width, extension);
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => return Some(bytes),
            _ => {}
        }
    }
    None
}

fn decode(
    bytes: Vec<u8>,
    source: &CoverSource,
    width: u32,
    cache_dir: &Path,
    persist: bool,
) -> Option<Arc<Image>> {
    let (extension, format) = sniff(&bytes)?;
    if persist {
        let _ = std::fs::create_dir_all(cache_dir);
        let _ = std::fs::write(cache_path(cache_dir, source, width, extension), &bytes);
    }
    // `Image::from_bytes` only stores the format and bytes (it does not decode,
    // so it cannot panic); the actual decode happens lazily at render time and
    // simply shows nothing for a corrupt payload.
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

fn cache_path(dir: &Path, source: &CoverSource, width: u32, extension: &str) -> PathBuf {
    dir.join(format!(
        "{}_{}_w{}.{}",
        source.kind(),
        sanitize_id(source.id()),
        width,
        extension
    ))
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Jellyfin returns the bytes without a usable extension here, so sniff the
/// magic number before handing them to the image decoder.
fn sniff(bytes: &[u8]) -> Option<(&'static str, ImageFormat)> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", ImageFormat::Jpeg))
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some(("png", ImageFormat::Png))
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("webp", ImageFormat::Webp))
    } else if bytes.starts_with(b"GIF8") {
        Some(("gif", ImageFormat::Gif))
    } else if bytes.starts_with(b"BM") {
        Some(("bmp", ImageFormat::Bmp))
    } else {
        None
    }
}
