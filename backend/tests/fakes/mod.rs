#![allow(dead_code)]
//! Stand-ins for Microsoft Graph and the Google Drive API. They follow the real paging and
//! change-tracking behaviour closely enough to exercise the connectors: paged listings with
//! continuation links, change feeds from a saved position, deletions, permissions and downloads.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub content: String,
    /// Raw file bytes, for PDF and Office files. Served instead of `content` when set.
    pub bytes: Option<Vec<u8>>,
    /// What the provider claims the size is, when it should differ from the real size.
    pub claimed_size: Option<u64>,
    pub version: u64,
    pub changed_at: u64,
    pub deleted: bool,
    /// Provider-shaped permission objects (Graph) or Drive permission objects.
    pub permissions: Vec<Value>,
    /// Drive only: report no permission list at all (the caller cannot see sharing).
    pub hide_permissions: bool,
}

impl Item {
    pub fn size(&self) -> u64 {
        self.bytes.as_ref().map_or(self.content.len(), |b| b.len()) as u64
    }

    pub fn body(&self) -> Vec<u8> {
        self.bytes
            .clone()
            .unwrap_or_else(|| self.content.clone().into_bytes())
    }
}

#[derive(Default)]
pub struct World {
    pub items: Vec<Item>,
    pub clock: u64,
    pub page_size: usize,
    /// Calls made, as "METHOD path" strings.
    pub calls: Vec<String>,
    /// Bearer tokens seen on requests.
    pub tokens: Vec<String>,
    pub fail_with: Option<u16>,
    /// Fail this many requests for the path containing the string, then recover.
    pub fail_path: Option<(String, u16, u32)>,
    /// Make delta/changes cursors from before this clock value invalid (410 / 400).
    pub expire_cursors_before: Option<u64>,
    /// Graph: send a continuation link that points at another host.
    pub foreign_next_link: bool,
}

pub type Shared = Arc<Mutex<World>>;

impl World {
    pub fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn add(
        &mut self,
        id: &str,
        name: &str,
        mime: &str,
        content: &str,
        permissions: Vec<Value>,
    ) {
        let at = self.tick();
        self.items.push(Item {
            id: id.into(),
            name: name.into(),
            mime: mime.into(),
            content: content.into(),
            bytes: None,
            claimed_size: None,
            version: at,
            changed_at: at,
            deleted: false,
            permissions,
            hide_permissions: false,
        });
    }

    /// A binary file (PDF, Word, ...), served as raw bytes.
    pub fn add_bytes(
        &mut self,
        id: &str,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
        permissions: Vec<Value>,
    ) {
        self.add(id, name, mime, "", permissions);
        self.items.last_mut().unwrap().bytes = Some(bytes);
    }

    pub fn edit(&mut self, id: &str, content: &str) {
        let at = self.tick();
        let item = self.items.iter_mut().find(|item| item.id == id).unwrap();
        item.content = content.into();
        item.version = at;
        item.changed_at = at;
    }

    pub fn set_permissions(&mut self, id: &str, permissions: Vec<Value>) {
        let at = self.tick();
        let item = self.items.iter_mut().find(|item| item.id == id).unwrap();
        item.permissions = permissions;
        item.changed_at = at;
    }

    pub fn remove(&mut self, id: &str) {
        let at = self.tick();
        let item = self.items.iter_mut().find(|item| item.id == id).unwrap();
        item.deleted = true;
        item.changed_at = at;
    }

    /// Forget a file without any change record, as if it vanished while a cursor was invalid.
    pub fn vanish(&mut self, id: &str) {
        self.items.retain(|item| item.id != id);
    }
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
}

fn record(world: &mut World, method: &str, path: &str, headers: &HeaderMap) -> Option<Response> {
    world.calls.push(format!("{method} {path}"));
    if let Some(token) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        world.tokens.push(token.to_string());
    }
    if let Some(status) = world.fail_with {
        return Some(StatusCode::from_u16(status).unwrap().into_response());
    }
    if let Some((needle, status, remaining)) = world.fail_path.as_mut()
        && *remaining > 0
        && path.contains(needle.as_str())
    {
        *remaining -= 1;
        return Some(StatusCode::from_u16(*status).unwrap().into_response());
    }
    None
}

// ---------------------------------------------------------------- Microsoft Graph

