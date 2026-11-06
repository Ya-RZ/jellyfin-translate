//! Application services for the desktop app.
//!
//! The GUI owns one [`AppService`] and calls these methods from background
//! tasks; no UI types live in this crate.
//!
//! [`AppService`] is `Send + Sync`: the SQLite connection and the Jellyfin
//! client live behind an `Arc<Mutex<..>>`, and no lock is held across an
//! `.await`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use translate::{Segment, TranslateRequest};

use store::{
    ItemDraft, ItemRecord, LibraryRecord, PersonRecord, PresetRecord, Store, TranslationRecord,
};

mod api;
use api::{JellyfinApi, PromptTranslator};

pub use jellyfin::SystemInfo as ServerInfo;

/// Page size used when walking a Jellyfin library.
const PAGE_SIZE: usize = 200;
const INCLUDE_ITEM_TYPES: &[&str] = &["Movie", "Series", "Season", "Episode"];

const SETTING_TEMPLATES: &str = "templates";
const SETTING_PROVIDER: &str = "provider";

/// `MetadataField` values accepted by Jellyfin 10.11/12 (per the official
/// OpenAPI-generated SDK). `Cast` is how People are locked; there is no
/// `People`/`OriginalTitle` member in the current enum.
const SERVER_LOCKED_FIELDS: &[&str] = &["Name", "Overview", "Tags", "Cast"];

/// Per-item translation state shown in the library grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranslationStatus {
    Untranslated,
    Draft,
    Written,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonView {
    /// Jellyfin person item id, used for the avatar image; absent for people
    /// typed by hand in the edit dialog.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemView {
    pub id: String,
    pub library_id: String,
    pub item_type: String,
    pub title: String,
    pub original_title: String,
    pub overview: String,
    pub tags: Vec<String>,
    pub people: Vec<PersonView>,
    pub status: TranslationStatus,
    pub locked: bool,
}

