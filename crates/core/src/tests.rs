//! Unit tests for the service. Everything runs against in-memory SQLite and a
//! fake Jellyfin API / translation provider — no network, no real server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::api::{BoxFuture, JellyfinApi, PromptTranslator};
use super::*;
use translate::{TranslatedSegment, Usage};

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

struct FakeApi {
    libraries: Vec<jellyfin::Library>,
    items: Vec<(String, jellyfin::ItemDto)>,
    dtos: Mutex<HashMap<String, Value>>,
    updates: Mutex<Vec<(String, Value)>>,
    /// Fields the simulated server refuses to persist (for verification tests).
    ignored_update_fields: Mutex<Vec<String>>,
    /// Simulate a server that rejects DTOs containing this `LockedFields` value.
    rejected_locked_field: Mutex<Option<String>>,
    image_calls: Mutex<Vec<(String, String, u32)>>,
    users: Mutex<Vec<jellyfin::UserDto>>,
    item_routes: Mutex<Vec<String>>,
    user_item_status: Mutex<Option<u16>>,
    legacy_item_status: Mutex<Option<u16>>,
}

impl FakeApi {
    fn new(
        libraries: Vec<jellyfin::Library>,
        items: Vec<(String, jellyfin::ItemDto)>,
    ) -> Arc<Self> {
        let dtos = items
            .iter()
            .map(|(_, item)| (item.id.clone(), dto_json(item)))
            .collect();
        Arc::new(Self {
            libraries,
            items,
            dtos: Mutex::new(dtos),
            updates: Mutex::new(Vec::new()),
            ignored_update_fields: Mutex::new(Vec::new()),
            rejected_locked_field: Mutex::new(None),
            image_calls: Mutex::new(Vec::new()),
            users: Mutex::new(Vec::new()),
            item_routes: Mutex::new(Vec::new()),
            user_item_status: Mutex::new(None),
            legacy_item_status: Mutex::new(None),
        })
    }

    fn ignore_update_field(&self, field: &str) {
        self.ignored_update_fields
            .lock()
            .unwrap()
            .push(field.to_string());
    }

    fn reject_locked_field(&self, field: &str) {
        *self.rejected_locked_field.lock().unwrap() = Some(field.to_string());
    }

    fn set_users(&self, users: &[(&str, &str)]) {
        *self.users.lock().unwrap() = users
            .iter()
            .map(|(id, name)| jellyfin::UserDto {
                id: (*id).to_string(),
                name: (*name).to_string(),
            })
            .collect();
    }

    fn set_user_item_status(&self, status: Option<u16>) {
        *self.user_item_status.lock().unwrap() = status;
    }

    fn set_legacy_item_status(&self, status: Option<u16>) {
        *self.legacy_item_status.lock().unwrap() = status;
    }

    fn item_routes(&self) -> Vec<String> {
        self.item_routes.lock().unwrap().clone()
    }

    fn updates(&self) -> Vec<(String, Value)> {
        self.updates.lock().unwrap().clone()
    }

    fn image_calls(&self) -> Vec<(String, String, u32)> {
        self.image_calls.lock().unwrap().clone()
    }
}

fn fake_api_error(status: u16, context: &str) -> anyhow::Error {
    jellyfin::ApiError {
        status: Some(status),
        context: context.to_string(),
        body: "fake server error".to_string(),
    }
    .into()
}

fn dto_json(item: &jellyfin::ItemDto) -> Value {
    json!({
        "Id": item.id,
        "Name": item.name,
        "OriginalTitle": item.original_title,
        "Overview": item.overview,
        "Tags": item.tags,
        "People": item.people.as_ref().map(|people| people.iter().map(|person| json!({
            "Id": person.id,
            "Name": person.name,
            "Role": person.role,
            "Type": person.person_type,
        })).collect::<Vec<_>>()),
        "ProductionYear": 1994,
        "Genres": item.genres,
        "SortName": item.sort_name,
        "LockedFields": ["Runtime"],
        "Type": item.item_type,
    })
}

impl JellyfinApi for FakeApi {
    fn system_info<'a>(&'a self) -> BoxFuture<'a, Result<ServerInfo>> {
        Box::pin(async move {
            Ok(ServerInfo {
                id: "server".into(),
                server_name: "media.home".into(),
                version: "12.0.0".into(),
            })
        })
    }

    fn libraries<'a>(&'a self) -> BoxFuture<'a, Result<Vec<jellyfin::Library>>> {
        Box::pin(async move { Ok(self.libraries.clone()) })
    }

    fn users<'a>(&'a self) -> BoxFuture<'a, Result<Vec<jellyfin::UserDto>>> {
        Box::pin(async move { Ok(self.users.lock().unwrap().clone()) })
    }

    fn items_page<'a>(
        &'a self,
        parent_id: Option<&'a str>,
        include_types: &'a [String],
        start: usize,
        limit: usize,
    ) -> BoxFuture<'a, Result<jellyfin::ItemsPage>> {
        Box::pin(async move {
            let parent = parent_id.unwrap_or_default();
            let mut items = self
                .items
                .iter()
                .filter(|(library_id, item)| {
                    library_id == parent
                        && (include_types.is_empty()
                            || include_types
                                .iter()
                                .any(|kind| item.item_type.as_deref() == Some(kind.as_str())))
                })
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            items.sort_by(|left, right| left.name.cmp(&right.name));
            let total_record_count = items.len() as i64;
            let page = items
                .into_iter()
                .skip(start)
                .take(limit)
                .collect::<Vec<_>>();
            Ok(jellyfin::ItemsPage {
                items: page,
                total_record_count,
            })
        })
    }

    fn item<'a>(&'a self, item_id: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            self.item_routes
                .lock()
                .unwrap()
                .push(format!("legacy:{item_id}"));
            if let Some(status) = *self.legacy_item_status.lock().unwrap() {
                return Err(fake_api_error(status, &format!("GET /Items/{item_id}")));
            }
            self.dtos
                .lock()
                .unwrap()
                .get(item_id)
                .cloned()
                .ok_or_else(|| anyhow!("fake api: unknown item {item_id}"))
        })
    }

    fn item_for_user<'a>(
        &'a self,
        user_id: &'a str,
        item_id: &'a str,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            self.item_routes
                .lock()
                .unwrap()
                .push(format!("user:{user_id}:{item_id}"));
            if let Some(status) = *self.user_item_status.lock().unwrap() {
                return Err(fake_api_error(
                    status,
                    &format!("GET /Users/{user_id}/Items/{item_id}"),
                ));
            }
            self.dtos
                .lock()
                .unwrap()
                .get(item_id)
                .cloned()
                .ok_or_else(|| anyhow!("fake api: unknown item {item_id}"))
        })
    }

    fn update_item<'a>(&'a self, item_id: &'a str, dto: &'a Value) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // Record every attempt (including ones the fake server rejects), so
            // retry behaviour can be asserted.
            self.updates
                .lock()
                .unwrap()
                .push((item_id.to_string(), dto.clone()));

            if let Some(rejected) = self.rejected_locked_field.lock().unwrap().as_deref() {
                let contains = dto
                    .get("LockedFields")
                    .and_then(Value::as_array)
                    .map(|values| values.iter().any(|value| value.as_str() == Some(rejected)))
                    .unwrap_or(false);
                if contains {
                    return Err(fake_api_error(
                        400,
                        &format!("POST /Items/{item_id} (LockedFields {rejected})"),
                    ));
                }
            }

            let mut stored = self.dtos.lock().unwrap();
            let old = stored.get(item_id).cloned().unwrap_or(Value::Null);

            // Simulate a server that silently drops some fields.
            let mut applied = dto.clone();
            for field in self.ignored_update_fields.lock().unwrap().iter() {
                if let (Some(object), Some(old_value)) = (applied.as_object_mut(), old.get(field)) {
                    object.insert(field.clone(), old_value.clone());
                }
            }
            stored.insert(item_id.to_string(), applied);
            Ok(())
        })
    }

    fn image<'a>(
        &'a self,
        item_id: &'a str,
        image_type: &'a str,
        max_width: u32,
    ) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(async move {
            self.image_calls.lock().unwrap().push((
                item_id.to_string(),
                image_type.to_string(),
                max_width,
            ));
            Ok(format!("image:{item_id}:{image_type}:{max_width}").into_bytes())
        })
    }
}

