//! Minimal, typed client for the Jellyfin 12.x REST API.
//!
//! Auth: `Authorization: MediaBrowser Client="...", Device="...", DeviceId="...", Version="...", Token="<api key>"`
//! (Jellyfin 12.0 removed `X-Emby-Token` / `api_key` query support.)

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

pub const CLIENT_NAME: &str = "jellyfin-translate";
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Fields requested from `/Items` so the caller gets everything the UI needs.
pub const ITEM_FIELDS: &str = "Overview,OriginalTitle,SortName,Tags,Genres,People,ImageTags,ProductionYear,PremiereDate,LockedFields,ProviderIds,SeriesId,ParentIndexNumber,IndexNumber";

/// Maximum number of response-body characters quoted in error messages.
const ERROR_SNIPPET_CHARS: usize = 400;

/// HTTP error returned by the client, retaining the status code so callers can
/// distinguish "user-scoped route not usable" (400/404) from real failures.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: Option<u16>,
    pub context: String,
    pub body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(
                formatter,
                "{} failed: HTTP {}: {}",
                self.context, status, self.body
            ),
            None => write!(formatter, "{} failed: {}", self.context, self.body),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Library {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub collection_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// Alias used by integration callers that only need id/name.
pub type UserSummary = UserDto;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersonDto {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, rename = "Type")]
    pub person_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemDto {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub original_title: Option<String>,
    #[serde(default)]
    pub sort_name: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub genres: Option<Vec<String>>,
    #[serde(default)]
    pub people: Option<Vec<PersonDto>>,
    #[serde(default)]
    pub image_tags: Option<HashMap<String, String>>,
    #[serde(default, rename = "Type")]
    pub item_type: Option<String>,
    #[serde(default)]
    pub production_year: Option<i32>,
    #[serde(default)]
    pub locked_fields: Option<Vec<String>>,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub parent_index_number: Option<i32>,
    #[serde(default)]
    pub index_number: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemsPage {
    #[serde(default)]
    pub items: Vec<ItemDto>,
    #[serde(default)]
    pub total_record_count: i64,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        let api_key = api_key.trim();

        anyhow::ensure!(!base_url.is_empty(), "Jellyfin URL must not be empty");
        let lower = base_url.to_ascii_lowercase();
        anyhow::ensure!(
            lower.starts_with("http://") || lower.starts_with("https://"),
            "Jellyfin URL must start with http:// or https:// (got {base_url:?})"
        );
        anyhow::ensure!(!api_key.is_empty(), "Jellyfin API key must not be empty");
        anyhow::ensure!(
            !api_key
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\')),
            "Jellyfin API key must not contain control characters, quotes or backslashes"
        );

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent(format!("{CLIENT_NAME}/{CLIENT_VERSION}"))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_header(&self) -> String {
        format!(
            "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"desktop\", DeviceId=\"{CLIENT_NAME}\", Version=\"{CLIENT_VERSION}\", Token=\"{}\"",
            self.api_key
        )
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(query)
            .send()
            .await
            .with_context(|| format!("GET {url} failed"))?;
        Ok(resp)
    }

    async fn ensure_ok(resp: reqwest::Response, ctx: &str) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.bytes().await.unwrap_or_default();
        Err(ApiError {
            status: Some(status.as_u16()),
            context: ctx.to_string(),
            body: body_snippet(body.as_ref()),
        }
        .into())
    }

    pub async fn system_info(&self) -> Result<SystemInfo> {
        let resp = self.get("/System/Info", &[]).await?;
        let resp = Self::ensure_ok(resp, "GET /System/Info").await?;
        parse_json(resp, "GET /System/Info").await
    }

    pub async fn libraries(&self) -> Result<Vec<Library>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Wrapper {
            #[serde(default)]
            items: Vec<Library>,
        }
        let resp = self.get("/Library/MediaFolders", &[]).await?;
        let resp = Self::ensure_ok(resp, "GET /Library/MediaFolders").await?;
        let wrapper: Wrapper = parse_json(resp, "GET /Library/MediaFolders").await?;
        Ok(wrapper.items)
    }

    /// `GET /Users` — used to pick a user context for item requests.
    pub async fn users(&self) -> Result<Vec<UserDto>> {
        let resp = self.get("/Users", &[]).await?;
        let resp = Self::ensure_ok(resp, "GET /Users").await?;
        parse_json(resp, "GET /Users").await
    }

    pub async fn items_page(
        &self,
        parent_id: Option<&str>,
        include_types: &[String],
        start: usize,
        limit: usize,
    ) -> Result<ItemsPage> {
        let mut query: Vec<(&str, String)> = vec![
            ("Recursive", "true".into()),
            ("Fields", ITEM_FIELDS.into()),
            ("SortBy", "SortName".into()),
            ("SortOrder", "Ascending".into()),
            ("StartIndex", start.to_string()),
            ("Limit", limit.to_string()),
        ];
        if let Some(parent) = parent_id {
            let parent = path_segment("parent id", parent)?;
            query.push(("ParentId", parent));
        }
        if !include_types.is_empty() {
            query.push(("IncludeItemTypes", include_types.join(",")));
        }
        let resp = self.get("/Items", &query).await?;
        let resp = Self::ensure_ok(resp, "GET /Items").await?;
        parse_json(resp, "GET /Items").await
    }

    /// Full DTO used as the base for safe read-modify-write updates.
    pub async fn item(&self, item_id: &str) -> Result<Value> {
        let item_id = path_segment("item id", item_id)?;
        self.item_at(&format!("/Items/{item_id}")).await
    }

    /// Same as [`Self::item`] but scoped to a user. Jellyfin 12.x sometimes
    /// rejects the user-less route when the API key has no user context.
    pub async fn item_for_user(&self, user_id: &str, item_id: &str) -> Result<Value> {
        let user_id = path_segment("user id", user_id)?;
        let item_id = path_segment("item id", item_id)?;
        self.item_at(&format!("/Users/{user_id}/Items/{item_id}"))
            .await
    }

    async fn item_at(&self, path: &str) -> Result<Value> {
        let resp = self.get(path, &[]).await?;
        let resp = Self::ensure_ok(resp, &format!("GET {path}")).await?;
        parse_json(resp, &format!("GET {path}")).await
    }

    /// `POST /Items/{itemId}` — body must be the complete DTO (the server overwrites scalars).
    pub async fn update_item(&self, item_id: &str, dto: &Value) -> Result<()> {
        let item_id = path_segment("item id", item_id)?;
        let path = format!("/Items/{item_id}");
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(dto)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        Self::ensure_ok(resp, &format!("POST {path}")).await?;
        Ok(())
    }

    /// Raw bytes of an item image (already decoded by the server).
    pub async fn image(&self, item_id: &str, image_type: &str, max_width: u32) -> Result<Vec<u8>> {
        let item_id = path_segment("item id", item_id)?;
        let image_type = path_segment("image type", image_type)?;
        let path = format!("/Items/{item_id}/Images/{image_type}");
        let resp = self
            .get(&path, &[("maxWidth", max_width.to_string())])
            .await?;
        let resp = Self::ensure_ok(resp, &format!("GET {path}")).await?;
        Ok(resp.bytes().await.context("read image body")?.to_vec())
    }

    /// Upload an image: the body is **base64 text**, `content_type` must be `image/*`.
    pub async fn upload_image_base64(
        &self,
        item_id: &str,
        image_type: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let item_id = path_segment("item id", item_id)?;
        let image_type = path_segment("image type", image_type)?;
        anyhow::ensure!(
            content_type.starts_with("image/"),
            "image content type must start with \"image/\" (got {content_type:?})"
        );
        let path = format!("/Items/{item_id}/Images/{image_type}");
        let url = format!("{}{}", self.base_url, path);
        let body = base64_encode(bytes);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        Self::ensure_ok(resp, &format!("POST {path}")).await?;
        Ok(())
    }

    /// Ask the server to download and store an image from an arbitrary http(s) URL.
    pub async fn download_remote_image(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<()> {
        let item_id = path_segment("item id", item_id)?;
        let image_type = path_segment("image type", image_type)?;
        anyhow::ensure!(
            !image_url.trim().is_empty(),
            "remote image URL must not be empty"
        );
        let path = format!("/Items/{item_id}/RemoteImages/Download");
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .query(&[("type", image_type.as_str()), ("imageUrl", image_url)])
            .send()
            .await
            .with_context(|| format!("POST {url} failed"))?;
        Self::ensure_ok(resp, &format!("POST {path}")).await?;
        Ok(())
    }
}