/// User-editable fields from the edit dialog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemEdit {
    pub title: String,
    pub original_title: String,
    pub overview: String,
    pub tags: Vec<String>,
    pub people: Vec<PersonView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryView {
    pub id: String,
    pub name: String,
    pub item_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Templates {
    pub title: String,
    pub overview: String,
    pub tags: String,
    pub people: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub source_lang: String,
    pub target_lang: String,
    pub concurrency: usize,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4.1-mini".into(),
            api_key: String::new(),
            source_lang: "auto".into(),
            target_lang: "zh-CN".into(),
            concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preset {
    pub source: String,
    pub target: String,
    pub uses: i64,
}

/// Persisted Jellyfin connection settings (`<data dir>/config.json`).
///
/// Stored so the desktop app can pre-fill the connection form and reconnect on
/// startup. The API key is a local secret; the file is written with `0600`
/// permissions on Unix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JellyfinConfig {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    jellyfin: JellyfinConfig,
}

#[derive(Debug, Clone, Default)]
pub struct SyncProgress {
    pub libraries_done: usize,
    pub libraries_total: usize,
    pub items: usize,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct SyncReport {
    pub libraries: usize,
    pub items: usize,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Default)]
pub struct TranslateReport {
    pub translated: usize,
    pub failed: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct WriteReport {
    pub written: usize,
    pub failed: Vec<(String, String)>,
}

#[cfg(test)]
type ClientFactory = dyn Fn(&str, &str) -> Result<Arc<dyn JellyfinApi>> + Send + Sync;

struct Inner {
    store: Store,
    api: Option<Arc<dyn JellyfinApi>>,
    #[cfg(test)]
    translator: Option<Arc<dyn PromptTranslator>>,
    /// Lets tests exercise `connect` without a real server.
    #[cfg(test)]
    client_factory: Option<Arc<ClientFactory>>,
}

/// Shared context for one `sync_libraries` run (keeps the helper's argument
/// list readable and the progress callback borrowed across the loop).
struct SyncRun<'a, F> {
    api: &'a dyn JellyfinApi,
    include_types: &'a [String],
    progress: &'a F,
    libraries_total: usize,
}

pub struct AppService {
    data_dir: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

impl AppService {
    /// Portable data directory: `<exe dir>/data` (falls back to the current
    /// directory when the executable path is unavailable, e.g. in tests).
    pub fn data_dir() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("data")
    }

    pub fn open() -> Result<Self> {
        Self::open_at(Self::data_dir())
    }

    /// Open (or create) the local database in `data_dir`, run the migrations and
    /// seed templates/provider settings on first launch.
    ///
    /// This is the injectable counterpart of [`AppService::open`], used by tests
    /// and by callers that need a portable directory of their own.
    pub fn open_at(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        let store = Store::open(data_dir.join("library.db"))
            .with_context(|| format!("open library.db in {}", data_dir.display()))?;
        let service = Self {
            data_dir,
            inner: Arc::new(Mutex::new(Inner {
                store,
                api: None,
                #[cfg(test)]
                translator: None,
                #[cfg(test)]
                client_factory: None,
            })),
        };
        service.seed_defaults()?;
        Ok(service)
    }

    /// In-memory variant used by tests (no files are touched).
    pub fn open_in_memory() -> Result<Self> {
        let service = Self {
            data_dir: PathBuf::from("."),
            inner: Arc::new(Mutex::new(Inner {
                store: Store::open_in_memory()?,
                api: None,
                #[cfg(test)]
                translator: None,
                #[cfg(test)]
                client_factory: None,
            })),
        };
        service.seed_defaults()?;
        Ok(service)
    }

    /// Directory that holds `library.db` and future logs/config.
    pub fn data_dir_path(&self) -> &Path {
        &self.data_dir
    }

    fn inner(&self) -> Result<MutexGuard<'_, Inner>> {
        self.inner
            .lock()
            .map_err(|_| anyhow!("app service lock poisoned"))
    }

    fn seed_defaults(&self) -> Result<()> {
        let mut inner = self.inner()?;
        if inner.store.get_setting(SETTING_TEMPLATES)?.is_none() {
            inner.store.set_setting(
                SETTING_TEMPLATES,
                &serde_json::to_string(&default_templates())?,
            )?;
        }
        if inner.store.get_setting(SETTING_PROVIDER)?.is_none() {
            inner.store.set_setting(
                SETTING_PROVIDER,
                &serde_json::to_string(&ProviderSettings::default())?,
            )?;
        }
        Ok(())
    }

    fn api(&self) -> Result<Arc<dyn JellyfinApi>> {
        self.inner()?
            .api
            .clone()
            .ok_or_else(|| anyhow!("尚未连接 Jellyfin 服务器"))
    }

    pub async fn connect(&self, url: &str, api_key: &str) -> Result<ServerInfo> {
        // Network round trip first; the mutex is only taken to publish the client.
        let api = self.make_api(url, api_key)?;
        let info = api.system_info().await?;

        // Persist only after the server accepted these credentials. A failed
        // write must not turn a successful connection into an error.
        let config = JellyfinConfig {
            url: url.trim().to_string(),
            api_key: api_key.trim().to_string(),
        };
        if let Err(error) = self.save_jellyfin_config(&config) {
            tracing::warn!(error = %error, "failed to persist Jellyfin connection settings");
        }

        {
            let mut inner = self.inner()?;
            inner.api = Some(api);
        }
        tracing::info!(server = %info.server_name, version = %info.version, "connected to Jellyfin");
        Ok(info)
    }

    fn make_api(&self, url: &str, api_key: &str) -> Result<Arc<dyn JellyfinApi>> {
        #[cfg(test)]
        {
            if let Some(factory) = self.inner()?.client_factory.clone() {
                return factory(url, api_key);
            }
        }
        Ok(Arc::new(jellyfin::Client::new(url, api_key)?))
    }

    /// Path of the persisted connection settings.
    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    /// Load the persisted Jellyfin connection settings.
    ///
    /// A missing file or invalid JSON yields empty defaults instead of an error
    /// (a fresh/broken config must never block startup). Other I/O errors (for
    /// example a permission problem) are reported.
    pub fn jellyfin_config(&self) -> Result<JellyfinConfig> {
        let path = self.config_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(JellyfinConfig::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("读取 {}", path.display()));
            }
        };
        match serde_json::from_slice::<ConfigFile>(&bytes) {
            Ok(file) => Ok(file.jellyfin),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "config.json 解析失败，使用默认连接配置"
                );
                Ok(JellyfinConfig::default())
            }
        }
    }

    /// Atomically persist the Jellyfin connection settings.
    ///
    /// The file is written to a sibling temp file (mode `0600` on Unix) and then
    /// renamed over `config.json`, so a crash never leaves a half-written file.
    pub fn save_jellyfin_config(&self, config: &JellyfinConfig) -> Result<()> {
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 {}", parent.display()))?;
        }
        let payload = serde_json::to_vec_pretty(&ConfigFile {
            jellyfin: config.clone(),
        })
        .context("序列化 config.json")?;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let tmp = path.with_file_name(format!(".config.json.tmp.{}.{}", std::process::id(), nanos));

        if let Err(error) = write_private_file(&tmp, &payload) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("替换 {}", path.display()));
        }
        Ok(())
    }

    pub fn libraries(&self) -> Result<Vec<LibraryView>> {
        let inner = self.inner()?;
        let libraries = inner.store.list_libraries()?;
        let counts = inner.store.library_item_counts()?;
        Ok(libraries
            .into_iter()
            .map(|library| LibraryView {
                item_count: counts.get(&library.id).copied().unwrap_or(0),
                id: library.id,
                name: library.name,
            })
            .collect())
    }

    /// List the media libraries on the server, without persisting them.
    ///
    /// `item_count` is how many items of that library already exist in the local
    /// mirror (0 before the first sync). Returns the Chinese "not connected"
    /// error when [`Self::connect`] has not succeeded yet.
    pub async fn remote_libraries(&self) -> Result<Vec<LibraryView>> {
        let api = self.api()?;
        let libraries = api.libraries().await.context("读取 Jellyfin 媒体库")?;
        let counts = {
            let inner = self.inner()?;
            inner.store.library_item_counts()?
        };
        Ok(libraries
            .into_iter()
            .map(|library| LibraryView {
                item_count: counts.get(&library.id).copied().unwrap_or(0),
                id: library.id,
                name: library.name,
            })
            .collect())
    }

    /// Sync only the selected libraries. Unknown ids are ignored.
    pub async fn sync_libraries<F>(
        &self,
        library_ids: Vec<String>,
        progress: F,
    ) -> Result<SyncReport>
    where
        F: Fn(SyncProgress) + Send + Sync + 'static,
    {
        let started = Instant::now();
        let api = self.api()?;
        let selected = library_ids.into_iter().collect::<HashSet<_>>();
        let libraries = api
            .libraries()
            .await
            .context("读取 Jellyfin 媒体库")?
            .into_iter()
            .filter(|library| selected.contains(&library.id))
            .collect::<Vec<_>>();

        let records = libraries
            .iter()
            .map(|library| LibraryRecord {
                id: library.id.clone(),
                name: library.name.clone(),
                collection_type: library.collection_type.clone(),
            })
            .collect::<Vec<_>>();
        {
            let mut inner = self.inner()?;
            inner.store.upsert_libraries(&records)?;
        }

        let total_libraries = libraries.len();
        let include_types = INCLUDE_ITEM_TYPES
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        let mut total_items = 0usize;
        progress(SyncProgress {
            libraries_done: 0,
            libraries_total: total_libraries,
            items: 0,
            message: format!("准备同步 {total_libraries} 个媒体库"),
        });

        let run = SyncRun {
            api: api.as_ref(),
            include_types: &include_types,
            progress: &progress,
            libraries_total: total_libraries,
        };
        for (library_index, library) in libraries.iter().enumerate() {
            total_items = self
                .sync_one_library(&run, library, library_index + 1, total_items)
                .await?;
        }

        let report = SyncReport {
            libraries: total_libraries,
            items: total_items,
            elapsed_ms: started.elapsed().as_millis(),
        };
        tracing::info!(
            libraries = report.libraries,
            items = report.items,
            elapsed_ms = report.elapsed_ms,
            "sync finished"
        );
        Ok(report)
    }

    /// Sync every library advertised by the server.
    pub async fn sync_all<F>(&self, progress: F) -> Result<SyncReport>
    where
        F: Fn(SyncProgress) + Send + Sync + 'static,
    {
        let library_ids = self
            .remote_libraries()
            .await?
            .into_iter()
            .map(|library| library.id)
            .collect::<Vec<_>>();
        self.sync_libraries(library_ids, progress).await
    }

    /// Page through one library into the local store. Returns the running item
    /// total so the caller can keep reporting cumulative progress.
    async fn sync_one_library<F>(
        &self,
        run: &SyncRun<'_, F>,
        library: &jellyfin::Library,
        libraries_done: usize,
        items_before: usize,
    ) -> Result<usize>
    where
        F: Fn(SyncProgress) + Send + Sync + 'static,
    {
        let mut start = 0usize;
        let mut total_items = items_before;
        loop {
            let page = run
                .api
                .items_page(Some(&library.id), run.include_types, start, PAGE_SIZE)
                .await
                .with_context(|| format!("分页读取媒体库 {} ({})", library.name, library.id))?;
            if page.items.is_empty() {
                break;
            }

            let count = page.items.len();
            let records = page
                .items
                .iter()
                .map(|dto| item_record_from_dto(&library.id, dto))
                .collect::<Vec<_>>();
            {
                let mut inner = self.inner()?;
                inner.store.upsert_items(&records)?;
            }
            total_items += count;
            start += count;
            // `total_record_count` is authoritative when present; fall back to a
            // short page when a server omits it.
            let reached_total =
                page.total_record_count > 0 && start as i64 >= page.total_record_count;
            if reached_total || count < PAGE_SIZE {
                break;
            }
        }

        (run.progress)(SyncProgress {
            libraries_done,
            libraries_total: run.libraries_total,
            items: total_items,
            message: format!(
                "已同步 {libraries_done}/{} 个媒体库（{total_items} 个条目）",
                run.libraries_total
            ),
        });
        tracing::debug!(library = %library.name, items = total_items, "library synced");
        Ok(total_items)
    }

    pub fn items(&self, library_id: &str) -> Result<Vec<ItemView>> {
        let inner = self.inner()?;
        let records = inner.store.list_items(Some(library_id))?;
        Ok(records.into_iter().map(item_view).collect())
    }

    pub fn save_draft(&self, item_id: &str, edit: ItemEdit) -> Result<()> {
        let mut inner = self.inner()?;
        let existing = inner
            .store
            .get_item(item_id)?
            .ok_or_else(|| anyhow!("本地库中不存在条目 {item_id}"))?;
        anyhow::ensure!(!existing.locked, "条目 {item_id} 已锁定，请先解锁");

        let draft = ItemDraft {
            people: merge_people(&existing.people, edit.people),
            name: edit.title,
            original_title: edit.original_title,
            overview: edit.overview,
            tags: edit.tags,
        };
        inner.store.save_draft(item_id, &draft)?;
        Ok(())
    }

    pub async fn translate(
        &self,
        ids: Vec<String>,
        templates: Templates,
        settings: ProviderSettings,
    ) -> Result<TranslateReport> {
        if ids.is_empty() {
            return Ok(TranslateReport::default());
        }
        let translator = self.translator(&settings)?;

        let mut failed = Vec::new();
        let mut items = Vec::new();
        let presets = {
            let inner = self.inner()?;
            for id in &ids {
                match inner.store.get_item(id)? {
                    Some(item) => items.push(item),
                    None => failed.push((id.clone(), "本地库中不存在该条目".to_string())),
                }
            }
            inner.store.presets()?
        };
        let preset_map = presets
            .iter()
            .map(|preset| (normalize_preset_key(&preset.source), preset.target.clone()))
            .collect::<HashMap<_, _>>();

        let mut report = TranslateReport {
            translated: 0,
            failed,
        };
        let mut used_presets = Vec::new();

        for item in items {
            if item.locked {
                tracing::debug!(item = %item.id, "skip locked item");
                continue;
            }
            match translate_item(
                translator.as_ref(),
                &item,
                &templates,
                &settings,
                &preset_map,
            )
            .await
            {
                Ok(outcome) => {
                    {
                        let mut inner = self.inner()?;
                        inner.store.save_draft(&item.id, &outcome.draft)?;
                        record_translations(&mut inner.store, &item, &outcome.draft, &settings)?;
                    }
                    used_presets.extend(outcome.preset_sources);
                    report.translated += 1;
                }
                Err(error) => {
                    tracing::warn!(item = %item.id, error = %error, "translation failed");
                    report.failed.push((item.id.clone(), format!("{error:#}")));
                }
            }
        }

        if !used_presets.is_empty() {
            let mut inner = self.inner()?;
            inner.store.increment_preset_uses(&used_presets)?;
        }
        Ok(report)
    }

    pub async fn write_back(&self, ids: Vec<String>) -> Result<WriteReport> {
        let api = self.api()?;
        let mut report = WriteReport::default();

        for id in ids {
            let item = {
                let inner = self.inner()?;
                inner.store.get_item(&id)?
            };
            let Some(item) = item else {
                report.failed.push((id, "本地库中不存在该条目".to_string()));
                continue;
            };
            if item.locked {
                tracing::debug!(item = %item.id, "skip locked item");
                continue;
            }

            match write_item(api.as_ref(), &item).await {
                Ok(locked_fields) => {
                    let mut inner = self.inner()?;
                    inner.store.mark_written(&item.id, &locked_fields)?;
                    report.written += 1;
                }
                Err(error) => {
                    tracing::warn!(item = %item.id, error = %error, "write-back failed");
                    report.failed.push((item.id.clone(), format!("{error:#}")));
                }
            }
        }
        Ok(report)
    }

    /// Raw bytes of an item's `Primary` image (poster/thumb), scaled to
    /// `max_width` by the server. Uses the client saved by [`Self::connect`].
    pub async fn cover(&self, item_id: &str, max_width: u32) -> Result<Vec<u8>> {
        let api = self.api()?;
        api.image(item_id, "Primary", max_width).await
    }

    /// Raw bytes of a person's `Primary` image (avatar).
    pub async fn person_cover(&self, person_id: &str, max_width: u32) -> Result<Vec<u8>> {
        let api = self.api()?;
        api.image(person_id, "Primary", max_width).await
    }

    pub fn set_locked(&self, ids: &[String], locked: bool) -> Result<()> {
        let mut inner = self.inner()?;
        inner.store.set_locked(ids, locked)?;
        Ok(())
    }

    pub fn presets(&self) -> Result<Vec<Preset>> {
        let inner = self.inner()?;
        Ok(inner
            .store
            .presets()?
            .into_iter()
            .map(preset_view)
            .collect())
    }

    pub fn upsert_preset(&self, source: &str, target: &str) -> Result<()> {
        let mut inner = self.inner()?;
        inner.store.upsert_preset(source, target)?;
        Ok(())
    }

    pub fn delete_preset(&self, source: &str) -> Result<()> {
        let mut inner = self.inner()?;
        inner.store.delete_preset(source)?;
        Ok(())
    }

    pub fn templates(&self) -> Result<Templates> {
        let raw = {
            let inner = self.inner()?;
            inner.store.get_setting(SETTING_TEMPLATES)?
        };
        match raw {
            Some(raw) => {
                serde_json::from_str(&raw).context("settings 表中的 templates JSON 无法解析")
            }
            None => Ok(default_templates()),
        }
    }

    pub fn save_templates(&self, templates: &Templates) -> Result<()> {
        let mut inner = self.inner()?;
        inner.store.set_setting(
            SETTING_TEMPLATES,
            &serde_json::to_string(templates).context("序列化 templates")?,
        )?;
        Ok(())
    }

    pub fn provider_settings(&self) -> Result<ProviderSettings> {
        let raw = {
            let inner = self.inner()?;
            inner.store.get_setting(SETTING_PROVIDER)?
        };
        match raw {
            Some(raw) => {
                serde_json::from_str(&raw).context("settings 表中的 provider JSON 无法解析")
            }
            None => Ok(ProviderSettings::default()),
        }
    }

    pub fn save_provider_settings(&self, settings: &ProviderSettings) -> Result<()> {
        let mut inner = self.inner()?;
        inner.store.set_setting(
            SETTING_PROVIDER,
            &serde_json::to_string(settings).context("序列化 provider settings")?,
        )?;
        Ok(())
    }

    fn translator(&self, settings: &ProviderSettings) -> Result<Arc<dyn PromptTranslator>> {
        #[cfg(test)]
        {
            if let Some(translator) = self.inner()?.translator.clone() {
                return Ok(translator);
            }
        }
        Ok(Arc::new(translate::OpenAiTranslator::new(
            settings.base_url.clone(),
            settings.api_key.clone(),
            settings.model.clone(),
        )?))
    }

    #[cfg(test)]
    fn install_api(&self, api: Arc<dyn JellyfinApi>) -> Result<()> {
        let mut inner = self.inner()?;
        inner.api = Some(api);
        Ok(())
    }

    #[cfg(test)]
    fn install_translator(&self, translator: Arc<dyn PromptTranslator>) -> Result<()> {
        let mut inner = self.inner()?;
        inner.translator = Some(translator);
        Ok(())
    }

    #[cfg(test)]
    fn install_client_factory(&self, factory: Arc<ClientFactory>) -> Result<()> {
        let mut inner = self.inner()?;
        inner.client_factory = Some(factory);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DTO → store
// ---------------------------------------------------------------------------

fn item_record_from_dto(library_id: &str, dto: &jellyfin::ItemDto) -> ItemRecord {
    let tags = dto.tags.clone().unwrap_or_default();
    let people = dto
        .people
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|person| PersonRecord {
            id: person.id,
            name: person.name,
            role: person.role,
            person_type: person.person_type,
        })
        .collect::<Vec<_>>();
    let image_tag = dto
        .image_tags
        .as_ref()
        .and_then(|tags| tags.get("Primary").cloned());
    let source_hash = source_hash(
        dto.name.as_deref(),
        dto.original_title.as_deref(),
        dto.overview.as_deref(),
        &tags,
        &people,
    );

    ItemRecord {
        id: dto.id.clone(),
        library_id: library_id.to_string(),
        item_type: dto
            .item_type
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        name: dto.name.clone(),
        original_title: dto.original_title.clone(),
        overview: dto.overview.clone(),
        tags,
        people,
        image_tag,
        locked_fields: dto.locked_fields.clone().unwrap_or_default(),
        source_hash,
        synced_at: store::now_unix(),
        status: store::STATUS_UNTRANSLATED.to_string(),
        locked: false,
        draft_name: None,
        draft_original_title: None,
        draft_overview: None,
        draft_tags: None,
        draft_people: None,
    }
}