struct EchoTranslator {
    prompts: Mutex<Vec<String>>,
}

impl EchoTranslator {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            prompts: Mutex::new(Vec::new()),
        })
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

impl PromptTranslator for EchoTranslator {
    fn translate_with_prompt<'a>(
        &'a self,
        request: translate::TranslateRequest,
        prompt: Option<&'a str>,
    ) -> BoxFuture<'a, Result<translate::TranslateResponse>> {
        Box::pin(async move {
            self.prompts
                .lock()
                .unwrap()
                .push(prompt.unwrap_or_default().to_string());
            let lang = request.target_lang.clone();
            Ok(translate::TranslateResponse {
                segments: request
                    .segments
                    .into_iter()
                    .map(|segment| TranslatedSegment {
                        id: segment.id,
                        text: format!("[{lang}] {}", segment.text),
                    })
                    .collect(),
                usage: Usage::default(),
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Echoes the input like [`EchoTranslator`], but holds each request for a short
/// while and records how many were in flight at once, so a test can prove that
/// `ProviderSettings.concurrency` really parallelizes item translation.
#[derive(Default)]
struct ConcurrencyProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl ConcurrencyProbe {
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

impl PromptTranslator for ConcurrencyProbe {
    fn translate_with_prompt<'a>(
        &'a self,
        request: translate::TranslateRequest,
        _prompt: Option<&'a str>,
    ) -> BoxFuture<'a, Result<translate::TranslateResponse>> {
        Box::pin(async move {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);

            let lang = request.target_lang.clone();
            Ok(translate::TranslateResponse {
                segments: request
                    .segments
                    .into_iter()
                    .map(|segment| TranslatedSegment {
                        id: segment.id,
                        text: format!("[{lang}] {}", segment.text),
                    })
                    .collect(),
                usage: Usage::default(),
            })
        })
    }
}

fn library(id: &str, name: &str) -> jellyfin::Library {
    jellyfin::Library {
        id: id.to_string(),
        name: name.to_string(),
        collection_type: Some("movies".into()),
    }
}

fn movie(id: &str, name: &str) -> jellyfin::ItemDto {
    jellyfin::ItemDto {
        id: id.to_string(),
        name: Some(name.to_string()),
        original_title: Some(name.to_string()),
        sort_name: Some(name.to_string()),
        genres: Some(vec!["Drama".into()]),
        overview: Some(format!("Overview of {name}")),
        tags: Some(vec!["Sci-Fi".into(), "Heist".into()]),
        people: Some(vec![
            jellyfin::PersonDto {
                id: Some("p1".into()),
                name: "Leonardo DiCaprio".into(),
                role: Some("Dom Cobb".into()),
                person_type: Some("Actor".into()),
            },
            jellyfin::PersonDto {
                id: Some("p2".into()),
                name: "Tom Hanks".into(),
                role: Some("Narrator".into()),
                person_type: Some("Actor".into()),
            },
        ]),
        item_type: Some("Movie".into()),
        ..Default::default()
    }
}

fn settings() -> ProviderSettings {
    ProviderSettings {
        api_key: "test-key".into(),
        ..ProviderSettings::default()
    }
}

fn templates() -> Templates {
    Templates {
        title: "标题模板 {{text}} -> {{target_lang}}".into(),
        overview: "简介模板 {{text}}".into(),
        tags: "标签模板 {{segments}}".into(),
        people: "演职员模板 {{segments}}".into(),
    }
}

fn temp_data_dir(label: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "jellyfin-translate-core-test-{label}-{}-{nanos}",
        std::process::id()
    ))
}

async fn synced_service(fake: &Arc<FakeApi>) -> AppService {
    let service = AppService::open_in_memory().unwrap();
    service.install_api(fake.clone()).unwrap();
    service.sync_all(|_| {}).await.unwrap();
    service
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn service_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AppService>();
}

