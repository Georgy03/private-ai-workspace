#![allow(dead_code)]
//! Stand-ins for the Dropbox API and the DocuSign eSignature API.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
}

// ---------------------------------------------------------------- Dropbox

#[derive(Clone, Debug, Default)]
pub struct DbxFile {
    pub path: String,
    pub display: String,
    pub content: Vec<u8>,
    pub hash: String,
    pub shared: bool,
    pub members: Vec<String>,
    pub invitees: Vec<String>,
    pub deleted: bool,
    pub changed: u64,
    pub no_permission: bool,
}

#[derive(Default)]
pub struct DropboxWorld {
    pub files: Vec<DbxFile>,
    /// Folders that were deleted, with when.
    pub trees: Vec<(String, u64)>,
    pub clock: u64,
    pub page_size: usize,
    pub calls: Vec<String>,
    pub tokens: Vec<String>,
    pub root_headers: Vec<String>,
    pub reset_next: bool,
    pub team_root: bool,
}

pub type Dropbox = Arc<Mutex<DropboxWorld>>;

impl DropboxWorld {
    pub fn add(&mut self, display: &str, content: &[u8], members: &[&str]) {
        self.clock += 1;
        self.files.push(DbxFile {
            path: display.to_lowercase(),
            display: display.into(),
            content: content.to_vec(),
            hash: format!("h{}", self.clock),
            shared: !members.is_empty(),
            members: members.iter().map(|m| m.to_string()).collect(),
            changed: self.clock,
            ..Default::default()
        });
    }

    pub fn edit(&mut self, display: &str, content: &[u8]) {
        self.clock += 1;
        let clock = self.clock;
        let file = self.files.iter_mut().find(|f| f.path == display.to_lowercase()).unwrap();
        file.content = content.to_vec();
        file.hash = format!("h{clock}");
        file.changed = clock;
    }

    pub fn remove(&mut self, display: &str) {
        self.clock += 1;
        let clock = self.clock;
        let file = self.files.iter_mut().find(|f| f.path == display.to_lowercase()).unwrap();
        file.deleted = true;
        file.changed = clock;
    }

    /// Dropbox reports a deleted folder once, not once per file in it.
    pub fn remove_folder(&mut self, folder: &str) {
        self.clock += 1;
        let prefix = format!("{}/", folder.to_lowercase());
        for file in self.files.iter_mut().filter(|f| f.path.starts_with(&prefix)) {
            file.deleted = true;
            file.changed = 0;
        }
        self.trees.push((folder.to_lowercase(), self.clock));
    }
}

fn dbx_entry(file: &DbxFile) -> Value {
    let mut entry = json!({
        ".tag": "file", "id": format!("id:{}", file.path), "name": file.display.rsplit('/').next().unwrap(),
        "path_lower": file.path, "path_display": file.display, "size": file.content.len(),
        "content_hash": file.hash, "server_modified": "2026-03-01T10:00:00Z", "is_downloadable": true,
    });
    if file.shared {
        entry["sharing_info"] = json!({ "read_only": false, "parent_shared_folder_id": "sf1" });
    }
    entry
}

fn dbx_record(world: &mut DropboxWorld, path: &str, headers: &HeaderMap) {
    world.calls.push(format!("POST {path}"));
    if let Some(token) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        world.tokens.push(token.to_string());
    }
    if let Some(root) = headers.get("dropbox-api-path-root").and_then(|v| v.to_str().ok()) {
        world.root_headers.push(root.to_string());
    }
}

pub async fn dropbox(world: Dropbox) -> String {
    let router = Router::new()
        .route("/2/users/get_current_account", post(dbx_account))
        .route("/2/files/list_folder", post(dbx_list))
        .route("/2/files/list_folder/continue", post(dbx_continue))
        .route("/2/sharing/list_file_members", post(dbx_members))
        .route("/2/files/download", post(dbx_download))
        .with_state(world);
    serve(router).await
}