pub async fn graph(world: Shared) -> String {
    let router = Router::new()
        .route("/v1.0/me/drive", get(graph_my_drive))
        .route("/v1.0/drives/{drive}/root/delta", get(graph_delta))
        .route(
            "/v1.0/drives/{drive}/items/{id}/permissions",
            get(graph_permissions),
        )
        .route(
            "/v1.0/drives/{drive}/items/{id}/content",
            get(graph_content),
        )
        .with_state(world);
    serve(router).await
}

async fn graph_my_drive(State(world): State<Shared>, headers: HeaderMap) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", "/me/drive", &headers) {
        return response;
    }
    Json(json!({ "id": "drive-xyz" })).into_response()
}

fn base_of(headers: &HeaderMap) -> String {
    format!("http://{}", headers.get("host").unwrap().to_str().unwrap())
}

fn graph_item(item: &Item) -> Value {
    if item.deleted {
        return json!({ "id": item.id, "deleted": { "state": "deleted" } });
    }
    json!({
        "id": item.id, "name": item.name, "size": item.claimed_size.unwrap_or_else(|| item.size()),
        "cTag": format!("c{}", item.version), "eTag": format!("e{}", item.changed_at),
        "lastModifiedDateTime": "2026-10-01T12:00:00Z",
        "webUrl": format!("https://firm.sharepoint.com/docs/{}", item.name),
        "file": { "mimeType": item.mime },
    })
}

async fn graph_delta(
    State(world): State<Shared>,
    Path(drive): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", &format!("/delta {query:?}"), &headers) {
        return response;
    }
    // token = "<since>:<skip>"; "0:0" is a fresh listing.
    let token = query.get("token").cloned().unwrap_or_else(|| "0:0".into());
    let (since, skip) = token.split_once(':').unwrap();
    let (since, skip): (u64, usize) = (since.parse().unwrap(), skip.parse().unwrap());
    if let Some(limit) = world.expire_cursors_before
        && since != 0
        && since < limit
    {
        return StatusCode::GONE.into_response();
    }
    let pending: Vec<&Item> = world
        .items
        .iter()
        // A fresh listing does not mention things that are already gone.
        .filter(|item| item.changed_at > since && !(since == 0 && item.deleted))
        .collect();
    let page_size = if world.page_size == 0 {
        200
    } else {
        world.page_size
    };
    let page: Vec<Value> = pending
        .iter()
        .skip(skip)
        .take(page_size)
        .map(|item| graph_item(item))
        .collect();
    let base = base_of(&headers);
    let link = |token: String| {
        if world.foreign_next_link {
            format!("http://evil.invalid/v1.0/drives/{drive}/root/delta?token={token}")
        } else {
            format!("{base}/v1.0/drives/{drive}/root/delta?token={token}")
        }
    };
    let mut body = json!({
        "value": [{ "id": "root", "name": "root", "root": {}, "folder": {} }]
    });
    body["value"].as_array_mut().unwrap().extend(page);
    if skip + page_size < pending.len() {
        body["@odata.nextLink"] = json!(link(format!("{since}:{}", skip + page_size)));
    } else {
        body["@odata.deltaLink"] = json!(link(format!("{}:0", world.clock)));
    }
    Json(body).into_response()
}