#[test]
fn open_creates_database_and_seeds_settings() {
    let dir = temp_data_dir("open");

    let service = AppService::open_at(&dir).unwrap();
    assert_eq!(service.data_dir_path(), dir);
    assert!(dir.join("library.db").exists());

    let templates = service.templates().unwrap();
    assert!(templates.title.contains("{{text}}"));
    assert!(templates.people.contains("{{segments}}"));
    assert_eq!(service.provider_settings().unwrap().target_lang, "zh-CN");

    service
        .save_templates(&Templates {
            title: "only-title".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(service.templates().unwrap().title, "only-title");
    assert!(service.templates().unwrap().overview.is_empty());

    let mut provider = service.provider_settings().unwrap();
    provider.api_key = "secret".into();
    provider.model = "test-model".into();
    service.save_provider_settings(&provider).unwrap();
    assert_eq!(service.provider_settings().unwrap().api_key, "secret");

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn data_dir_ends_with_data() {
    assert!(AppService::data_dir().ends_with("data"));
}

#[tokio::test]
async fn connect_validates_locally_before_any_request() {
    let service = AppService::open_in_memory().unwrap();
    assert!(service.connect("not-a-url", "key").await.is_err());
    assert!(service.connect("ftp://host", "key").await.is_err());
    assert!(service.connect("http://host", "").await.is_err());
}

#[tokio::test]
async fn sync_pages_through_libraries_and_reports_progress() {
    let mut items = (0..250)
        .map(|index| {
            (
                "movies".to_string(),
                movie(&format!("m{index:03}"), &format!("Movie {index:03}")),
            )
        })
        .collect::<Vec<_>>();
    items.push((
        "shows".to_string(),
        jellyfin::ItemDto {
            id: "s1".into(),
            name: Some("Breaking Bad".into()),
            item_type: Some("Series".into()),
            ..Default::default()
        },
    ));
    let fake = FakeApi::new(
        vec![library("movies", "Movies"), library("shows", "Shows")],
        items,
    );
    let service = AppService::open_in_memory().unwrap();
    service.install_api(fake.clone()).unwrap();

    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = progress.clone();
    let report = service
        .sync_all(move |snapshot| sink.lock().unwrap().push(snapshot))
        .await
        .unwrap();

    assert_eq!(report.libraries, 2);
    assert_eq!(report.items, 251);

    let snapshots = progress.lock().unwrap();
    assert!(snapshots.len() >= 2);
    let last = snapshots.last().unwrap();
    assert_eq!(last.libraries_done, 2);
    assert_eq!(last.libraries_total, 2);
    assert_eq!(last.items, 251);

    let libraries = service.libraries().unwrap();
    assert_eq!(libraries.len(), 2);
    let movies = libraries.iter().find(|lib| lib.id == "movies").unwrap();
    let shows = libraries.iter().find(|lib| lib.id == "shows").unwrap();
    assert_eq!(movies.item_count, 250);
    assert_eq!(shows.item_count, 1);
    assert_eq!(service.items("movies").unwrap().len(), 250);
    assert_eq!(service.items("shows").unwrap().len(), 1);
    assert!(service.items("missing").unwrap().is_empty());

    let first = &service.items("movies").unwrap()[0];
    assert_eq!(first.status, TranslationStatus::Untranslated);
    assert!(!first.locked);
    assert_eq!(first.tags, vec!["Sci-Fi".to_string(), "Heist".to_string()]);
    assert_eq!(first.people.len(), 2);
    assert_eq!(first.people[0].id.as_deref(), Some("p1"));
    assert_eq!(first.people[1].id.as_deref(), Some("p2"));
    // Source is the untouched server snapshot.
    assert_eq!(first.source.title, first.title);
    assert_eq!(first.source.overview, first.overview);
    assert_eq!(first.source.tags, first.tags);
    assert_eq!(first.source.people[0].id.as_deref(), Some("p1"));
    // Genres/SortName are synced from Jellyfin's Genres/SortName fields.
    assert_eq!(first.sort_name, "Movie 000");
    assert_eq!(first.genres, vec!["Drama".to_string()]);
    assert_eq!(first.source.sort_name, "Movie 000");
    assert_eq!(first.source.genres, vec!["Drama".to_string()]);
}

#[tokio::test]
async fn translate_stores_drafts_uses_presets_and_skips_locked() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service
        .upsert_preset(
            PresetKind::Person,
            "  leonardo dicaprio ",
            "莱昂纳多·迪卡普里奥",
        )
        .unwrap();

    let translator = EchoTranslator::new();
    service.install_translator(translator.clone()).unwrap();

    let report = service
        .translate(vec!["m1".into()], templates(), settings())
        .await
        .unwrap();
    assert_eq!(report.translated, 1);
    assert!(report.failed.is_empty());

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Draft);
    assert_eq!(item.title, "[zh-CN] Inception");
    assert_eq!(item.original_title, "[zh-CN] Inception");
    assert_eq!(item.overview, "[zh-CN] Overview of Inception");
    assert_eq!(
        item.tags,
        vec!["[zh-CN] Sci-Fi".to_string(), "[zh-CN] Heist".to_string()]
    );
    assert_eq!(item.people[0].id.as_deref(), Some("p1"));
    assert_eq!(item.people[0].name, "莱昂纳多·迪卡普里奥");
    assert_eq!(item.people[0].role.as_deref(), Some("[zh-CN] Dom Cobb"));
    assert_eq!(item.people[1].name, "[zh-CN] Tom Hanks");
    assert_eq!(item.people[1].role.as_deref(), Some("[zh-CN] Narrator"));
    // Effective values are the draft; source keeps the original English text.
    assert_eq!(item.source.title, "Inception");
    assert_eq!(item.source.overview, "Overview of Inception");
    assert_eq!(item.source.tags, vec!["Sci-Fi", "Heist"]);
    assert_eq!(item.source.people[0].name, "Leonardo DiCaprio");
    assert_eq!(item.source.people[0].role.as_deref(), Some("Dom Cobb"));

    // Preset usage counter moved, and the field instructions were rendered.
    assert_eq!(service.presets().unwrap()[0].uses, 1);
    let prompts = translator.prompts();
    assert!(!prompts.is_empty());
    assert!(prompts
        .iter()
        .any(|prompt| prompt.contains("标题模板") && prompt.contains("zh-CN")));
    assert!(prompts.iter().any(|prompt| prompt.contains("演职员模板")));

    // The audit table recorded the draft translations.
    let translations = {
        let inner = service.inner().unwrap();
        inner.store.list_translations("m1").unwrap()
    };
    assert!(translations.iter().any(|row| row.field == "name"));
    assert!(translations.iter().any(|row| row.field == "people"));

    // Locked items are skipped without touching their draft.
    service.set_locked(&["m1".into()], true).unwrap();
    let report = service
        .translate(vec!["m1".into()], templates(), settings())
        .await
        .unwrap();
    assert_eq!(report.translated, 0);
    assert!(service.items("movies").unwrap()[0].locked);
}

#[tokio::test]
async fn translate_reports_missing_items_and_requires_api_key() {
    let service = AppService::open_in_memory().unwrap();
    service.install_translator(EchoTranslator::new()).unwrap();
    let report = service
        .translate(vec!["nope".into()], templates(), settings())
        .await
        .unwrap();
    assert_eq!(report.translated, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].1.contains("不存在"));

    // Without the test translator the real OpenAI-compatible provider is built
    // and an empty API key is rejected before any request.
    let plain = AppService::open_in_memory().unwrap();
    let error = plain
        .translate(vec!["x".into()], templates(), ProviderSettings::default())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("API key"), "unexpected error: {error}");
}

#[tokio::test]
async fn translate_processes_multiple_items_concurrently() {
    let movies = (1..=6)
        .map(|i| {
            (
                "movies".to_string(),
                movie(&format!("m{i}"), &format!("Movie {i}")),
            )
        })
        .collect::<Vec<_>>();
    let fake = FakeApi::new(vec![library("movies", "Movies")], movies);
    let service = synced_service(&fake).await;

    let probe = Arc::new(ConcurrencyProbe::default());
    service.install_translator(probe.clone()).unwrap();

    let mut provider = settings();
    provider.concurrency = 3;
    let report = service
        .translate(
            (1..=6).map(|i| format!("m{i}")).collect(),
            templates(),
            provider,
        )
        .await
        .unwrap();

    assert_eq!(report.translated, 6);
    assert!(report.failed.is_empty());
    assert!(
        probe.peak() > 1,
        "expected concurrent item translations, peak was {}",
        probe.peak()
    );

    for i in 1..=6 {
        let item = service
            .items("movies")
            .unwrap()
            .into_iter()
            .find(|item| item.id == format!("m{i}"))
            .unwrap();
        assert_eq!(item.title, format!("[zh-CN] Movie {i}"));
        assert_eq!(item.status, TranslationStatus::Draft);
    }
}

#[tokio::test]
async fn save_draft_downgrades_written_to_draft_and_rejects_locked() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    {
        let mut inner = service.inner().unwrap();
        inner
            .store
            .mark_written("m1", &["Name".to_string()])
            .unwrap();
    }
    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "盗梦空间 排序".into(),
                genres: vec!["剧情".into()],
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into()],
                people: vec![PersonView {
                    id: None,
                    name: "莱昂纳多".into(),
                    role: Some("科布".into()),
                }],
            },
        )
        .unwrap();

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Draft);
    assert_eq!(item.title, "盗梦空间");
    assert_eq!(item.sort_name, "盗梦空间 排序");
    assert_eq!(item.genres, vec!["剧情".to_string()]);
    assert_eq!(item.tags, vec!["科幻".to_string()]);

    service.set_locked(&["m1".into()], true).unwrap();
    let error = service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: String::new(),
                genres: Vec::new(),
                title: "x".into(),
                original_title: String::new(),
                overview: String::new(),
                tags: Vec::new(),
                people: Vec::new(),
            },
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("锁定"), "unexpected error: {error}");
}

