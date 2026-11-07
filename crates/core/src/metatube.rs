//! MetaTube Server client used by the "抓取元数据" action in the details
//! drawer.
//!
//! Covered endpoints:
//!
//! - `GET /v1/movies/search` — keyword search / candidate list
//! - `GET /v1/movies/{provider}/{id}` — full movie metadata
//!
//! `/v1/images/*` is intentionally not used: the scrape only fills the local
//! draft, posters are never written back.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{normalize_preset_key, ItemEdit, ItemView, PersonView};

/// Default MetaTube endpoint; overridable on the settings page.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:18081";

/// Search fan-out requests can take up to the server's 1 minute timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Where the MetaTube server lives. Stored as JSON under the `metatube`
/// settings key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetatubeSettings {
    pub base_url: String,
    /// Empty when the server was started without `-token`.
    pub token: String,
}

impl Default for MetatubeSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            token: String::new(),
        }
    }
}

/// One element of `GET /v1/movies/search` (`data` array).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MovieSearchResult {
    pub id: String,
    pub number: String,
    pub title: String,
    pub provider: String,
    pub homepage: String,
    pub thumb_url: String,
    pub cover_url: String,
    pub score: f64,
    pub actors: Vec<String>,
    pub release_date: String,
}

/// The subset of `MovieInfo` the scrape actually uses.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MovieInfo {
    pub id: String,
    pub number: String,
    pub title: String,
    pub summary: String,
    pub provider: String,
    pub director: String,
    pub actors: Vec<String>,
    pub maker: String,
    pub label: String,
    pub series: String,
    pub genres: Vec<String>,
    pub score: f64,
    pub runtime: f64,
    pub release_date: String,
}

/// `{ "data": <T> }` / `{ "error": { "code": N, "message": "..." } }`.
#[derive(Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    data: Option<T>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// Thin HTTP client. Cheap to construct; one per job.
pub struct MetatubeClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl MetatubeClient {
    pub fn new(settings: &MetatubeSettings) -> Result<Self> {
        let base_url = settings.base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(anyhow!("MetaTube 服务地址未配置，请到设置页填写"));
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("构建 MetaTube HTTP 客户端失败")?;
        Ok(Self {
            base_url,
            token: settings.token.trim().to_string(),
            http,
        })
    }

    /// `provider = None` searches every enabled provider (slow: dozens of
    /// sites in parallel). A provider name restricts the fan-out.
    pub async fn search(
        &self,
        q: &str,
        provider: Option<&str>,
        fallback: bool,
    ) -> Result<Vec<MovieSearchResult>> {
        let keyword = q.trim();
        if keyword.is_empty() {
            return Err(anyhow!("请输入搜索关键词"));
        }

        let mut request = self
            .http
            .get(format!("{}/v1/movies/search", self.base_url))
            .query(&[
                ("q", keyword),
                ("fallback", if fallback { "true" } else { "false" }),
            ]);
        if let Some(provider) = provider.map(str::trim).filter(|p| !p.is_empty()) {
            request = request.query(&[("provider", provider)]);
        }

        let response = self.send(request).await?;
        let body: Envelope<Vec<MovieSearchResult>> = response
            .json()
            .await
            .context("解析 MetaTube 搜索结果失败")?;
        body.data
            .ok_or_else(|| anyhow!("MetaTube 响应缺少 data 字段"))
    }

    /// `lazy = true` lets the server answer from its own database first;
    /// `false` forces a fresh scrape.
    pub async fn detail(&self, provider: &str, id: &str, lazy: bool) -> Result<MovieInfo> {
        if provider.trim().is_empty() || id.trim().is_empty() {
            return Err(anyhow!("provider 或影片 ID 为空"));
        }
        let url = format!(
            "{}/v1/movies/{}/{}",
            self.base_url,
            encode_segment(provider.trim()),
            encode_segment(id.trim())
        );
        let request = self
            .http
            .get(url)
            .query(&[("lazy", if lazy { "true" } else { "false" })]);

        let response = self.send(request).await?;
        let body: Envelope<MovieInfo> = response
            .json()
            .await
            .context("解析 MetaTube 影片详情失败")?;
        body.data
            .ok_or_else(|| anyhow!("MetaTube 响应缺少 data 字段"))
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let request = if self.token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.token)
        };
        let response = request
            .send()
            .await
            .map_err(|error| anyhow!("无法连接 MetaTube 服务（{}）：{error}", self.base_url))?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Envelope<()>>(&body)
            .ok()
            .and_then(|envelope| envelope.error)
            .map(|error| error.message)
            .unwrap_or_else(|| body.trim().to_string());
        bail!(friendly_error(status.as_u16(), &message))
    }
}