fn source_hash(
    name: Option<&str>,
    original_title: Option<&str>,
    overview: Option<&str>,
    tags: &[String],
    people: &[PersonRecord],
) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [name, original_title, overview] {
        match value {
            Some(text) => {
                hasher.update(text.as_bytes());
            }
            None => {
                hasher.update(b"\0");
            }
        }
        hasher.update(b"\x1e");
    }
    for tag in tags {
        hasher.update(tag.as_bytes());
        hasher.update(b"\x1e");
    }
    for person in people {
        hasher.update(person.name.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(person.role.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\x1e");
    }
    hasher.finalize().to_hex().to_string()
}

fn item_view(record: ItemRecord) -> ItemView {
    let title = record.effective_name().to_string();
    let original_title = record.effective_original_title().to_string();
    let overview = record.effective_overview().to_string();
    let tags = record.effective_tags().to_vec();
    let people = record
        .effective_people()
        .iter()
        .map(|person| PersonView {
            id: person.id.clone(),
            name: person.name.clone(),
            role: person.role.clone(),
        })
        .collect();
    ItemView {
        id: record.id,
        library_id: record.library_id,
        item_type: record.item_type,
        title,
        original_title,
        overview,
        tags,
        people,
        status: parse_status(&record.status),
        locked: record.locked,
    }
}

fn parse_status(raw: &str) -> TranslationStatus {
    match raw {
        store::STATUS_DRAFT => TranslationStatus::Draft,
        store::STATUS_WRITTEN => TranslationStatus::Written,
        _ => TranslationStatus::Untranslated,
    }
}

fn preset_view(record: PresetRecord) -> Preset {
    Preset {
        source: record.source,
        target: record.target,
        uses: record.uses,
    }
}

/// The edit dialog carries the person id when it was opened from the grid (the
/// user may also type new people by hand). Keep the server-side id and type from
/// the matching snapshot entry so write-back preserves them.
fn merge_people(existing: &[PersonRecord], edit: Vec<PersonView>) -> Vec<PersonRecord> {
    edit.into_iter()
        .enumerate()
        .map(|(index, person)| {
            let id = person.id.clone();
            // Prefer an explicit id, then the same (normalized) name, then the
            // positional entry. This survives deleting an entry above without
            // shifting ids/types onto the wrong actor.
            let base = id
                .as_deref()
                .and_then(|id| existing.iter().find(|base| base.id.as_deref() == Some(id)))
                .or_else(|| {
                    let key = normalize_preset_key(&person.name);
                    existing
                        .iter()
                        .find(|base| normalize_preset_key(&base.name) == key)
                })
                .or_else(|| existing.get(index));
            PersonRecord {
                id: id.or_else(|| base.and_then(|base| base.id.clone())),
                name: person.name,
                role: person.role,
                person_type: base.and_then(|base| base.person_type.clone()),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Translation
// ---------------------------------------------------------------------------

struct TranslateOutcome {
    draft: ItemDraft,
    /// Preset source names that were applied, for the usage counter.
    preset_sources: Vec<String>,
}

async fn translate_item(
    translator: &dyn PromptTranslator,
    item: &ItemRecord,
    templates: &Templates,
    settings: &ProviderSettings,
    presets: &HashMap<String, String>,
) -> Result<TranslateOutcome> {
    let source_name = source_text(&item.name, &item.draft_name);
    let source_original = source_text(&item.original_title, &item.draft_original_title);
    let source_overview = source_text(&item.overview, &item.draft_overview);
    let source_tags = item.tags.clone();
    let source_people = item.people.clone();

    // Title + original title share the "title" template.
    let mut title_segments = Vec::new();
    if !source_name.trim().is_empty() {
        title_segments.push(Segment {
            id: "name".to_string(),
            text: source_name.clone(),
        });
    }
    if !source_original.trim().is_empty() && source_original != source_name {
        title_segments.push(Segment {
            id: "original_title".to_string(),
            text: source_original.clone(),
        });
    }
    let title_translations = if title_segments.is_empty() {
        HashMap::new()
    } else {
        let instruction = render_template(
            &templates.title,
            &source_name,
            &title_segments,
            settings,
            "title",
        );
        call_segments(translator, &title_segments, instruction, settings)
            .await
            .context("翻译标题失败")?
    };

    let draft_name = if source_name.trim().is_empty() {
        source_name.clone()
    } else {
        title_translations
            .get("name")
            .cloned()
            .unwrap_or_else(|| source_name.clone())
    };
    let draft_original_title = if source_original.trim().is_empty() {
        String::new()
    } else if source_original == source_name {
        draft_name.clone()
    } else {
        title_translations
            .get("original_title")
            .cloned()
            .unwrap_or_else(|| source_original.clone())
    };

    let draft_overview = if source_overview.trim().is_empty() {
        source_overview.clone()
    } else {
        let segments = vec![Segment {
            id: "overview".to_string(),
            text: source_overview.clone(),
        }];
        let instruction = render_template(
            &templates.overview,
            &source_overview,
            &segments,
            settings,
            "overview",
        );
        let translations = call_segments(translator, &segments, instruction, settings)
            .await
            .context("翻译简介失败")?;
        translations
            .get("overview")
            .cloned()
            .unwrap_or_else(|| source_overview.clone())
    };

    let draft_tags = translate_tags(translator, &source_tags, templates, settings).await?;

    let people = translate_people(translator, &source_people, templates, settings, presets).await?;

    Ok(TranslateOutcome {
        draft: ItemDraft {
            name: draft_name,
            original_title: draft_original_title,
            overview: draft_overview,
            tags: draft_tags,
            people: people.people,
        },
        preset_sources: people.preset_sources,
    })
}

struct TranslatedPeople {
    people: Vec<PersonRecord>,
    preset_sources: Vec<String>,
}

async fn translate_tags(
    translator: &dyn PromptTranslator,
    tags: &[String],
    templates: &Templates,
    settings: &ProviderSettings,
) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    for (index, tag) in tags.iter().enumerate() {
        if !tag.trim().is_empty() {
            segments.push(Segment {
                id: format!("tag:{index}"),
                text: tag.clone(),
            });
        }
    }
    if segments.is_empty() {
        return Ok(tags.to_vec());
    }
    let instruction = render_template(
        &templates.tags,
        &tags.join("\n"),
        &segments,
        settings,
        "tags",
    );
    let translations = call_segments(translator, &segments, instruction, settings)
        .await
        .context("翻译标签失败")?;
    Ok(tags
        .iter()
        .enumerate()
        .map(|(index, tag)| {
            if tag.trim().is_empty() {
                tag.clone()
            } else {
                translations
                    .get(&format!("tag:{index}"))
                    .cloned()
                    .unwrap_or_else(|| tag.clone())
            }
        })
        .collect())
}

async fn translate_people(
    translator: &dyn PromptTranslator,
    people: &[PersonRecord],
    templates: &Templates,
    settings: &ProviderSettings,
    presets: &HashMap<String, String>,
) -> Result<TranslatedPeople> {
    if people.is_empty() {
        return Ok(TranslatedPeople {
            people: Vec::new(),
            preset_sources: Vec::new(),
        });
    }

    let mut segments = Vec::new();
    let mut preset_hits: HashMap<usize, String> = HashMap::new();
    let mut preset_sources = Vec::new();
    for (index, person) in people.iter().enumerate() {
        if !person.name.trim().is_empty() {
            if let Some(target) = presets.get(&normalize_preset_key(&person.name)) {
                preset_hits.insert(index, target.clone());
                preset_sources.push(person.name.trim().to_string());
            } else {
                segments.push(Segment {
                    id: format!("person_name:{index}"),
                    text: person.name.clone(),
                });
            }
        }
        if let Some(role) = person
            .role
            .as_deref()
            .filter(|role| !role.trim().is_empty())
        {
            segments.push(Segment {
                id: format!("person_role:{index}"),
                text: role.to_string(),
            });
        }
    }

    let translations = if segments.is_empty() {
        HashMap::new()
    } else {
        let names = people
            .iter()
            .map(|person| person.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let instruction = render_template(&templates.people, &names, &segments, settings, "people");
        call_segments(translator, &segments, instruction, settings)
            .await
            .context("翻译演职员失败")?
    };

    let mut translated = Vec::with_capacity(people.len());
    for (index, person) in people.iter().enumerate() {
        let name = if let Some(target) = preset_hits.get(&index) {
            target.clone()
        } else if person.name.trim().is_empty() {
            person.name.clone()
        } else {
            translations
                .get(&format!("person_name:{index}"))
                .cloned()
                .ok_or_else(|| anyhow!("翻译服务未返回演职员 {index} 的姓名"))?
        };
        let role = match person.role.as_deref() {
            Some(role) if !role.trim().is_empty() => Some(
                translations
                    .get(&format!("person_role:{index}"))
                    .cloned()
                    .ok_or_else(|| anyhow!("翻译服务未返回演职员 {index} 的角色"))?,
            ),
            other => other.map(str::to_string),
        };
        translated.push(PersonRecord {
            id: person.id.clone(),
            name,
            role,
            person_type: person.person_type.clone(),
        });
    }

    Ok(TranslatedPeople {
        people: translated,
        preset_sources,
    })
}

async fn call_segments(
    translator: &dyn PromptTranslator,
    segments: &[Segment],
    prompt: Option<String>,
    settings: &ProviderSettings,
) -> Result<HashMap<String, String>> {
    if segments.is_empty() {
        return Ok(HashMap::new());
    }
    let request = TranslateRequest {
        segments: segments.to_vec(),
        source_lang: settings.source_lang.clone(),
        target_lang: settings.target_lang.clone(),
        glossary: Vec::new(),
    };
    let response = translator
        .translate_with_prompt(request, prompt.as_deref())
        .await?;
    let map = response
        .segments
        .into_iter()
        .map(|segment| (segment.id, segment.text))
        .collect::<HashMap<_, _>>();
    for segment in segments {
        anyhow::ensure!(
            map.contains_key(&segment.id),
            "翻译服务未返回字段 {} 的结果",
            segment.id
        );
    }
    Ok(map)
}

fn render_template(
    template: &str,
    text: &str,
    segments: &[Segment],
    settings: &ProviderSettings,
    field: &str,
) -> Option<String> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    let segments_json = serde_json::to_string(
        &segments
            .iter()
            .map(|segment| json!({ "id": segment.id, "text": segment.text }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());
    Some(
        template
            .replace("{{text}}", text)
            .replace("{{segments}}", &segments_json)
            .replace("{{source_lang}}", settings.source_lang.trim())
            .replace("{{target_lang}}", settings.target_lang.trim())
            .replace("{{field}}", field),
    )
}

fn source_text(primary: &Option<String>, draft: &Option<String>) -> String {
    primary
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or(draft.as_deref())
        .unwrap_or("")
        .to_string()
}

fn normalize_preset_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn record_translations(
    store: &mut Store,
    item: &ItemRecord,
    draft: &ItemDraft,
    settings: &ProviderSettings,
) -> Result<()> {
    let now = store::now_unix();
    let fields = [
        ("name", draft.name.clone()),
        ("original_title", draft.original_title.clone()),
        ("overview", draft.overview.clone()),
        ("tags", serde_json::to_string(&draft.tags)?),
        ("people", serde_json::to_string(&draft.people)?),
    ];
    for (field, text) in fields {
        store.upsert_translation(&TranslationRecord {
            item_id: item.id.clone(),
            field: field.to_string(),
            lang: settings.target_lang.clone(),
            source_hash: item.source_hash.clone(),
            text,
            provider: "openai".to_string(),
            model: Some(settings.model.clone()),
            status: store::STATUS_DRAFT.to_string(),
            updated_at: now,
        })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Write-back
// ---------------------------------------------------------------------------

async fn write_item(api: &dyn JellyfinApi, item: &ItemRecord) -> Result<Vec<String>> {
    let mut dto = api
        .item(&item.id)
        .await
        .with_context(|| format!("读取条目 {} 完整 DTO 失败", item.id))?;
    let object = dto
        .as_object_mut()
        .ok_or_else(|| anyhow!("条目 {} 的 DTO 不是 JSON 对象", item.id))?;

    let name = item.effective_name().trim().to_string();
    anyhow::ensure!(!name.is_empty(), "标题为空，拒绝写回");
    let original_title = effective_option(&item.draft_original_title, &item.original_title);
    let overview = effective_option(&item.draft_overview, &item.overview);
    let tags = item.effective_tags().to_vec();
    let people = item.effective_people().to_vec();

    object.insert("Name".to_string(), json!(name));
    if let Some(value) = original_title {
        object.insert("OriginalTitle".to_string(), json!(value));
    }
    if let Some(value) = overview {
        object.insert("Overview".to_string(), json!(value));
    }
    object.insert("Tags".to_string(), json!(tags));
    object.insert(
        "People".to_string(),
        Value::Array(merge_people_json(object.get("People"), &people)),
    );

    let locked_fields = merge_locked_fields(object.get("LockedFields"));
    object.insert("LockedFields".to_string(), json!(locked_fields));

    api.update_item(&item.id, &dto)
        .await
        .with_context(|| format!("写回条目 {} 失败", item.id))?;

    verify_written(api, item, &name, original_title, overview, &tags, &people).await?;
    Ok(locked_fields)
}

fn effective_option<'a>(draft: &'a Option<String>, source: &'a Option<String>) -> Option<&'a str> {
    draft.as_deref().or(source.as_deref())
}

/// Jellyfin `People` entries carry more than name/role (id, type, images).
/// Keep the existing entry for the same person and only overwrite the text.
fn merge_people_json(existing: Option<&Value>, people: &[PersonRecord]) -> Vec<Value> {
    let existing_people = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let by_id = existing_people
        .iter()
        .filter_map(|value| {
            value
                .get("Id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), value))
        })
        .collect::<HashMap<_, _>>();

    people
        .iter()
        .enumerate()
        .map(|(index, person)| {
            let base = person
                .id
                .as_deref()
                .and_then(|id| by_id.get(id).copied())
                .or_else(|| existing_people.get(index));
            let mut entry = base.and_then(Value::as_object).cloned().unwrap_or_default();
            entry.insert("Name".to_string(), json!(person.name));
            match &person.role {
                Some(role) => {
                    entry.insert("Role".to_string(), json!(role));
                }
                None => {
                    entry.remove("Role");
                }
            }
            if let Some(person_type) = &person.person_type {
                entry.insert("Type".to_string(), json!(person_type));
            } else {
                entry
                    .entry("Type".to_string())
                    .or_insert_with(|| json!("Actor"));
            }
            if let Some(id) = &person.id {
                entry.entry("Id".to_string()).or_insert_with(|| json!(id));
            }
            Value::Object(entry)
        })
        .collect()
}

fn merge_locked_fields(existing: Option<&Value>) -> Vec<String> {
    let mut fields = existing
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for field in SERVER_LOCKED_FIELDS {
        if !fields.iter().any(|value| value.eq_ignore_ascii_case(field)) {
            fields.push((*field).to_string());
        }
    }
    fields
}

async fn verify_written(
    api: &dyn JellyfinApi,
    item: &ItemRecord,
    expected_name: &str,
    expected_original_title: Option<&str>,
    expected_overview: Option<&str>,
    expected_tags: &[String],
    expected_people: &[PersonRecord],
) -> Result<()> {
    let after = api
        .item(&item.id)
        .await
        .with_context(|| format!("读回条目 {} 失败", item.id))?;

    let actual_name = after.get("Name").and_then(Value::as_str).unwrap_or("");
    anyhow::ensure!(
        actual_name == expected_name,
        "读回校验失败：Name 期望 {expected_name:?}，实际 {actual_name:?}"
    );

    if let Some(expected) = expected_original_title {
        let actual = after
            .get("OriginalTitle")
            .and_then(Value::as_str)
            .unwrap_or("");
        anyhow::ensure!(
            actual == expected,
            "读回校验失败：OriginalTitle 期望 {expected:?}，实际 {actual:?}"
        );
    }

    if let Some(expected) = expected_overview {
        let actual = after.get("Overview").and_then(Value::as_str).unwrap_or("");
        anyhow::ensure!(
            actual == expected,
            "读回校验失败：Overview 期望 {expected:?}，实际 {actual:?}"
        );
    }

    let actual_tags = after
        .get("Tags")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    anyhow::ensure!(
        actual_tags == expected_tags,
        "读回校验失败：Tags 期望 {expected_tags:?}，实际 {actual_tags:?}"
    );

    let actual_people = after
        .get("People")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    (
                        value
                            .get("Name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        value
                            .get("Role")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected = expected_people
        .iter()
        .map(|person| (person.name.clone(), person.role.clone()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        actual_people == expected,
        "读回校验失败：People 期望 {expected:?}，实际 {actual_people:?}"
    );

    Ok(())
}

/// Create `path` with private permissions (0600 on Unix) and flush it.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("创建 {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("写入 {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("同步 {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_templates() -> Templates {
    Templates {
        title: "你是影视元数据翻译。把下面的电影/剧集标题翻译成{{target_lang}}，保留专有名词的通用译法，只输出译文，不要解释。\n\n原文：{{text}}".to_string(),
        overview: "你是影视简介翻译。把下面的剧情简介翻译成{{target_lang}}，保持语气与信息完整，不加评论，不改写剧情。\n\n原文：{{text}}".to_string(),
        tags: "把下面的影视标签逐个翻译成{{target_lang}}，保持数量与顺序一致，只输出译文。\n\n{{segments}}".to_string(),
        people: "把下面的演职员信息翻译成{{target_lang}}：姓名使用通用译名，角色名翻译成目标语言。\n\n{{segments}}".to_string(),
    }
}

#[cfg(test)]
mod tests;
