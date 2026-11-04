//! Wiremock integration tests for the Jellyfin HTTP client.
//!
//! These tests pin down the wire format Jellyfin 12.x expects:
//! * `Authorization: MediaBrowser ... Token="<key>"` (no `X-Emby-Token`, no `api_key` query),
//! * request paths and query parameters,
//! * JSON bodies sent verbatim on `POST /Items/{id}`,
//! * base64 text bodies for image uploads,
//! * raw bytes for image downloads,
//! * error messages that quote the HTTP status and a response snippet.

use std::collections::HashMap;

use jellyfin::Client;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "test-api-key";

fn client(server: &MockServer) -> Client {
    Client::new(&server.uri(), TOKEN).expect("client should be constructed")
}

async fn received(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("mock server should still be running")
}

fn assert_media_browser_auth(req: &Request) {
    let auth = req
        .headers
        .get("authorization")
        .expect("Authorization header must be sent")
        .to_str()
        .expect("Authorization header must be valid ASCII");
    assert!(
        auth.starts_with("MediaBrowser "),
        "expected the MediaBrowser scheme, got {auth:?}"
    );
    assert!(
        auth.contains(&format!("Token=\"{TOKEN}\"")),
        "expected the API key as Token=\"...\", got {auth:?}"
    );
    assert!(
        req.headers.get("x-emby-token").is_none(),
        "X-Emby-Token must not be sent"
    );
    let query = req.url.query().unwrap_or_default();
    assert!(
        !query.contains("api_key") && !query.contains(TOKEN),
        "the API key must not leak into the query string: {query:?}"
    );
}

fn query_pairs(req: &Request) -> HashMap<String, String> {
    req.url.query_pairs().into_owned().collect()
}

#[tokio::test]
async fn system_info_uses_mediabrowser_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Id": "server-1",
            "ServerName": "Home",
            "Version": "10.9.11",
        })))
        .mount(&server)
        .await;

    let info = client(&server).system_info().await.expect("system info");
    assert_eq!(info.id, "server-1");
    assert_eq!(info.server_name, "Home");
    assert_eq!(info.version, "10.9.11");

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);
}

#[tokio::test]
async fn libraries_lists_media_folders() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Library/MediaFolders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [
                { "Id": "lib-movies", "Name": "Movies", "CollectionType": "movies" },
                { "Id": "lib-shows", "Name": "Shows", "CollectionType": "tvshows" },
            ],
            "TotalRecordCount": 2,
        })))
        .mount(&server)
        .await;

    let libraries = client(&server).libraries().await.expect("libraries");
    assert_eq!(libraries.len(), 2);
    assert_eq!(libraries[0].id, "lib-movies");
    assert_eq!(libraries[0].name, "Movies");
    assert_eq!(libraries[0].collection_type.as_deref(), Some("movies"));
    assert_eq!(libraries[1].name, "Shows");

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);
}

#[tokio::test]
async fn items_page_sends_pagination_and_filter_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("Recursive", "true"))
        .and(query_param("StartIndex", "0"))
        .and(query_param("Limit", "2"))
        .and(query_param("ParentId", "lib-movies"))
        .and(query_param("IncludeItemTypes", "Movie,Series"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [
                { "Id": "item-1", "Name": "First", "Type": "Movie" },
                { "Id": "item-2", "Name": "Second", "Type": "Series" },
            ],
            "TotalRecordCount": 3,
        })))
        .mount(&server)
        .await;

    let types = vec!["Movie".to_string(), "Series".to_string()];
    let page = client(&server)
        .items_page(Some("lib-movies"), &types, 0, 2)
        .await
        .expect("items page");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].id, "item-1");
    assert_eq!(page.items[1].item_type.as_deref(), Some("Series"));
    assert_eq!(page.total_record_count, 3);

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);
    let query = query_pairs(&requests[0]);
    assert_eq!(
        query.get("Fields").map(String::as_str),
        Some(jellyfin::ITEM_FIELDS)
    );
    assert_eq!(query.get("SortBy").map(String::as_str), Some("SortName"));
    assert_eq!(
        query.get("SortOrder").map(String::as_str),
        Some("Ascending")
    );
}