#[tokio::test]
async fn write_back_updates_metadata_only_and_locks_fields() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "Inception".into(),
                genres: vec!["Drama".into()],
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into(), "犯罪".into()],
                people: vec![
                    PersonView {
                        id: Some("p1".into()),
                        name: "莱昂纳多".into(),
                        role: Some("科布".into()),
                    },
                    PersonView {
                        id: Some("p2".into()),
                        name: "汤姆·汉克斯".into(),
                        role: Some("旁白".into()),
                    },
                ],
            },
        )
        .unwrap();

    let report = service.write_back(vec!["m1".into()]).await.unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let updates = fake.updates();
    assert_eq!(updates.len(), 1);
    let sent = &updates[0].1;
    assert_eq!(sent["Name"], "盗梦空间");
    assert_eq!(sent["OriginalTitle"], "Inception");
    assert_eq!(sent["Overview"], "简介");
    assert_eq!(sent["Tags"], json!(["科幻", "犯罪"]));
    assert_eq!(sent["ProductionYear"], 1994);
    assert_eq!(sent["Genres"], json!(["Drama"]));
    assert_eq!(sent["People"][0]["Name"], "莱昂纳多");
    assert_eq!(sent["People"][0]["Role"], "科布");
    assert_eq!(sent["People"][0]["Id"], "p1");
    assert_eq!(sent["People"][0]["Type"], "Actor");

    let locked = sent["LockedFields"].as_array().unwrap();
    let locked = locked.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    for field in ["Runtime", "Name", "Overview", "Tags", "Cast"] {
        assert!(locked.contains(&field), "missing locked field {field}");
    }

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Written);
    assert_eq!(item.title, "盗梦空间");
}

#[tokio::test]
async fn write_back_detects_a_server_that_ignored_a_field() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.ignore_update_field("Overview");
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "盗梦空间 排序".into(),
                genres: vec!["剧情".into()],
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into()],
                people: Vec::new(),
            },
        )
        .unwrap();

    let report = service.write_back(vec!["m1".into()]).await.unwrap();
    assert_eq!(report.written, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(report.failed[0].1.contains("读回校验失败"));
    assert!(report.failed[0].1.contains("Overview"));
    assert_eq!(
        service.items("movies").unwrap()[0].status,
        TranslationStatus::Draft
    );
}

#[tokio::test]
async fn write_back_skips_locked_items() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service.set_locked(&["m1".into()], true).unwrap();

    let report = service.write_back(vec!["m1".into()]).await.unwrap();
    assert_eq!(report.written, 0);
    assert!(report.failed.is_empty());
    assert!(fake.updates().is_empty());
}

#[tokio::test]
async fn write_back_requires_a_connection() {
    let service = AppService::open_in_memory().unwrap();
    let error = service
        .write_back(vec!["m1".into()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("尚未连接"), "unexpected error: {error}");
}

#[tokio::test]
async fn presets_crud_roundtrip_per_kind() {
    let service = AppService::open_in_memory().unwrap();
    service
        .upsert_preset(PresetKind::Person, " Tom Hanks ", "汤姆·汉克斯")
        .unwrap();
    // Same kind + normalized source updates the existing entry.
    service
        .upsert_preset(PresetKind::Person, "tom hanks", "汤姆汉克斯")
        .unwrap();
    // Same source under other kinds is a separate entry.
    service
        .upsert_preset(PresetKind::Tag, "Tom Hanks", "汉克斯标签")
        .unwrap();
    service
        .upsert_preset(PresetKind::Genre, "Drama", "剧情")
        .unwrap();

    let presets = service.presets().unwrap();
    assert_eq!(presets.len(), 3);
    let person = presets
        .iter()
        .find(|preset| preset.kind == PresetKind::Person)
        .unwrap();
    assert_eq!(person.target, "汤姆汉克斯");

    service
        .delete_preset(PresetKind::Tag, " tom hanks")
        .unwrap();
    let presets = service.presets().unwrap();
    assert_eq!(presets.len(), 2);
    assert!(presets.iter().all(|preset| preset.kind != PresetKind::Tag));

    service
        .delete_preset(PresetKind::Person, "TOM HANKS")
        .unwrap();
    let presets = service.presets().unwrap();
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].kind, PresetKind::Genre);

    // Serde shape used by the UI.
    assert_eq!(
        serde_json::to_value(PresetKind::Person).unwrap(),
        json!("person")
    );
    assert_eq!(serde_json::to_value(PresetKind::Tag).unwrap(), json!("tag"));
    assert_eq!(
        serde_json::to_value(PresetKind::Genre).unwrap(),
        json!("genre")
    );
}

#[tokio::test]
async fn cover_fetches_primary_images_from_the_connected_client() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    let poster = service.cover("m1", 320).await.unwrap();
    assert_eq!(poster, b"image:m1:Primary:320");
    let avatar = service.person_cover("p1", 96).await.unwrap();
    assert_eq!(avatar, b"image:p1:Primary:96");

    assert_eq!(
        fake.image_calls(),
        vec![
            ("m1".to_string(), "Primary".to_string(), 320),
            ("p1".to_string(), "Primary".to_string(), 96),
        ]
    );
}