/// Reject values that would alter the request path (e.g. `../System/Info`).
fn path_segment(what: &str, value: &str) -> Result<String> {
    anyhow::ensure!(!value.is_empty(), "{what} must not be empty");
    anyhow::ensure!(
        !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '?' | '#')),
        "{what} {value:?} contains characters that are not allowed in a URL path segment"
    );
    Ok(value.to_string())
}

fn body_snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let snippet: String = text.chars().take(ERROR_SNIPPET_CHARS).collect();
    let snippet = snippet.trim();
    if snippet.is_empty() {
        "<empty response body>".to_string()
    } else {
        snippet.to_string()
    }
}

async fn parse_json<T: DeserializeOwned>(resp: reqwest::Response, ctx: &str) -> Result<T> {
    let body = resp
        .bytes()
        .await
        .with_context(|| format!("{ctx}: read response body"))?;
    serde_json::from_slice(&body).map_err(|err| {
        anyhow::anyhow!(
            "{ctx}: parse JSON failed ({err}): {}",
            body_snippet(body.as_ref())
        )
    })
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        out.push(TABLE[(b[0] >> 2) as usize] as char);
        out.push(TABLE[(((b[0] & 0b11) << 4) | (b[1] >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0b1111) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b[2] & 0b111111) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, body_snippet, path_segment};

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn path_segment_rejects_traversal_and_separators() {
        assert_eq!(path_segment("item id", "abc-123").unwrap(), "abc-123");
        for bad in [
            "",
            "../System/Info",
            "a/b",
            r"a\b",
            "a b",
            "a?b",
            "a#b",
            "a\nb",
        ] {
            assert!(
                path_segment("item id", bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn body_snippet_is_lossy_and_bounded() {
        assert_eq!(body_snippet(b""), "<empty response body>");
        assert_eq!(body_snippet(b"  boom  "), "boom");
        assert_eq!(body_snippet(&[0xff, b'o', b'k']), "\u{fffd}ok");
        assert_eq!(body_snippet(&vec![b'x'; 1_000]).chars().count(), 400);
    }
}