async fn dbx_account(State(world): State<Dropbox>, headers: HeaderMap) -> Response {
    let mut w = world.lock().unwrap();
    dbx_record(&mut w, "/2/users/get_current_account", &headers);
    let (root, home) = if w.team_root { ("team-ns", "home-ns") } else { ("home-ns", "home-ns") };
    Json(json!({ "email": "owner@firm.com", "root_info": { "root_namespace_id": root, "home_namespace_id": home } })).into_response()
}

fn dbx_page(w: &DropboxWorld, mode: &str, snap: u64, offset: usize) -> Value {
    let live: Vec<&DbxFile> = w.files.iter().filter(|f| !f.deleted).collect();
    let size = if w.page_size == 0 { 1000 } else { w.page_size };
    let page: Vec<Value> = live.iter().skip(offset).take(size).map(|f| dbx_entry(f)).collect();
    let next = offset + page.len();
    let has_more = mode == "all" && next < live.len();
    let cursor = if has_more { format!("all:{snap}:{next}") } else { format!("delta:{snap}") };
    json!({ "entries": page, "cursor": cursor, "has_more": has_more })
}

async fn dbx_list(State(world): State<Dropbox>, headers: HeaderMap) -> Response {
    let mut w = world.lock().unwrap();
    dbx_record(&mut w, "/2/files/list_folder", &headers);
    let snap = w.clock;
    Json(dbx_page(&w, "all", snap, 0)).into_response()
}

async fn dbx_continue(State(world): State<Dropbox>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let mut w = world.lock().unwrap();
    dbx_record(&mut w, "/2/files/list_folder/continue", &headers);
    if std::mem::take(&mut w.reset_next) {
        return (StatusCode::CONFLICT, Json(json!({ "error_summary": "reset/...", "error": { ".tag": "reset" } }))).into_response();
    }
    let cursor = body["cursor"].as_str().unwrap_or("");
    let parts: Vec<&str> = cursor.split(':').collect();
    match parts.as_slice() {
        ["all", snap, offset] => Json(dbx_page(&w, "all", snap.parse().unwrap(), offset.parse().unwrap())).into_response(),
        ["delta", since] => {
            let since: u64 = since.parse().unwrap();
            let mut entries: Vec<Value> = w
                .files
                .iter()
                .filter(|f| f.changed > since)
                .map(|f| if f.deleted { json!({ ".tag": "deleted", "path_lower": f.path, "name": f.display }) } else { dbx_entry(f) })
                .collect();
            entries.extend(w.trees.iter().filter(|(_, at)| *at > since).map(|(path, _)| json!({ ".tag": "deleted", "path_lower": path })));
            Json(json!({ "entries": entries, "cursor": format!("delta:{}", w.clock), "has_more": false })).into_response()
        }
        _ => (StatusCode::CONFLICT, Json(json!({ "error_summary": "reset/" }))).into_response(),
    }
}

async fn dbx_members(State(world): State<Dropbox>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let mut w = world.lock().unwrap();
    dbx_record(&mut w, "/2/sharing/list_file_members", &headers);
    let path = body["file"].as_str().unwrap_or("");
    let Some(file) = w.files.iter().find(|f| f.path == path) else {
        return (StatusCode::CONFLICT, Json(json!({ "error_summary": "invalid_file/not_found" }))).into_response();
    };
    if file.no_permission {
        return (StatusCode::CONFLICT, Json(json!({ "error_summary": "access_error/no_permission" }))).into_response();
    }
    Json(json!({
        "users": file.members.iter().map(|m| json!({ "user": { "email": m }, "access_type": { ".tag": "viewer" } })).collect::<Vec<_>>(),
        "invitees": file.invitees.iter().map(|m| json!({ "invitee": { ".tag": "email", "email": m } })).collect::<Vec<_>>(),
        "groups": [{ "group": { "group_name": "Everyone at the firm", "group_id": "g:1" } }],
    }))
    .into_response()
}