#[tokio::test]
async fn images_require_a_connection() {
    let service = AppService::open_in_memory().unwrap();
    for error in [
        service.cover("m1", 100).await.unwrap_err().to_string(),
        service
            .person_cover("p1", 100)
            .await
            .unwrap_err()
            .to_string(),
    ] {
        assert!(
            error.contains("尚未连接 Jellyfin 服务器"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn person_view_id_is_optional_for_backwards_compatible_json() {
    let view: PersonView = serde_json::from_str(r#"{"name":"Actor","role":null}"#).unwrap();
    assert!(view.id.is_none());
    assert_eq!(view.name, "Actor");

    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json["id"], Value::Null);
}

#[test]
fn jellyfin_config_roundtrip_uses_atomic_write() {
    let dir = temp_data_dir("config-roundtrip");
    let service = AppService::open_at(&dir).unwrap();

    let config = JellyfinConfig {
        url: "http://media.home:8096".into(),
        api_key: "secret".into(),
    };
    service.save_jellyfin_config(&config).unwrap();
    assert_eq!(service.jellyfin_config().unwrap(), config);
    assert_eq!(service.config_path(), dir.join("config.json"));

    // Saving again replaces the file atomically.
    let replaced = JellyfinConfig {
        url: "https://other.example".into(),
        api_key: "key-2".into(),
    };
    service.save_jellyfin_config(&replaced).unwrap();
    assert_eq!(service.jellyfin_config().unwrap(), replaced);

    let raw = std::fs::read_to_string(dir.join("config.json")).unwrap();
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["jellyfin"]["url"], "https://other.example");
    assert_eq!(json["jellyfin"]["api_key"], "key-2");

    // The temp file was renamed, not left behind.
    let leftovers = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("config.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn jellyfin_config_defaults_when_missing_or_corrupt() {
    let dir = temp_data_dir("config-default");
    let service = AppService::open_at(&dir).unwrap();

    // Missing file.
    assert_eq!(
        service.jellyfin_config().unwrap(),
        JellyfinConfig::default()
    );

    // Invalid JSON.
    std::fs::write(dir.join("config.json"), b"{ this is not json").unwrap();
    assert_eq!(
        service.jellyfin_config().unwrap(),
        JellyfinConfig::default()
    );

    // Partially populated JSON keeps what is there, defaults the rest.
    std::fs::write(
        dir.join("config.json"),
        br#"{"jellyfin":{"url":"http://media.home:8096"}}"#,
    )
    .unwrap();
    let config = service.jellyfin_config().unwrap();
    assert_eq!(config.url, "http://media.home:8096");
    assert!(config.api_key.is_empty());

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn connect_persists_credentials_after_success() {
    let dir = temp_data_dir("connect-persist");
    let service = AppService::open_at(&dir).unwrap();

    let fake = FakeApi::new(vec![], vec![]);
    service
        .install_client_factory(Arc::new(move |_url, _key| {
            Ok(fake.clone() as Arc<dyn JellyfinApi>)
        }))
        .unwrap();

    let info = service
        .connect("  http://media.home:8096  ", " secret ")
        .await
        .unwrap();
    assert_eq!(info.server_name, "media.home");

    let config = service.jellyfin_config().unwrap();
    assert_eq!(config.url, "http://media.home:8096");
    assert_eq!(config.api_key, "secret");

    // The connected fake client was installed and is usable immediately.
    let bytes = service.cover("m1", 64).await.unwrap();
    assert_eq!(bytes, b"image:m1:Primary:64");

    let raw = std::fs::read_to_string(dir.join("config.json")).unwrap();
    let json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["jellyfin"]["url"], "http://media.home:8096");
    assert_eq!(json["jellyfin"]["api_key"], "secret");

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

fn series(id: &str, name: &str) -> jellyfin::ItemDto {
    jellyfin::ItemDto {
        id: id.to_string(),
        name: Some(name.to_string()),
        item_type: Some("Series".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn remote_libraries_lists_server_libraries_with_local_counts() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies"), library("shows", "Shows")],
        vec![
            ("movies".to_string(), movie("m1", "Inception")),
            ("shows".to_string(), series("s1", "Breaking Bad")),
        ],
    );
    let service = AppService::open_in_memory().unwrap();
    service.install_api(fake).unwrap();

    // Remote listing works before any sync and reports zero local items; it
    // must not persist libraries as a side effect.
    let remote = service.remote_libraries().await.unwrap();
    assert_eq!(remote.len(), 2);
    assert_eq!(remote[0].id, "movies");
    assert_eq!(remote[0].name, "Movies");
    assert_eq!(remote[0].item_count, 0);
    assert_eq!(remote[1].item_count, 0);
    assert!(service.libraries().unwrap().is_empty());

    service.sync_all(|_| {}).await.unwrap();
    let remote = service.remote_libraries().await.unwrap();
    let movies = remote
        .iter()
        .find(|library| library.id == "movies")
        .unwrap();
    let shows = remote.iter().find(|library| library.id == "shows").unwrap();
    assert_eq!(movies.item_count, 1);
    assert_eq!(shows.item_count, 1);
}

#[tokio::test]
async fn sync_libraries_only_syncs_selected_ids_and_ignores_unknown() {
    let fake = FakeApi::new(
        vec![
            library("movies", "Movies"),
            library("shows", "Shows"),
            library("docs", "Docs"),
        ],
        vec![
            ("movies".to_string(), movie("m1", "Inception")),
            ("movies".to_string(), movie("m2", "Interstellar")),
            ("shows".to_string(), series("s1", "Breaking Bad")),
        ],
    );
    let service = AppService::open_in_memory().unwrap();
    service.install_api(fake).unwrap();

    let progress = Arc::new(Mutex::new(Vec::new()));
    let sink = progress.clone();
    let report = service
        .sync_libraries(
            vec![
                "shows".to_string(),
                "does-not-exist".to_string(),
                "shows".to_string(),
            ],
            move |snapshot| sink.lock().unwrap().push(snapshot),
        )
        .await
        .unwrap();

    assert_eq!(report.libraries, 1);
    assert_eq!(report.items, 1);
    assert!(service.items("movies").unwrap().is_empty());
    assert!(service.items("docs").unwrap().is_empty());
    assert_eq!(service.items("shows").unwrap().len(), 1);
    assert_eq!(service.items("shows").unwrap()[0].title, "Breaking Bad");

    // Only the selected library is persisted locally.
    let libraries = service.libraries().unwrap();
    assert_eq!(libraries.len(), 1);
    assert_eq!(libraries[0].id, "shows");
    assert_eq!(libraries[0].item_count, 1);

    {
        let snapshots = progress.lock().unwrap();
        let last = snapshots.last().unwrap();
        assert_eq!(last.libraries_done, 1);
        assert_eq!(last.libraries_total, 1);
        assert_eq!(last.items, 1);
    }

    // An unknown-only selection is a no-op, not an error.
    let report = service
        .sync_libraries(vec!["nope".to_string()], |_| {})
        .await
        .unwrap();
    assert_eq!(report.libraries, 0);
    assert_eq!(report.items, 0);
}

#[tokio::test]
async fn remote_and_selected_sync_require_a_connection() {
    let service = AppService::open_in_memory().unwrap();
    let errors = [
        service.remote_libraries().await.unwrap_err().to_string(),
        service
            .sync_libraries(vec!["movies".to_string()], |_| {})
            .await
            .unwrap_err()
            .to_string(),
    ];
    for error in errors {
        assert!(
            error.contains("尚未连接 Jellyfin 服务器"),
            "unexpected error: {error}"
        );
    }
}

#[tokio::test]
async fn translate_fields_only_translates_selected_fields() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    let translator = EchoTranslator::new();
    service.install_translator(translator.clone()).unwrap();

    let report = service
        .translate_fields(
            "m1",
            vec![FieldKind::Overview, FieldKind::Overview],
            templates(),
            settings(),
        )
        .await
        .unwrap();
    assert_eq!(report.translated, 1);
    assert!(report.failed.is_empty());

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Draft);
    assert_eq!(item.overview, "[zh-CN] Overview of Inception");
    // Unselected fields are untouched.
    assert_eq!(item.title, "Inception");
    assert_eq!(item.original_title, "Inception");
    assert_eq!(item.tags, vec!["Sci-Fi", "Heist"]);
    assert_eq!(item.people[0].name, "Leonardo DiCaprio");
    // Source still holds the server originals.
    assert_eq!(item.source.title, "Inception");
    assert_eq!(item.source.overview, "Overview of Inception");

    // Duplicate selections are deduped: one provider request, one audit row.
    let prompts = translator.prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("简介模板"));
    let rows = {
        let inner = service.inner().unwrap();
        inner.store.list_translations("m1").unwrap()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].field, "overview");
}

#[tokio::test]
async fn write_back_fields_only_writes_selected_field_and_keeps_draft() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "盗梦空间 排序".into(),
                genres: vec!["剧情".into()],
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into()],
                people: vec![PersonView {
                    id: Some("p1".into()),
                    name: "莱昂纳多".into(),
                    role: Some("科布".into()),
                }],
            },
        )
        .unwrap();

    let report = service
        .write_back_fields("m1", vec![FieldKind::Overview])
        .await
        .unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let updates = fake.updates();
    assert_eq!(updates.len(), 1);
    let sent = &updates[0].1;
    assert_eq!(sent["Overview"], "简介");
    // Everything the user did not select keeps the server's original value.
    assert_eq!(sent["Name"], "Inception");
    assert_eq!(sent["OriginalTitle"], "Inception");
    assert_eq!(sent["Tags"], json!(["Sci-Fi", "Heist"]));
    assert_eq!(sent["People"][0]["Name"], "Leonardo DiCaprio");
    let locked = sent["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(locked.contains(&"Runtime"));
    assert!(locked.contains(&"Overview"));
    assert!(!locked.contains(&"Name"));
    assert!(!locked.contains(&"Tags"));
    assert!(!locked.contains(&"Cast"));

    // A successful write-back marks the item Written but keeps the draft text.
    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Written);
    assert_eq!(item.title, "盗梦空间");
    assert_eq!(item.overview, "简介");
    let record = {
        let inner = service.inner().unwrap();
        inner.store.get_item("m1").unwrap().unwrap()
    };
    assert!(record.locked_fields.iter().any(|field| field == "Overview"));
    assert!(!record.locked_fields.iter().any(|field| field == "Name"));

    // Editing/translating again naturally goes back to Draft.
    service.save_draft("m1", title_edit("盗梦空间 2")).unwrap();
    assert_eq!(
        service.items("movies").unwrap()[0].status,
        TranslationStatus::Draft
    );
}

