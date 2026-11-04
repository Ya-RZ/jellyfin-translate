//! Unit tests for the service. Everything runs against in-memory SQLite and a
//! fake Jellyfin API / translation provider — no network, no real server.

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
        })
    }

    fn ignore_update_field(&self, field: &str) {
        self.ignored_update_fields
            .lock()
            .unwrap()
            .push(field.to_string());
    }

    fn updates(&self) -> Vec<(String, Value)> {
        self.updates.lock().unwrap().clone()
    }
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
        "Genres": ["Drama"],
        "LockedFields": ["Runtime"],
        "Type": item.item_type,
    })
}

impl JellyfinApi for FakeApi {
    fn libraries<'a>(&'a self) -> BoxFuture<'a, Result<Vec<jellyfin::Library>>> {
        Box::pin(async move { Ok(self.libraries.clone()) })
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
            drop(stored);
            self.updates
                .lock()
                .unwrap()
                .push((item_id.to_string(), dto.clone()));
            Ok(())
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "jellyfin-translate-core-test-{}-{nanos}",
        std::process::id()
    ));

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
}

#[tokio::test]
async fn translate_stores_drafts_uses_presets_and_skips_locked() {
    let fake = FakeApi::new(
        vec![library("movies", "Movies")],
        vec![("movies".to_string(), movie("m1", "Inception"))],
    );
    let service = synced_service(&fake).await;
    service
        .upsert_preset("  leonardo dicaprio ", "莱昂纳多·迪卡普里奥")
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
    assert_eq!(item.people[0].name, "莱昂纳多·迪卡普里奥");
    assert_eq!(item.people[0].role.as_deref(), Some("[zh-CN] Dom Cobb"));
    assert_eq!(item.people[1].name, "[zh-CN] Tom Hanks");
    assert_eq!(item.people[1].role.as_deref(), Some("[zh-CN] Narrator"));

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
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into()],
                people: vec![PersonView {
                    name: "莱昂纳多".into(),
                    role: Some("科布".into()),
                }],
            },
        )
        .unwrap();

    let item = service.items("movies").unwrap().remove(0);
    assert_eq!(item.status, TranslationStatus::Draft);
    assert_eq!(item.title, "盗梦空间");
    assert_eq!(item.tags, vec!["科幻".to_string()]);

    service.set_locked(&["m1".into()], true).unwrap();
    let error = service
        .save_draft(
            "m1",
            ItemEdit {
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
                title: "盗梦空间".into(),
                original_title: "Inception".into(),
                overview: "简介".into(),
                tags: vec!["科幻".into(), "犯罪".into()],
                people: vec![
                    PersonView {
                        name: "莱昂纳多".into(),
                        role: Some("科布".into()),
                    },
                    PersonView {
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
async fn presets_crud_roundtrip() {
    let service = AppService::open_in_memory().unwrap();
    service.upsert_preset(" Tom Hanks ", "汤姆·汉克斯").unwrap();
    service.upsert_preset("tom hanks", "汤姆汉克斯").unwrap();
    let presets = service.presets().unwrap();
    assert_eq!(presets.len(), 1);
    assert_eq!(presets[0].target, "汤姆汉克斯");

    service.delete_preset(" TOM HANKS").unwrap();
    assert!(service.presets().unwrap().is_empty());
}