async fn graph_permissions(
    State(world): State<Shared>,
    Path((_, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", &format!("/permissions/{id}"), &headers) {
        return response;
    }
    match world.items.iter().find(|item| item.id == id) {
        Some(item) => Json(json!({ "value": item.permissions })).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn graph_content(
    State(world): State<Shared>,
    Path((_, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", &format!("/content/{id}"), &headers) {
        return response;
    }
    match world.items.iter().find(|item| item.id == id) {
        Some(item) => item.body().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------- Google Drive

pub async fn drive(world: Shared) -> String {
    let router = Router::new()
        .route("/drive/v3/changes/startPageToken", get(drive_start_token))
        .route("/drive/v3/changes", get(drive_changes))
        .route("/drive/v3/files", get(drive_list))
        .route("/drive/v3/files/{id}/export", get(drive_export))
        .route("/drive/v3/files/{id}", get(drive_media))
        .with_state(world);
    serve(router).await
}

fn drive_file(item: &Item) -> Value {
    let mut file = json!({
        "id": item.id, "name": item.name, "mimeType": item.mime,
        "size": item.claimed_size.unwrap_or_else(|| item.size()).to_string(), "version": item.version.to_string(),
        "modifiedTime": "2026-10-01T12:00:00Z", "trashed": item.deleted,
        "webViewLink": format!("https://docs.google.com/file/d/{}/view", item.id),
    });
    if !item.hide_permissions {
        file["permissions"] = json!(item.permissions);
    }
    file
}

async fn drive_start_token(State(world): State<Shared>, headers: HeaderMap) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", "/startPageToken", &headers) {
        return response;
    }
    Json(json!({ "startPageToken": world.clock.to_string() })).into_response()
}

async fn drive_list(
    State(world): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", &format!("/files {query:?}"), &headers) {
        return response;
    }
    let skip: usize = query
        .get("pageToken")
        .map(|t| t.parse().unwrap())
        .unwrap_or(0);
    let live: Vec<&Item> = world.items.iter().filter(|item| !item.deleted).collect();
    let page_size = if world.page_size == 0 {
        200
    } else {
        world.page_size
    };
    let mut body = json!({ "files": live.iter().skip(skip).take(page_size).map(|i| drive_file(i)).collect::<Vec<_>>() });
    if skip + page_size < live.len() {
        body["nextPageToken"] = json!((skip + page_size).to_string());
    }
    Json(body).into_response()
}

async fn drive_changes(
    State(world): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(&mut world, "GET", &format!("/changes {query:?}"), &headers) {
        return response;
    }
    // pageToken = "<since>:<skip>"
    let token = query.get("pageToken").cloned().unwrap();
    let (since, skip) = token.split_once(':').unwrap_or((token.as_str(), "0"));
    let (since, skip): (u64, usize) = (since.parse().unwrap(), skip.parse().unwrap());
    if let Some(limit) = world.expire_cursors_before
        && since < limit
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let pending: Vec<&Item> = world
        .items
        .iter()
        .filter(|item| item.changed_at > since)
        .collect();
    let page_size = if world.page_size == 0 {
        200
    } else {
        world.page_size
    };
    let changes: Vec<Value> = pending
        .iter()
        .skip(skip)
        .take(page_size)
        .map(|item| {
            if item.deleted {
                json!({ "fileId": item.id, "removed": true })
            } else {
                json!({ "fileId": item.id, "removed": false, "file": drive_file(item) })
            }
        })
        .collect();
    let mut body = json!({ "changes": changes });
    if skip + page_size < pending.len() {
        body["nextPageToken"] = json!(format!("{since}:{}", skip + page_size));
    } else {
        body["newStartPageToken"] = json!(world.clock.to_string());
    }
    Json(body).into_response()
}

async fn drive_export(
    State(world): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(
        &mut world,
        "GET",
        &format!("/export/{id} {query:?}"),
        &headers,
    ) {
        return response;
    }
    match world.items.iter().find(|item| item.id == id) {
        Some(item) => item.body().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn drive_media(
    State(world): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut world = world.lock().unwrap();
    if let Some(response) = record(
        &mut world,
        "GET",
        &format!("/media/{id} {query:?}"),
        &headers,
    ) {
        return response;
    }
    match world.items.iter().find(|item| item.id == id) {
        Some(item) => item.body().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------- Sign-in service

/// A token endpoint that hands out access tokens for a known refresh token.
pub async fn token_endpoint(refresh_token: &'static str, calls: Arc<Mutex<u32>>) -> String {
    use axum::Form;
    let router = Router::new().route(
        "/token",
        axum::routing::post(move |Form(form): Form<HashMap<String, String>>| {
            let calls = calls.clone();
            async move {
                *calls.lock().unwrap() += 1;
                if form.get("grant_type").map(String::as_str) == Some("authorization_code") {
                    return match form.get("code").map(String::as_str) {
                        Some("good-code") => Json(json!({
                            "access_token": "access-1", "refresh_token": refresh_token, "expires_in": 3600
                        }))
                        .into_response(),
                        // A grant that carries no refresh token (consent without offline access).
                        Some("no-refresh") => Json(json!({ "access_token": "access-1", "expires_in": 3600 })).into_response(),
                        _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))).into_response(),
                    };
                }
                if form.get("refresh_token").map(String::as_str) == Some(refresh_token)
                    && form.get("grant_type").map(String::as_str) == Some("refresh_token")
                {
                    Json(json!({ "access_token": "access-1", "expires_in": 3600 })).into_response()
                } else {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": "invalid_grant" })),
                    )
                        .into_response()
                }
            }
        }),
    );
    format!("{}/token", serve(router).await)
}

pub mod cognito;
pub mod kms;
pub mod more;