#[tokio::test]
async fn original_title_lock_falls_back_when_the_server_rejects_it() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.reject_locked_field("OriginalTitle");
    let service = synced_service(&fake).await;

    let report = service
        .write_back_fields("m1", vec![FieldKind::OriginalTitle])
        .await
        .unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let updates = fake.updates();
    assert_eq!(updates.len(), 2, "one rejected attempt + one retry");
    let first = updates[0].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(first.contains(&"OriginalTitle"));
    let retry = updates[1].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(!retry.contains(&"OriginalTitle"));

    // The session remembers the limitation, so the next write does not retry.
    service
        .write_back_fields("m1", vec![FieldKind::Tags])
        .await
        .unwrap();
    let updates = fake.updates();
    assert_eq!(updates.len(), 3);
    let last = updates[2].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(!last.contains(&"OriginalTitle"));
    assert!(last.contains(&"Tags"));

    let record = {
        let inner = service.inner().unwrap();
        inner.store.get_item("m1").unwrap().unwrap()
    };
    assert!(!record
        .locked_fields
        .iter()
        .any(|field| field == "OriginalTitle"));
    assert_eq!(record.status, store::STATUS_WRITTEN);
}

#[tokio::test]
async fn empty_field_selection_is_a_noop() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service.install_translator(EchoTranslator::new()).unwrap();

    let report = service
        .translate_fields("m1", Vec::new(), templates(), settings())
        .await
        .unwrap();
    assert_eq!(report.translated, 0);
    assert!(service.items("movies").unwrap()[0].status == TranslationStatus::Untranslated);

    let report = service.write_back_fields("m1", Vec::new()).await.unwrap();
    assert_eq!(report.written, 0);
    assert!(fake.updates().is_empty());
}

#[tokio::test]
async fn only_selected_libraries_are_visible_on_the_home_tab() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies"), library("shows", "Shows")],
        vec![
            ("movies".to_string(), movie("m1", "Inception")),
            ("movies".to_string(), movie("m2", "Interstellar")),
            ("shows".to_string(), series("s1", "Breaking Bad")),
        ],
    );
    let service = AppService::open_in_memory().unwrap();
    service.install_api(fake).unwrap();

    // First selection syncs both libraries: both visible.
    service
        .sync_libraries(vec!["movies".to_string(), "shows".to_string()], |_| {})
        .await
        .unwrap();
    let visible = service.libraries().unwrap();
    assert_eq!(visible.len(), 2);

    // Re-sync only Movies: Shows disappears from the home tab...
    service
        .sync_libraries(vec!["movies".to_string()], |_| {})
        .await
        .unwrap();
    let visible = service.libraries().unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, "movies");
    assert_eq!(visible[0].item_count, 2);

    // ...but its rows and items are kept.
    assert_eq!(service.items("shows").unwrap().len(), 1);
    assert_eq!(service.items("shows").unwrap()[0].title, "Breaking Bad");

    // The selection dialog still sees every server library with local counts.
    let remote = service.remote_libraries().await.unwrap();
    assert_eq!(remote.len(), 2);
    let shows = remote.iter().find(|library| library.id == "shows").unwrap();
    assert_eq!(shows.item_count, 1);

    // Re-selecting both restores both on the home tab.
    service
        .sync_libraries(vec!["movies".to_string(), "shows".to_string()], |_| {})
        .await
        .unwrap();
    assert_eq!(service.libraries().unwrap().len(), 2);

    // An empty or unknown-only selection must not hide everything.
    service.sync_libraries(Vec::new(), |_| {}).await.unwrap();
    assert_eq!(service.libraries().unwrap().len(), 2);
    service
        .sync_libraries(vec!["does-not-exist".to_string()], |_| {})
        .await
        .unwrap();
    assert_eq!(service.libraries().unwrap().len(), 2);
}

#[tokio::test]
async fn translate_preview_returns_values_without_writing_the_store() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service
        .upsert_preset(
            PresetKind::Person,
            "  leonardo dicaprio ",
            "莱昂纳多·迪卡普里奥",
        )
        .unwrap();
    service.install_translator(EchoTranslator::new()).unwrap();

    let preview = service
        .translate_fields_preview(
            "m1",
            vec![FieldKind::Title, FieldKind::Tags, FieldKind::People],
            templates(),
            settings(),
        )
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("[zh-CN] Inception"));
    assert!(preview.original_title.is_none());
    assert!(preview.overview.is_none());
    assert_eq!(
        preview.tags.unwrap(),
        vec!["[zh-CN] Sci-Fi".to_string(), "[zh-CN] Heist".to_string()]
    );
    let people = preview.people.unwrap();
    assert_eq!(people[0].id.as_deref(), Some("p1"));
    assert_eq!(people[0].name, "莱昂纳多·迪卡普里奥");
    assert_eq!(people[0].role.as_deref(), Some("[zh-CN] Dom Cobb"));

    // Nothing was persisted: no draft, no status change, no audit rows and no
    // preset usage increment.
    let record = {
        let inner = service.inner().unwrap();
        assert!(inner.store.list_translations("m1").unwrap().is_empty());
        inner.store.get_item("m1").unwrap().unwrap()
    };
    assert_eq!(record.status, store::STATUS_UNTRANSLATED);
    assert!(record.draft_name.is_none());
    assert!(record.draft_tags.is_none());
    assert!(record.draft_people.is_none());

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Untranslated);
    assert_eq!(item.title, "Inception");
    assert_eq!(service.presets().unwrap()[0].uses, 0);
}