#[tokio::test]
async fn items_page_respects_total_record_count_boundary() {
    let server = MockServer::start().await;
    // Last partial page: only one item remains out of three.
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("StartIndex", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [{ "Id": "item-3", "Name": "Third" }],
            "TotalRecordCount": 3,
        })))
        .mount(&server)
        .await;
    // One page past the end: empty page, but the total is still reported.
    Mock::given(method("GET"))
        .and(path("/Items"))
        .and(query_param("StartIndex", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Items": [],
            "TotalRecordCount": 3,
        })))
        .mount(&server)
        .await;

    let client = client(&server);
    let last = client.items_page(None, &[], 2, 2).await.expect("last page");
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.items[0].id, "item-3");
    assert_eq!(last.total_record_count, 3);

    let past_end = client
        .items_page(None, &[], 3, 2)
        .await
        .expect("page past the end");
    assert!(past_end.items.is_empty());
    assert_eq!(past_end.total_record_count, 3);

    let requests = received(&server).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(
        query_pairs(&requests[0])
            .get("StartIndex")
            .map(String::as_str),
        Some("2")
    );
    assert_eq!(
        query_pairs(&requests[1])
            .get("StartIndex")
            .map(String::as_str),
        Some("3")
    );
}

#[tokio::test]
async fn item_fetches_full_dto() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/Items/item-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Id": "item-1",
            "Name": "Original name",
            "OriginalTitle": "原标题",
            "Overview": "简介",
            "Tags": ["tag-a", "tag-b"],
            "Genres": ["Drama"],
            "People": [
                { "Id": "person-1", "Name": "张三", "Role": "主角", "Type": "Actor" }
            ],
            "ImageTags": { "Primary": "primary-tag" },
            "Type": "Movie",
            "ProductionYear": 2020,
            "LockedFields": ["Name"],
            "SeriesId": "series-1",
            "ParentIndexNumber": 1,
            "IndexNumber": 2,
        })))
        .mount(&server)
        .await;

    let dto = client(&server).item("item-1").await.expect("item dto");
    assert_eq!(dto["Id"], "item-1");
    assert_eq!(dto["OriginalTitle"], "原标题");
    assert_eq!(dto["People"][0]["Name"], "张三");
    assert_eq!(dto["ImageTags"]["Primary"], "primary-tag");
    assert_eq!(dto["ProductionYear"], 2020);

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);
}

#[tokio::test]
async fn update_item_posts_the_dto_body_verbatim() {
    let server = MockServer::start().await;
    let dto = json!({
        "Id": "item-1",
        "Name": "新名字",
        "OriginalTitle": "New title",
        "Overview": "line one\nline two",
        "Tags": ["tag-a"],
        "Genres": [],
        "LockedFields": ["Name", "Overview"],
        "People": [{ "Name": "李四", "Role": "配角", "Type": "Actor" }],
        "ProviderIds": { "Tmdb": "42" },
    });
    Mock::given(method("POST"))
        .and(path("/Items/item-1"))
        .and(body_json(dto.clone()))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client(&server)
        .update_item("item-1", &dto)
        .await
        .expect("update item");

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);

    let content_type = requests[0]
        .headers
        .get("content-type")
        .expect("Content-Type must be set")
        .to_str()
        .expect("valid header");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content type {content_type:?}"
    );

    let sent: Value = serde_json::from_slice(&requests[0].body).expect("body must be JSON");
    assert_eq!(sent, dto, "the DTO must be sent unmodified");
}