/// Turn the server's machine-readable error into something a Chinese UI can
/// show directly. Unknown errors keep the upstream message as a suffix.
fn friendly_error(status: u16, message: &str) -> String {
    let message = message.trim();
    match (status, message) {
        (400, "invalid keyword") => "关键词为空或无效".to_string(),
        (400, "invalid id") => "影片 ID 无效".to_string(),
        (400, "invalid url") => "链接无法解析出影片 ID".to_string(),
        (401, _) => "MetaTube 服务需要 Token，请到设置页填写".to_string(),
        (404, "provider not found") => {
            "没有这个 provider，请检查拼写，或留空搜索全部来源".to_string()
        }
        // The server answers "info not found" for the all-provider fan-out and
        // plain "Not Found" when a specific provider has no hit.
        (404, "info not found" | "Not Found" | "not found" | "") => "未找到匹配的影片".to_string(),
        (500, "incomplete metadata") => "抓取到的元数据不完整".to_string(),
        _ if message.is_empty() => format!("MetaTube 返回错误（HTTP {status}）"),
        _ => format!("MetaTube 返回错误（HTTP {status}）：{message}"),
    }
}

/// Percent-encode one path segment (`provider` / `id`).
fn encode_segment(raw: &str) -> String {
    const UNRESERVED: &[u8] = b"-_.~";
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Fold a scraped [`MovieInfo`] into the item's next draft.
///
/// Scraped fields overwrite their current value; fields the scrape did not
/// return keep the current draft/server value. Actors have no role in the
/// MetaTube payload, so an existing person with the same normalized name
/// keeps its role and Jellyfin person id.
pub fn merge_draft(item: &ItemView, info: &MovieInfo) -> ItemEdit {
    let title = info.title.trim();
    let (title, original_title) = if title.is_empty() {
        (item.title.clone(), item.original_title.clone())
    } else {
        (title.to_string(), title.to_string())
    };

    let summary = info.summary.trim();
    let overview = if summary.is_empty() {
        item.overview.clone()
    } else {
        summary.to_string()
    };

    let genres = if info.genres.is_empty() {
        item.genres.clone()
    } else {
        info.genres.clone()
    };

    let tags = compose_tags(info);
    let tags = if tags.is_empty() {
        item.tags.clone()
    } else {
        tags
    };

    let actors: Vec<PersonView> =
        info.actors
            .iter()
            .filter_map(|actor| {
                let name = actor.trim();
                if name.is_empty() {
                    return None;
                }
                let existing = item.people.iter().find(|person| {
                    normalize_preset_key(&person.name) == normalize_preset_key(name)
                });
                Some(PersonView {
                    id: existing.and_then(|person| person.id.clone()),
                    name: name.to_string(),
                    role: existing.and_then(|person| person.role.clone()),
                })
            })
            .collect();
    let people = if actors.is_empty() {
        item.people.clone()
    } else {
        actors
    };

    ItemEdit {
        title,
        sort_name: item.sort_name.clone(),
        original_title,
        genres,
        overview,
        tags,
        people,
    }
}

/// `maker` / `label` / `series` as tags, de-duplicated and order preserving.
fn compose_tags(info: &MovieInfo) -> Vec<String> {
    let mut tags = Vec::new();
    for value in [&info.maker, &info.label, &info.series] {
        let value = value.trim();
        if !value.is_empty() && !tags.iter().any(|tag| tag == value) {
            tags.push(value.to_string());
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldKind, TranslationStatus};

    fn item() -> ItemView {
        ItemView {
            id: "m1".to_string(),
            library_id: "lib".to_string(),
            item_type: "Movie".to_string(),
            title: "旧标题".to_string(),
            sort_name: "旧排序".to_string(),
            original_title: "旧原名".to_string(),
            genres: vec!["旧类型".to_string()],
            overview: "旧简介".to_string(),
            tags: vec!["旧标签".to_string()],
            people: vec![PersonView {
                id: Some("p1".to_string()),
                name: "妃月るい".to_string(),
                role: Some("演员".to_string()),
            }],
            status: TranslationStatus::Untranslated,
            locked: false,
            source: crate::ItemSource {
                title: "旧标题".to_string(),
                sort_name: "旧排序".to_string(),
                original_title: "旧原名".to_string(),
                genres: vec![],
                overview: String::new(),
                tags: vec![],
                people: vec![],
            },
        }
    }

    fn info() -> MovieInfo {
        MovieInfo {
            id: "ipx00001".to_string(),
            number: "IPX-001".to_string(),
            title: "新しいタイトル".to_string(),
            summary: "あらすじ".to_string(),
            provider: "FANZA".to_string(),
            actors: vec!["妃月るい".to_string(), "  ".to_string()],
            maker: "厂商A".to_string(),
            label: "厂商A".to_string(),
            series: "系列B".to_string(),
            genres: vec!["类型A".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn merge_overwrites_scraped_fields_and_keeps_the_rest() {
        let draft = merge_draft(&item(), &info());
        assert_eq!(draft.title, "新しいタイトル");
        assert_eq!(draft.original_title, "新しいタイトル");
        assert_eq!(draft.overview, "あらすじ");
        assert_eq!(draft.genres, vec!["类型A".to_string()]);
        // maker == label → de-duplicated; series appended.
        assert_eq!(draft.tags, vec!["厂商A".to_string(), "系列B".to_string()]);
        // SortName has no scrape source and must survive untouched.
        assert_eq!(draft.sort_name, "旧排序");
        // Matching actor keeps its id and role.
        assert_eq!(draft.people.len(), 1);
        assert_eq!(draft.people[0].name, "妃月るい");
        assert_eq!(draft.people[0].id.as_deref(), Some("p1"));
        assert_eq!(draft.people[0].role.as_deref(), Some("演员"));
    }

    #[test]
    fn merge_keeps_current_values_when_scrape_is_empty() {
        let draft = merge_draft(&item(), &MovieInfo::default());
        assert_eq!(draft.title, "旧标题");
        assert_eq!(draft.original_title, "旧原名");
        assert_eq!(draft.overview, "旧简介");
        assert_eq!(draft.genres, vec!["旧类型".to_string()]);
        assert_eq!(draft.tags, vec!["旧标签".to_string()]);
        assert_eq!(draft.people.len(), 1);
    }

    #[test]
    fn search_result_parses_the_documented_envelope() {
        let raw = r#"{"data":[{"id":"ipx00001","number":"IPX-001","title":"t","provider":"FANZA","homepage":"https://x","thumb_url":"https://x/1.jpg","cover_url":"https://x/2.jpg","score":3.55,"actors":["a"],"release_date":"2017-08-11T00:00:00Z"}]}"#;
        let envelope: Envelope<Vec<MovieSearchResult>> = serde_json::from_str(raw).unwrap();
        let results = envelope.data.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].provider, "FANZA");
        assert_eq!(results[0].number, "IPX-001");
        assert_eq!(results[0].release_date, "2017-08-11T00:00:00Z");
        assert_eq!(results[0].actors, vec!["a".to_string()]);
    }

    #[test]
    fn movie_info_parses_the_documented_payload() {
        let raw = r#"{"data":{"id":"ipx00001","number":"IPX-001","title":"t","summary":"s","provider":"FANZA","director":"d","actors":["a"],"maker":"m","label":"l","series":"se","genres":["g"],"score":4.52,"runtime":120,"release_date":"2022-11-25T00:00:00Z"}}"#;
        let envelope: Envelope<MovieInfo> = serde_json::from_str(raw).unwrap();
        let info = envelope.data.unwrap();
        assert_eq!(info.maker, "m");
        assert_eq!(info.genres, vec!["g".to_string()]);
        assert_eq!(info.runtime, 120.0);
    }

    #[test]
    fn errors_are_translated_to_chinese_hints() {
        assert_eq!(friendly_error(404, "info not found"), "未找到匹配的影片");
        assert_eq!(friendly_error(404, "Not Found"), "未找到匹配的影片");
        assert!(friendly_error(404, "provider not found").contains("provider"));
        assert!(friendly_error(401, "").contains("Token"));
        assert!(friendly_error(500, "boom").contains("boom"));
    }

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(encode_segment("FANZA"), "FANZA");
        assert_eq!(encode_segment("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn empty_base_url_is_rejected() {
        let settings = MetatubeSettings {
            base_url: "  ".to_string(),
            token: String::new(),
        };
        assert!(MetatubeClient::new(&settings).is_err());
    }

    #[test]
    fn default_settings_point_at_the_local_bridge() {
        let settings = MetatubeSettings::default();
        assert_eq!(settings.base_url, DEFAULT_BASE_URL);
        assert_eq!(settings.token, "");
    }

    // `FieldKind` is imported only to keep the crate-root import list honest
    // if the mapping grows fields; this also documents that SortName is not
    // scraped.
    #[test]
    fn sort_name_is_not_a_scraped_field() {
        let draft = merge_draft(&item(), &info());
        assert_eq!(draft.sort_name, item().sort_name);
        let _ = FieldKind::SortName;
    }
}