#[tokio::test]
async fn translate_preview_rejects_locked_items() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service.install_translator(EchoTranslator::new()).unwrap();
    service.set_locked(&["m1".into()], true).unwrap();

    let error = service
        .translate_fields_preview("m1", vec![FieldKind::Title], templates(), settings())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("已锁定"), "unexpected error: {error}");

    let record = {
        let inner = service.inner().unwrap();
        inner.store.get_item("m1").unwrap().unwrap()
    };
    assert_eq!(record.status, store::STATUS_UNTRANSLATED);
    assert!(record.draft_name.is_none());
}

async fn connected_service(label: &str, fake: &Arc<FakeApi>) -> (AppService, std::path::PathBuf) {
    let dir = temp_data_dir(label);
    let service = AppService::open_at(&dir).unwrap();
    let fake_for_factory = fake.clone();
    service
        .install_client_factory(Arc::new(move |_url, _key| {
            Ok(fake_for_factory.clone() as Arc<dyn JellyfinApi>)
        }))
        .unwrap();
    service
        .connect("http://media.home:8096", "secret")
        .await
        .unwrap();
    service.sync_all(|_| {}).await.unwrap();
    (service, dir)
}

fn title_edit(title: &str) -> ItemEdit {
    ItemEdit {
        sort_name: String::new(),
        genres: Vec::new(),
        title: title.to_string(),
        original_title: "Inception".into(),
        overview: "简介".into(),
        tags: vec!["科幻".into()],
        people: Vec::new(),
    }
}

#[tokio::test]
async fn item_reads_prefer_the_cached_user_route() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.set_users(&[("u1", "Alice"), ("u2", "Bob")]);
    let (service, dir) = connected_service("user-route", &fake).await;

    service.save_draft("m1", title_edit("盗梦空间")).unwrap();
    let report = service
        .write_back_fields("m1", vec![FieldKind::Title])
        .await
        .unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let routes = fake.item_routes();
    assert_eq!(routes.len(), 2, "one read + one read-back: {routes:?}");
    assert!(
        routes.iter().all(|route| route == "user:u1:m1"),
        "expected the first cached user, got {routes:?}"
    );

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn item_reads_fall_back_to_the_legacy_route_on_400_or_404() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.set_users(&[("u1", "Alice")]);
    fake.set_user_item_status(Some(404));
    let (service, dir) = connected_service("user-route-fallback", &fake).await;

    service.save_draft("m1", title_edit("盗梦空间")).unwrap();
    let report = service
        .write_back_fields("m1", vec![FieldKind::Title])
        .await
        .unwrap();
    assert_eq!(report.written, 1);

    let routes = fake.item_routes();
    assert_eq!(
        routes,
        vec![
            "user:u1:m1".to_string(),
            "legacy:m1".to_string(),
            "user:u1:m1".to_string(),
            "legacy:m1".to_string(),
        ],
        "write read and read-back verification both fall back"
    );

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn item_reads_use_the_legacy_route_without_users() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    // No users: connect caches no user id.
    let (service, dir) = connected_service("user-route-none", &fake).await;

    service.save_draft("m1", title_edit("盗梦空间")).unwrap();
    let report = service
        .write_back_fields("m1", vec![FieldKind::Title])
        .await
        .unwrap();
    assert_eq!(report.written, 1);

    let routes = fake.item_routes();
    assert!(
        routes.iter().all(|route| route == "legacy:m1"),
        "expected legacy-only routing, got {routes:?}"
    );

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn item_errors_keep_the_http_status_when_both_routes_fail() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.set_users(&[("u1", "Alice")]);
    fake.set_user_item_status(Some(400));
    fake.set_legacy_item_status(Some(500));
    let (service, dir) = connected_service("user-route-errors", &fake).await;

    let error = service.fetch_item(fake.as_ref(), "m1").await.unwrap_err();
    let api_error = error
        .downcast_ref::<jellyfin::ApiError>()
        .expect("HTTP status must survive as a typed error");
    assert_eq!(api_error.status, Some(500));

    let report = service
        .write_back_fields("m1", vec![FieldKind::Title])
        .await
        .unwrap();
    assert_eq!(report.written, 0);
    assert!(report.failed[0].1.contains("500"), "{:?}", report.failed);

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn translate_fields_supports_genres_and_sort_name() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service.install_translator(EchoTranslator::new()).unwrap();

    let report = service
        .translate_fields(
            "m1",
            vec![FieldKind::SortName, FieldKind::Genres],
            templates(),
            settings(),
        )
        .await
        .unwrap();
    assert_eq!(report.translated, 1);

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.sort_name, "[zh-CN] Inception");
    assert_eq!(item.genres, vec!["[zh-CN] Drama".to_string()]);
    // Other fields are untouched; source keeps the server originals.
    assert_eq!(item.title, "Inception");
    assert_eq!(item.tags, vec!["Sci-Fi", "Heist"]);
    assert_eq!(item.source.sort_name, "Inception");
    assert_eq!(item.source.genres, vec!["Drama".to_string()]);

    let rows = {
        let inner = service.inner().unwrap();
        inner.store.list_translations("m1").unwrap()
    };
    assert!(rows.iter().any(|row| row.field == "sort_name"));
    assert!(rows.iter().any(|row| row.field == "genres"));
}

#[tokio::test]
async fn translate_preview_supports_genres_and_sort_name_without_saving() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service.install_translator(EchoTranslator::new()).unwrap();

    let preview = service
        .translate_fields_preview(
            "m1",
            vec![FieldKind::SortName, FieldKind::Genres],
            templates(),
            settings(),
        )
        .await
        .unwrap();
    assert_eq!(preview.sort_name.as_deref(), Some("[zh-CN] Inception"));
    assert_eq!(preview.genres.unwrap(), vec!["[zh-CN] Drama".to_string()]);
    assert!(preview.title.is_none());
    assert!(preview.overview.is_none());

    let record = {
        let inner = service.inner().unwrap();
        inner.store.get_item("m1").unwrap().unwrap()
    };
    assert_eq!(record.status, store::STATUS_UNTRANSLATED);
    assert!(record.draft_sort_name.is_none());
    assert!(record.draft_genres.is_none());
}

#[tokio::test]
async fn write_back_fields_only_writes_genres() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "盗梦空间".into(),
                genres: vec!["剧情".into(), "科幻".into()],
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into()],
                people: Vec::new(),
            },
        )
        .unwrap();

    let report = service
        .write_back_fields("m1", vec![FieldKind::Genres])
        .await
        .unwrap();
    assert_eq!(report.written, 1);

    let updates = fake.updates();
    assert_eq!(updates.len(), 1);
    let sent = &updates[0].1;
    assert_eq!(sent["Genres"], json!(["剧情", "科幻"]));
    // SortName/Name were not selected: server values are kept.
    assert_eq!(sent["SortName"], "Inception");
    assert_eq!(sent["Name"], "Inception");
    let locked = sent["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(locked.contains(&"Genres"));
    assert!(!locked.contains(&"SortName"));
    assert!(!locked.contains(&"Name"));

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Written);
    assert_eq!(item.genres, vec!["剧情".to_string(), "科幻".to_string()]);
    assert_eq!(item.sort_name, "盗梦空间");
}