async fn dbx_download(State(world): State<Dropbox>, headers: HeaderMap) -> Response {
    let mut w = world.lock().unwrap();
    dbx_record(&mut w, "/2/files/download", &headers);
    let arg: Value = headers.get("dropbox-api-arg").and_then(|v| v.to_str().ok()).and_then(|v| serde_json::from_str(v).ok()).unwrap_or(Value::Null);
    match w.files.iter().find(|f| Some(f.path.as_str()) == arg["path"].as_str() && !f.deleted) {
        Some(file) => file.content.clone().into_response(),
        None => (StatusCode::CONFLICT, Json(json!({ "error_summary": "path/not_found/" }))).into_response(),
    }
}

// ---------------------------------------------------------------- DocuSign

#[derive(Clone, Debug)]
pub struct EnvelopeData {
    pub id: String,
    pub subject: String,
    pub status: String,
    pub sender: String,
    /// Name, email, status.
    pub signers: Vec<(String, String, String)>,
    pub pdf: Vec<u8>,
    /// Seconds since 1970 when its status last changed.
    pub changed: i64,
}

#[derive(Default)]
pub struct DocusignWorld {
    pub envelopes: Vec<EnvelopeData>,
    pub page_size: usize,
    pub calls: Vec<String>,
    /// The `from_date` of each list call.
    pub from_dates: Vec<String>,
}

pub type Docusign = Arc<Mutex<DocusignWorld>>;

impl DocusignWorld {
    #[allow(clippy::too_many_arguments)]
    pub fn add(&mut self, id: &str, subject: &str, status: &str, sender: &str, signers: &[(&str, &str, &str)], pdf: Vec<u8>, changed: i64) {
        self.envelopes.push(EnvelopeData {
            id: id.into(),
            subject: subject.into(),
            status: status.into(),
            sender: sender.into(),
            signers: signers.iter().map(|(n, e, s)| (n.to_string(), e.to_string(), s.to_string())).collect(),
            pdf,
            changed,
        });
    }

    pub fn update(&mut self, id: &str, status: &str, signers: &[(&str, &str, &str)], changed: i64) {
        let e = self.envelopes.iter_mut().find(|e| e.id == id).unwrap();
        e.status = status.into();
        e.signers = signers.iter().map(|(n, e, s)| (n.to_string(), e.to_string(), s.to_string())).collect();
        e.changed = changed;
    }
}

fn env_json(e: &EnvelopeData) -> Value {
    let when = chrono::DateTime::from_timestamp(e.changed, 0).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    json!({
        "envelopeId": e.id, "status": e.status, "emailSubject": e.subject,
        "sentDateTime": when, "statusChangedDateTime": when,
        "sender": { "userName": "Sender", "email": e.sender },
        "recipients": { "signers": e.signers.iter().map(|(n, m, s)| json!({ "name": n, "email": m, "status": s })).collect::<Vec<_>>() },
    })
}

pub async fn docusign(world: Docusign) -> String {
    let router = Router::new()
        .route("/restapi/v2.1/accounts/{account}/envelopes", get(ds_list))
        .route("/restapi/v2.1/accounts/{account}/envelopes/{id}", get(ds_one))
        .route("/restapi/v2.1/accounts/{account}/envelopes/{id}/documents/combined", get(ds_pdf))
        .with_state(world);
    serve(router).await
}