#[tokio::test]
async fn image_download_returns_raw_bytes() {
    let server = MockServer::start().await;
    let bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
    Mock::given(method("GET"))
        .and(path("/Items/item-1/Images/Primary"))
        .and(query_param("maxWidth", "320"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .mount(&server)
        .await;

    let image = client(&server)
        .image("item-1", "Primary", 320)
        .await
        .expect("download image");
    assert_eq!(image, bytes);

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);
    assert_eq!(
        query_pairs(&requests[0])
            .get("maxWidth")
            .map(String::as_str),
        Some("320")
    );
}

#[tokio::test]
async fn upload_image_sends_base64_text_with_image_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/Items/item-1/Images/Backdrop"))
        .and(header("content-type", "image/jpeg"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client(&server)
        .upload_image_base64("item-1", "Backdrop", "image/jpeg", b"hello world")
        .await
        .expect("upload image");

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);

    let body = String::from_utf8(requests[0].body.clone()).expect("base64 body must be UTF-8 text");
    assert_eq!(body, "aGVsbG8gd29ybGQ=");
    assert_eq!(
        requests[0]
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/jpeg")
    );
}

#[tokio::test]
async fn download_remote_image_sends_type_and_url_as_query_parameters() {
    let server = MockServer::start().await;
    let image_url = "https://cdn.example.com/covers/a b.jpg?size=big&lang=zh";
    Mock::given(method("POST"))
        .and(path("/Items/item-1/RemoteImages/Download"))
        .and(query_param("type", "Backdrop"))
        .and(query_param("imageUrl", image_url))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    client(&server)
        .download_remote_image("item-1", "Backdrop", image_url)
        .await
        .expect("download remote image");

    let requests = received(&server).await;
    assert_eq!(requests.len(), 1);
    assert_media_browser_auth(&requests[0]);

    let query = query_pairs(&requests[0]);
    assert_eq!(query.get("type").map(String::as_str), Some("Backdrop"));
    assert_eq!(query.get("imageUrl").map(String::as_str), Some(image_url));

    let raw_query = requests[0].url.query().expect("query string");
    assert!(
        !raw_query.contains(' '),
        "the image URL must be percent-encoded: {raw_query:?}"
    );
    assert!(
        raw_query.contains("imageUrl="),
        "unexpected query string: {raw_query:?}"
    );
}

#[tokio::test]
async fn http_errors_include_status_and_response_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .respond_with(ResponseTemplate::new(404).set_body_string("no such endpoint"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/Items/item-1"))
        .respond_with(ResponseTemplate::new(500).set_body_string("backend exploded"))
        .mount(&server)
        .await;

    let client = client(&server);

    let not_found = client
        .system_info()
        .await
        .expect_err("404 must be an error")
        .to_string();
    assert!(not_found.contains("GET /System/Info"), "{not_found}");
    assert!(not_found.contains("404"), "{not_found}");
    assert!(not_found.contains("no such endpoint"), "{not_found}");

    let server_error = client
        .update_item("item-1", &json!({ "Id": "item-1" }))
        .await
        .expect_err("500 must be an error")
        .to_string();
    assert!(
        server_error.contains("POST /Items/item-1"),
        "{server_error}"
    );
    assert!(server_error.contains("500"), "{server_error}");
    assert!(server_error.contains("backend exploded"), "{server_error}");
}

#[tokio::test]
async fn invalid_json_success_body_reports_a_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
        .mount(&server)
        .await;

    let message = client(&server)
        .system_info()
        .await
        .expect_err("malformed JSON must be an error")
        .to_string();
    assert!(message.contains("parse JSON failed"), "{message}");
    assert!(message.contains("{not valid json"), "{message}");
}

#[tokio::test]
async fn non_utf8_error_body_is_reported_lossily() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .respond_with(
            ResponseTemplate::new(503).set_body_bytes(vec![0xff, 0xfe, b'b', b'o', b'o', b'm']),
        )
        .mount(&server)
        .await;

    let message = client(&server)
        .system_info()
        .await
        .expect_err("503 must be an error")
        .to_string();
    assert!(message.contains("503"), "{message}");
    assert!(message.contains("boom"), "{message}");
}

#[tokio::test]
async fn constructor_validates_url_and_api_key() {
    assert!(Client::new("", "key").is_err());
    assert!(Client::new("   ", "key").is_err());
    assert!(Client::new("ftp://jellyfin.local", "key").is_err());
    assert!(Client::new("jellyfin.local:8096", "key").is_err());
    assert!(Client::new("http://jellyfin.local", "").is_err());
    assert!(Client::new("http://jellyfin.local", "ke\ny").is_err());
    assert!(Client::new("http://jellyfin.local", "key\"").is_err());

    let trimmed = Client::new(" http://jellyfin.local/ ", " key ").expect("valid client");
    assert_eq!(trimmed.base_url(), "http://jellyfin.local");
}

#[tokio::test]
async fn invalid_ids_are_rejected_before_any_request_is_sent() {
    let server = MockServer::start().await;
    let client = client(&server);

    let message = client
        .item("../System/Info")
        .await
        .expect_err("path traversal must be rejected")
        .to_string();
    assert!(message.contains("item id"), "{message}");

    let message = client
        .image("item-1", "../Primary", 320)
        .await
        .expect_err("path traversal in the image type must be rejected")
        .to_string();
    assert!(message.contains("image type"), "{message}");

    let requests = received(&server).await;
    assert!(
        requests.is_empty(),
        "no request should reach the server, got {requests:?}"
    );
}
