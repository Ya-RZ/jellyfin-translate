//! Application services: sync Jellyfin libraries into the local store.
//!
//! The GUI calls these async functions from a tokio runtime and receives
//! progress through a callback. No CLI, no UI types in this crate.

use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use jellyfin::{Client, ItemDto};
use store::{ItemRecord, PersonRecord, Store};

/// Item types we pull from a library. Jellyfin collection types map to these,
/// but asking for all four is harmless and keeps the first version simple.
const ITEM_TYPES: &[&str] = &["Movie", "Series", "Season", "Episode"];

pub struct AppConfig {
    pub jellyfin_url: String,
    pub api_key: String,
    pub db_path: PathBuf,
}

impl AppConfig {
    /// Per-user data directory for the local mirror database.
    pub fn default_db_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("share"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("jellyfin-translate").join("library.db")
    }
}

#[derive(Debug, Clone)]
pub struct SyncProgress {
    pub libraries_done: usize,
    pub libraries_total: usize,
    pub items: usize,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct SyncReport {
    pub libraries: usize,
    pub items: usize,
    pub elapsed_ms: u128,
}

pub struct SyncService {
    client: Client,
    store: Store,
}

impl SyncService {
    pub fn new(client: Client, store: Store) -> Self {
        Self { client, store }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Walk every library page by page and mirror the items we care about.
    pub async fn sync_all(
        &mut self,
        mut progress: impl FnMut(SyncProgress) + Send,
    ) -> Result<SyncReport> {
        let started = Instant::now();
        let libraries = self
            .client
            .libraries()
            .await
            .context("list Jellyfin libraries")?;
        let total = libraries.len();
        let mut items_seen = 0usize;

        for (index, library) in libraries.iter().enumerate() {
            let types: Vec<String> = ITEM_TYPES.iter().map(|t| (*t).to_string()).collect();
            let mut start = 0usize;
            loop {
                let page = self
                    .client
                    .items_page(Some(&library.id), &types, start, 200)
                    .await
                    .with_context(|| format!("list items of library {}", library.name))?;
                let page_len = page.items.len();
                if page_len == 0 {
                    break;
                }
                let records: Vec<ItemRecord> = page
                    .items
                    .iter()
                    .map(|dto| to_record(dto, &library.id))
                    .collect();
                self.store
                    .upsert_items(&records)
                    .context("write items to local store")?;
                items_seen += page_len;
                start += page_len;
                if page.total_record_count > 0 && start as i64 >= page.total_record_count {
                    break;
                }
            }
            progress(SyncProgress {
                libraries_done: index + 1,
                libraries_total: total,
                items: items_seen,
                message: format!("已同步 {}", library.name),
            });
        }

        Ok(SyncReport {
            libraries: total,
            items: items_seen,
            elapsed_ms: started.elapsed().as_millis(),
        })
    }
}

fn to_record(dto: &ItemDto, library_id: &str) -> ItemRecord {
    let people: Vec<PersonRecord> = dto
        .people
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|p| PersonRecord {
            id: p.id.clone(),
            name: p.name.clone(),
            role: p.role.clone(),
            person_type: p.person_type.clone(),
        })
        .collect();

    let tags = dto.tags.clone().unwrap_or_default();
    let name = dto.name.clone().unwrap_or_default();
    let original_title = dto.original_title.clone().unwrap_or_default();
    let overview = dto.overview.clone().unwrap_or_default();

    let mut hasher = blake3::Hasher::new();
    hasher.update(name.as_bytes());
    hasher.update(b"\0");
    hasher.update(original_title.as_bytes());
    hasher.update(b"\0");
    hasher.update(overview.as_bytes());
    for tag in &tags {
        hasher.update(b"\0");
        hasher.update(tag.as_bytes());
    }
    for person in &people {
        hasher.update(b"\0");
        hasher.update(person.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(person.role.as_deref().unwrap_or("").as_bytes());
    }

    ItemRecord {
        id: dto.id.clone(),
        library_id: library_id.to_string(),
        item_type: dto.item_type.clone().unwrap_or_default(),
        name: dto.name.clone(),
        original_title: dto.original_title.clone(),
        overview: dto.overview.clone(),
        tags,
        people,
        image_tag: dto
            .image_tags
            .as_ref()
            .and_then(|m| m.get("Primary").cloned()),
        locked_fields: dto.locked_fields.clone().unwrap_or_default(),
        source_hash: hasher.finalize().to_hex().to_string(),
        synced_at: now_ts(),
    }
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