async fn ds_list(State(world): State<Docusign>, Path(_account): Path<String>, Query(query): Query<HashMap<String, String>>) -> Response {
    let mut w = world.lock().unwrap();
    w.calls.push("GET envelopes".into());
    let from = query.get("from_date").cloned().unwrap_or_default();
    w.from_dates.push(from.clone());
    let since = chrono::DateTime::parse_from_rfc3339(&from).map(|d| d.timestamp()).unwrap_or(0);
    let mut matching: Vec<&EnvelopeData> = w.envelopes.iter().filter(|e| e.changed >= since).collect();
    matching.sort_by_key(|e| e.changed);
    let start: usize = query.get("start_position").and_then(|s| s.parse().ok()).unwrap_or(0);
    let size = if w.page_size == 0 { 100 } else { w.page_size };
    let page: Vec<Value> = matching.iter().skip(start).take(size).map(|e| env_json(e)).collect();
    Json(json!({
        "resultSetSize": page.len().to_string(), "totalSetSize": matching.len().to_string(),
        "startPosition": start.to_string(), "envelopes": page,
    }))
    .into_response()
}

async fn ds_one(State(world): State<Docusign>, Path((_account, id)): Path<(String, String)>) -> Response {
    let mut w = world.lock().unwrap();
    w.calls.push(format!("GET envelopes/{id}"));
    match w.envelopes.iter().find(|e| e.id == id) {
        Some(e) => Json(env_json(e)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ds_pdf(State(world): State<Docusign>, Path((_account, id)): Path<(String, String)>) -> Response {
    let mut w = world.lock().unwrap();
    w.calls.push(format!("GET envelopes/{id}/documents/combined"));
    match w.envelopes.iter().find(|e| e.id == id) {
        Some(e) => e.pdf.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// DocuSign's token endpoint: it wants the client ID and secret in a Basic header, and hands out a
/// new refresh token each time one is used.
pub async fn docusign_token(first_refresh: &'static str, next_refresh: &'static str) -> String {
    let router = Router::new().route(
        "/token",
        post(move |headers: HeaderMap, Form(form): Form<HashMap<String, String>>| async move {
            let basic = headers.get("authorization").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("Basic "));
            if !basic || form.contains_key("client_secret") {
                return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_client" }))).into_response();
            }
            if form.get("grant_type").map(String::as_str) == Some("authorization_code") && form.get("code").map(String::as_str) == Some("good-code") {
                return Json(json!({ "access_token": "access-1", "refresh_token": first_refresh, "expires_in": 3600 })).into_response();
            }
            if matches!(form.get("refresh_token").map(String::as_str), Some(t) if t == first_refresh || t == next_refresh) {
                return Json(json!({ "access_token": "access-2", "refresh_token": next_refresh, "expires_in": 3600 })).into_response();
            }
            (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))).into_response()
        }),
    );
    serve(router).await
}

/// DocuSign's "who just signed in" call, naming the account's API address.
pub async fn docusign_userinfo(api_base: String) -> String {
    let router = Router::new().route(
        "/userinfo",
        get(move || {
            let api_base = api_base.clone();
            async move {
                Json(json!({ "email": "Owner@Firm.com", "accounts": [
                    { "account_id": "other", "base_uri": "https://na9.docusign.net", "is_default": false },
                    { "account_id": "acct-1", "base_uri": api_base, "is_default": true },
                ] }))
            }
        }),
    );
    serve(router).await
}

// ---------------------------------------------------------------- Outlook mail (Microsoft Graph)

#[derive(Default)]
pub struct MailWorld {
    /// Every message Outlook was asked to send, as Graph received it.
    pub sent: Vec<Value>,
    /// Answer every send with this status instead of accepting it.
    pub fail_with: Option<u16>,
    pub tokens: Vec<String>,
}

pub type Mail = Arc<Mutex<MailWorld>>;

pub async fn graph_mail(world: Mail) -> String {
    let router = Router::new()
        .route("/v1.0/me", get(|| async { Json(json!({ "mail": "alice@outlook.example", "userPrincipalName": "alice@firm.onmicrosoft.com" })) }))
        .route("/v1.0/me/sendMail", post(mail_send))
        .with_state(world);
    serve(router).await
}

async fn mail_send(State(world): State<Mail>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let mut w = world.lock().unwrap();
    if let Some(token) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        w.tokens.push(token.to_string());
    }
    if let Some(status) = w.fail_with {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    w.sent.push(body);
    StatusCode::ACCEPTED.into_response()
}

// ---------------------------------------------------------------- Google Drive (file upload)

#[derive(Default)]
pub struct DriveUploadWorld {
    /// Every upload as Drive received it: the content type and the whole body.
    pub uploads: Vec<(String, String)>,
    /// Every authorization header it was called with.
    pub tokens: Vec<String>,
    /// Answer every upload with this status.
    pub fail_with: Option<u16>,
}

pub type DriveUpload = Arc<Mutex<DriveUploadWorld>>;

pub async fn drive_upload(world: DriveUpload) -> String {
    let router = Router::new().route("/upload/drive/v3/files", post(drive_upload_file)).with_state(world);
    serve(router).await
}

async fn drive_upload_file(State(world): State<DriveUpload>, headers: HeaderMap, body: String) -> Response {
    let mut w = world.lock().unwrap();
    if let Some(token) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        w.tokens.push(token.to_string());
    }
    if let Some(status) = w.fail_with {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    let kind = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    w.uploads.push((kind, body));
    Json(json!({ "id": "file-1", "webViewLink": "https://docs.google.com/document/d/file-1/edit" })).into_response()
}

// ---------------------------------------------------------------- QuickBooks Online

#[derive(Default)]
pub struct QboWorld {
    /// Every record, by kind, in the order QuickBooks lists them.
    pub records: HashMap<String, Vec<Value>>,
    /// What the change feed returns on its next call, as (kind, record). Cleared once read.
    pub changed: Vec<(String, Value)>,
    /// Answer the change feed with this status instead.
    pub cdc_status: Option<u16>,
    /// The `Accept` header of every call, and the authorization header.
    pub accepts: Vec<String>,
    pub tokens: Vec<String>,
    pub queries: Vec<String>,
    pub cdc_since: Vec<String>,
}

pub type Qbo = Arc<Mutex<QboWorld>>;

impl QboWorld {
    pub fn add(&mut self, kind: &str, record: Value) {
        self.records.entry(kind.to_string()).or_default().push(record);
    }
}

pub async fn quickbooks(world: Qbo) -> String {
    let router = Router::new()
        .route("/v3/company/{realm}/query", get(qbo_query))
        .route("/v3/company/{realm}/cdc", get(qbo_cdc))
        .route("/v3/company/{realm}/reports/{name}", get(qbo_report))
        .route("/v3/company/{realm}/companyinfo/{id}", get(|| async { Json(json!({ "CompanyInfo": { "CompanyName": "Firm Books Ltd" } })) }))
        .with_state(world);
    serve(router).await
}

fn qbo_note(w: &mut QboWorld, headers: &HeaderMap) {
    w.accepts.push(headers.get("accept").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
    w.tokens.push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
}

async fn qbo_query(State(world): State<Qbo>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut w = world.lock().unwrap();
    qbo_note(&mut w, &headers);
    let text = q.get("query").cloned().unwrap_or_default();
    w.queries.push(text.clone());
    // "select * from Invoice STARTPOSITION 101 MAXRESULTS 100"
    let words: Vec<&str> = text.split_whitespace().collect();
    let kind = words.get(3).copied().unwrap_or("");
    let at = |key: &str| words.iter().position(|w| *w == key).and_then(|i| words.get(i + 1)).and_then(|n| n.parse::<usize>().ok());
    let (start, max) = (at("STARTPOSITION").unwrap_or(1), at("MAXRESULTS").unwrap_or(100));
    let all = w.records.get(kind).cloned().unwrap_or_default();
    let page: Vec<Value> = all.into_iter().skip(start - 1).take(max).collect();
    if page.is_empty() {
        return Json(json!({ "QueryResponse": {} })).into_response();
    }
    Json(json!({ "QueryResponse": { kind: page, "startPosition": start } })).into_response()
}

async fn qbo_cdc(State(world): State<Qbo>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut w = world.lock().unwrap();
    qbo_note(&mut w, &headers);
    w.cdc_since.push(q.get("changedSince").cloned().unwrap_or_default());
    if let Some(status) = w.cdc_status {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    let changed = std::mem::take(&mut w.changed);
    let mut groups: Vec<Value> = Vec::new();
    for (kind, record) in changed {
        groups.push(json!({ kind: [record] }));
    }
    Json(json!({ "CDCResponse": [{ "QueryResponse": groups }] })).into_response()
}

async fn qbo_report(State(world): State<Qbo>, headers: HeaderMap, Path((_realm, name)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut w = world.lock().unwrap();
    qbo_note(&mut w, &headers);
    let period = q.get("date_macro").cloned().unwrap_or_default();
    Json(json!({
        "Header": { "ReportName": name, "StartPeriod": "2026-01-01", "EndPeriod": "2026-12-31", "ReportBasis": "Accrual", "Currency": "CAD" },
        "Columns": { "Column": [{ "ColTitle": "" }, { "ColTitle": "Total" }] },
        "Rows": { "Row": [{ "type": "Data", "ColData": [{ "value": format!("{name} for {period}: zebra revenue") }, { "value": "5000.00" }] }] },
    }))
    .into_response()
}


// ---------------------------------------------------------------- Zoom

#[derive(Default)]
pub struct ZoomWorld {
    /// Every meeting Zoom was asked to create, as it received it.
    pub meetings: Vec<Value>,
    pub tokens: Vec<String>,
    /// Answer every call with this status instead.
    pub fail_with: Option<u16>,
    /// Every change Zoom was asked to make to a meeting: (meeting id, what it received).
    pub patches: Vec<(String, Value)>,
    /// Every meeting Zoom was asked to delete.
    pub deleted: Vec<String>,
    /// Answer "no such meeting" for changes and deletions.
    pub missing: bool,
}

pub type Zoom = Arc<Mutex<ZoomWorld>>;

pub async fn zoom(world: Zoom) -> String {
    let router = Router::new()
        .route("/v2/users/me", get(|| async { Json(json!({ "email": "alice@zoom.example", "display_name": "Alice" })) }))
        .route("/v2/users/me/meetings", post(zoom_create))
        .route("/v2/meetings/{id}", axum::routing::patch(zoom_patch).delete(zoom_delete))
        .with_state(world);
    serve(router).await
}

async fn zoom_patch(State(world): State<Zoom>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>, Json(body): Json<Value>) -> Response {
    let mut w = world.lock().unwrap();
    w.tokens.push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
    if let Some(status) = w.fail_with {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    if w.missing {
        return StatusCode::NOT_FOUND.into_response();
    }
    w.patches.push((id, body));
    StatusCode::NO_CONTENT.into_response()
}

async fn zoom_delete(State(world): State<Zoom>, headers: HeaderMap, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let mut w = world.lock().unwrap();
    w.tokens.push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
    if let Some(status) = w.fail_with {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    if w.missing {
        return StatusCode::NOT_FOUND.into_response();
    }
    w.deleted.push(id);
    StatusCode::NO_CONTENT.into_response()
}

async fn zoom_create(State(world): State<Zoom>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let mut w = world.lock().unwrap();
    w.tokens.push(headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string());
    if let Some(status) = w.fail_with {
        return StatusCode::from_u16(status).unwrap().into_response();
    }
    w.meetings.push(body);
    Json(json!({
        "id": 81234567890u64,
        "join_url": "https://us06web.zoom.us/j/81234567890?pwd=abc123",
        "start_url": "https://us06web.zoom.us/s/81234567890?zak=SECRET-HOST-TOKEN",
        "password": "abc123",
    }))
    .into_response()
}