#[tokio::test]
async fn sort_name_lock_falls_back_when_the_server_rejects_it() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    fake.reject_locked_field("SortName");
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: "盗梦空间".into(),
                genres: Vec::new(),
                title: "Inception".into(),
                original_title: "Inception".into(),
                overview: "Overview of Inception".into(),
                tags: vec!["Sci-Fi".into(), "Heist".into()],
                people: Vec::new(),
            },
        )
        .unwrap();

    let report = service
        .write_back_fields("m1", vec![FieldKind::SortName])
        .await
        .unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let updates = fake.updates();
    assert_eq!(updates.len(), 2, "one rejected attempt + one retry");
    let first = updates[0].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(first.contains(&"SortName"));
    let retry = updates[1].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(!retry.contains(&"SortName"));

    // The session remembers it: the next write does not include/retry it.
    service
        .write_back_fields("m1", vec![FieldKind::Genres])
        .await
        .unwrap();
    let updates = fake.updates();
    assert_eq!(updates.len(), 3);
    let last = updates[2].1["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(!last.contains(&"SortName"));
    assert!(last.contains(&"Genres"));
}

#[tokio::test]
async fn write_back_skips_empty_unknown_source_fields() {
    // Simulate a row synced before Genres/SortName existed: the server has no
    // value and the local mirror is empty.
    let mut legacy = movie("m1", "Inception");
    legacy.genres = None;
    legacy.sort_name = None;
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), legacy)],
    );
    let service = synced_service(&fake).await;

    let item = service.items("movies").unwrap().remove(0);
    assert!(item.genres.is_empty());
    assert!(item.sort_name.is_empty());

    // Full write-back must not send empty Genres/SortName (which would wipe the
    // server) and must not lock those fields.
    let report = service.write_back(vec!["m1".into()]).await.unwrap();
    assert_eq!(report.written, 1);
    assert!(report.failed.is_empty());

    let sent = &fake.updates()[0].1;
    assert_eq!(sent["Genres"], Value::Null);
    assert_eq!(sent["SortName"], Value::Null);
    // Fields that do have source data are written as before.
    assert_eq!(sent["Name"], "Inception");
    assert_eq!(sent["Overview"], "Overview of Inception");
    assert_eq!(sent["Tags"], json!(["Sci-Fi", "Heist"]));

    let locked = sent["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(locked.contains(&"Name"));
    assert!(!locked.contains(&"Genres"));
    assert!(!locked.contains(&"SortName"));
}

#[tokio::test]
async fn write_back_with_only_empty_source_fields_is_a_noop() {
    let mut legacy = movie("m1", "Inception");
    legacy.genres = None;
    legacy.sort_name = None;
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), legacy)],
    );
    let service = synced_service(&fake).await;

    let report = service
        .write_back_fields("m1", vec![FieldKind::Genres, FieldKind::SortName])
        .await
        .unwrap();
    assert_eq!(report.written, 0);
    assert!(report.failed.is_empty());
    assert!(fake.updates().is_empty(), "no request should be sent");
    assert_eq!(
        service.items("movies").unwrap()[0].status,
        TranslationStatus::Untranslated
    );
}

#[tokio::test]
async fn empty_drafts_are_written_and_locked() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    service
        .save_draft(
            "m1",
            ItemEdit {
                sort_name: String::new(),
                genres: Vec::new(),
                title: "Inception".into(),
                original_title: "Inception".into(),
                overview: "Overview of Inception".into(),
                tags: vec!["Sci-Fi".into(), "Heist".into()],
                people: Vec::new(),
            },
        )
        .unwrap();

    let report = service
        .write_back_fields("m1", vec![FieldKind::Genres, FieldKind::SortName])
        .await
        .unwrap();
    assert_eq!(report.written, 1);

    let sent = &fake.updates()[0].1;
    assert_eq!(sent["Genres"], json!([]));
    assert_eq!(sent["SortName"], "");
    let locked = sent["LockedFields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(locked.contains(&"Genres"));
    assert!(locked.contains(&"SortName"));

    assert_eq!(
        service.items("movies").unwrap()[0].status,
        TranslationStatus::Written
    );
}

#[tokio::test]
async fn presets_are_scoped_by_kind_and_hits_skip_the_provider() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;

    service
        .upsert_preset(
            PresetKind::Person,
            "Leonardo DiCaprio",
            "莱昂纳多·迪卡普里奥",
        )
        .unwrap();
    service
        .upsert_preset(PresetKind::Tag, "Sci-Fi", "科幻")
        .unwrap();
    service
        .upsert_preset(PresetKind::Genre, "Drama", "剧情")
        .unwrap();
    // Wrong-kind entries must never be applied across categories.
    service
        .upsert_preset(PresetKind::Tag, "Drama", "错误标签")
        .unwrap();
    service
        .upsert_preset(PresetKind::Genre, "Sci-Fi", "错误类型")
        .unwrap();
    service
        .upsert_preset(PresetKind::Tag, "Tom Hanks", "标签汤姆")
        .unwrap();

    let translator = EchoTranslator::new();
    service.install_translator(translator.clone()).unwrap();

    let report = service
        .translate_fields(
            "m1",
            vec![FieldKind::Tags, FieldKind::Genres, FieldKind::People],
            templates(),
            settings(),
        )
        .await
        .unwrap();
    assert_eq!(report.translated, 1);
    assert!(report.failed.is_empty());

    let item = service.items("movies").unwrap().remove(0);
    // Tag preset hit + normal translation of the miss.
    assert_eq!(
        item.tags,
        vec!["科幻".to_string(), "[zh-CN] Heist".to_string()]
    );
    // Genre preset hit; the Tag preset for "Drama" must not leak in.
    assert_eq!(item.genres, vec!["剧情".to_string()]);
    // Person preset hit; the Tag preset for "Tom Hanks" must not apply.
    assert_eq!(item.people[0].name, "莱昂纳多·迪卡普里奥");
    assert_eq!(item.people[1].name, "[zh-CN] Tom Hanks");
    assert_eq!(item.people[1].role.as_deref(), Some("[zh-CN] Narrator"));

    // Preset hits were not sent to the provider: only misses appear in prompts,
    // and the all-hit Genres field made no provider call at all.
    let prompts = translator.prompts();
    assert_eq!(prompts.len(), 2, "tags miss + people miss: {prompts:?}");
    let joined = prompts.join("\n");
    assert!(!joined.contains("Sci-Fi"));
    assert!(!joined.contains("Drama"));
    assert!(!joined.contains("Leonardo DiCaprio"));
    assert!(joined.contains("Heist"));
    assert!(joined.contains("Tom Hanks"));

    // Usage counters are independent per kind and only bumped for real hits.
    let uses = |kind: PresetKind, source: &str| {
        service
            .presets()
            .unwrap()
            .into_iter()
            .find(|preset| preset.kind == kind && preset.source.eq_ignore_ascii_case(source))
            .map(|preset| preset.uses)
            .unwrap()
    };
    assert_eq!(uses(PresetKind::Person, "Leonardo DiCaprio"), 1);
    assert_eq!(uses(PresetKind::Tag, "Sci-Fi"), 1);
    assert_eq!(uses(PresetKind::Genre, "Drama"), 1);
    assert_eq!(uses(PresetKind::Tag, "Drama"), 0);
    assert_eq!(uses(PresetKind::Genre, "Sci-Fi"), 0);
    assert_eq!(uses(PresetKind::Tag, "Tom Hanks"), 0);
}
