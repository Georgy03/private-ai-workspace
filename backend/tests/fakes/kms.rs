#![allow(dead_code)]
//! A stand-in for AWS KMS, speaking its real JSON protocol so the real AWS client is what the tests
//! exercise. Like KMS, it wraps each data key under one key's own secret and refuses to open it unless
//! the same encryption context is given again, or if the caller has been denied the key.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, Generate, KeyInit, Payload},
};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Default)]
pub struct KmsWorld {
    /// Each key's own secret, by key ID.
    pub keys: HashMap<String, [u8; 32]>,
    /// Every request: "<Action> <key>".
    pub calls: Vec<String>,
    /// The key and context of every data key that was made.
    pub generated: Vec<(String, HashMap<String, String>)>,
    pub tags: HashMap<String, Vec<(String, String)>>,
    pub rotation: HashSet<String>,
    pub aliases: HashMap<String, String>,
    pub scheduled_deletion: Vec<String>,
    /// Keys the caller is not allowed to use.
    pub denied: HashSet<String>,
    /// Make naming a new key fail, to see what is left behind.
    pub fail_create_alias: bool,
}

pub type Kms = Arc<Mutex<KmsWorld>>;

type Gcm = Nonce<<Aes256Gcm as AeadCore>::NonceSize>;

pub async fn kms(world: Kms) -> String {
    super::serve(Router::new().route("/", post(handle)).with_state(world)).await
}

/// "arn:aws:kms:...:key/<id>" and "<id>" name the same key.
pub fn key_id(any: &str) -> String {
    any.rsplit('/').next().unwrap_or(any).to_string()
}

fn error(kind: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE, "application/x-amz-json-1.1")],
        json!({ "__type": kind, "message": message }).to_string(),
    )
        .into_response()
}

fn ok(body: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/x-amz-json-1.1")], body.to_string()).into_response()
}

fn context_text(request: &Value) -> String {
    let mut pairs: Vec<String> = request["EncryptionContext"]
        .as_object()
        .map(|map| map.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default())).collect())
        .unwrap_or_default();
    pairs.sort();
    pairs.join("&")
}

fn random_32() -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    out
}

async fn handle(State(world): State<Kms>, headers: HeaderMap, body: Bytes) -> Response {
    let action = headers
        .get("x-amz-target")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("TrentService."))
        .unwrap_or_default()
        .to_string();
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut w = world.lock().unwrap();
    let asked = request["KeyId"].as_str().map(key_id).unwrap_or_default();
    w.calls.push(format!("{action} {asked}"));
    if matches!(action.as_str(), "GenerateDataKey" | "Decrypt") {
        if w.denied.contains(&asked) {
            return error("AccessDeniedException", "the caller may not use this key");
        }
        if !w.keys.contains_key(&asked) {
            return error("NotFoundException", "no such key");
        }
    }
    match action.as_str() {
        "GenerateDataKey" => {
            let master = w.keys[&asked];
            let data_key = random_32();
            let cipher = Aes256Gcm::new_from_slice(&master).unwrap();
            let nonce = Gcm::generate();
            let context = context_text(&request);
            let sealed = cipher.encrypt(&nonce, Payload { msg: &data_key, aad: context.as_bytes() }).unwrap();
            let blob = [nonce.to_vec(), sealed].concat();
            let recorded = request["EncryptionContext"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
                .unwrap_or_default();
            w.generated.push((asked.clone(), recorded));
            ok(json!({ "CiphertextBlob": STANDARD.encode(blob), "Plaintext": STANDARD.encode(data_key), "KeyId": asked }))
        }
        "Decrypt" => {
            let blob = STANDARD.decode(request["CiphertextBlob"].as_str().unwrap_or_default()).unwrap_or_default();
            if blob.len() < 12 {
                return error("InvalidCiphertextException", "the data key is damaged");
            }
            let cipher = Aes256Gcm::new_from_slice(&w.keys[&asked]).unwrap();
            let nonce = Gcm::try_from(&blob[..12]).unwrap();
            let context = context_text(&request);
            match cipher.decrypt(&nonce, Payload { msg: &blob[12..], aad: context.as_bytes() }) {
                Ok(data_key) => ok(json!({ "Plaintext": STANDARD.encode(data_key), "KeyId": asked })),
                Err(_) => error("InvalidCiphertextException", "the encryption context does not match, or the key is wrong"),
            }
        }
        "CreateKey" => {
            let id = Uuid::new_v4().to_string();
            w.keys.insert(id.clone(), random_32());
            let tags = request["Tags"]
                .as_array()
                .map(|tags| tags.iter().map(|t| (t["TagKey"].as_str().unwrap_or_default().to_string(), t["TagValue"].as_str().unwrap_or_default().to_string())).collect())
                .unwrap_or_default();
            w.tags.insert(id.clone(), tags);
            ok(json!({ "KeyMetadata": { "KeyId": id, "Arn": format!("arn:aws:kms:us-east-2:111122223333:key/{id}") } }))
        }
        "EnableKeyRotation" => {
            w.rotation.insert(asked);
            ok(json!({}))
        }
        "CreateAlias" => {
            if w.fail_create_alias {
                return error("LimitExceededException", "too many aliases");
            }
            let alias = request["AliasName"].as_str().unwrap_or_default().to_string();
            let target = request["TargetKeyId"].as_str().map(key_id).unwrap_or_default();
            w.aliases.insert(alias, target);
            ok(json!({}))
        }
        "ScheduleKeyDeletion" => {
            w.scheduled_deletion.push(asked.clone());
            ok(json!({ "KeyId": asked, "DeletionDate": 1_800_000_000.0 }))
        }
        _ => error("UnknownOperationException", "not supported by the stand-in"),
    }
}
