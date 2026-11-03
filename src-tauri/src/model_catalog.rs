//! Signed, content-free model catalog handling. Catalog traffic only contains model metadata;
//! prompts, document text, embeddings and answers are never part of this protocol.

use std::{fs, path::PathBuf};

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    db::Database,
    error::{AppError, AppResult},
    models::{ModelCatalog, ModelCatalogItem, ModelRuntimeStatus},
};

const CATALOG_KEY: &str = "verified_model_catalog";
// Release engineering owns the matching private key in the macOS Keychain / release vault. Only
// this public key is compiled into the desktop application.
const CATALOG_PUBLIC_KEY_HEX: &str =
    "18e73ada1146bbc847c77e4a3053e95412a066509644cc5112cf21cefad7fde7";

#[derive(Deserialize)]
struct SignedCatalog {
    payload: String,
    signature: String,
}

pub fn verify_catalog(bytes: &[u8]) -> AppResult<ModelCatalog> {
    let envelope: SignedCatalog = serde_json::from_slice(bytes)
        .map_err(|_| AppError::Verification("Model catalog is not valid JSON.".into()))?;
    let key_bytes = hex::decode(CATALOG_PUBLIC_KEY_HEX).expect("compiled public key is valid hex");
    let key =
        VerifyingKey::from_bytes(key_bytes.as_slice().try_into().expect("32 byte public key"))
            .map_err(|_| AppError::Verification("Model catalog signing key is invalid.".into()))?;
    let signature = STANDARD
        .decode(envelope.signature)
        .map_err(|_| AppError::Verification("Model catalog signature is invalid.".into()))?;
    let signature = Signature::from_slice(&signature)
        .map_err(|_| AppError::Verification("Model catalog signature is invalid.".into()))?;
    key.verify(envelope.payload.as_bytes(), &signature)
        .map_err(|_| AppError::Verification("Model catalog signature did not verify.".into()))?;
    let catalog: ModelCatalog = serde_json::from_str(&envelope.payload)
        .map_err(|_| AppError::Verification("Model catalog payload is invalid.".into()))?;
    if catalog.models.is_empty() || catalog.models.iter().any(invalid_item) {
        return Err(AppError::Verification(
            "Model catalog contains an invalid model entry.".into(),
        ));
    }
    Ok(catalog)
}

fn invalid_item(item: &ModelCatalogItem) -> bool {
    item.id.is_empty()
        || item.name.is_empty()
        || item.version.is_empty()
        || !item.url.starts_with("https://")
        || item.sha256.len() != 64
        || item.bytes == 0
        || item.minimum_memory_gb == 0
}

pub fn store_catalog(db: &Database, bytes: &[u8]) -> AppResult<ModelCatalog> {
    let catalog = verify_catalog(bytes)?;
    db.set_setting(
        CATALOG_KEY,
        std::str::from_utf8(bytes)
            .map_err(|_| AppError::Verification("Model catalog is not UTF-8.".into()))?,
    )?;
    Ok(catalog)
}

pub fn catalog(db: &Database) -> AppResult<Option<ModelCatalog>> {
    db.setting(CATALOG_KEY)?
        .map(|value| verify_catalog(value.as_bytes()))
        .transpose()
}

pub fn runtime_status(db: &Database) -> AppResult<ModelRuntimeStatus> {
    let base = dirs::data_local_dir()
        .ok_or_else(|| AppError::Filesystem("no local data directory is available".into()))?
        .join("PrivateAIConnectorWorkspace");
    let runtime = base.join("runtime").join(if cfg!(windows) {
        "llama-cli.exe"
    } else {
        "llama-cli"
    });
    let model = base.join("models").join("recommended.gguf");
    let model_bytes = fs::metadata(&model)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let verified = catalog(db)?.is_some();
    Ok(ModelRuntimeStatus {
        runtime_present: runtime.is_file(),
        model_present: model.is_file(),
        model_bytes,
        catalog_verified: verified,
        message: if runtime.is_file() && model.is_file() {
            "Local runtime and model are installed.".into()
        } else {
            "Search-only mode until the signed local runtime and model package are installed."
                .into()
        },
    })
}

/// The caller supplies a model artifact already downloaded from the verified catalog. Its GGUF
/// magic and SHA-256 are checked before atomically replacing the active model.
pub fn install_model_file(source: PathBuf, expected_sha256: &str) -> AppResult<()> {
    let bytes = fs::read(&source).map_err(|error| AppError::Filesystem(error.to_string()))?;
    if bytes.len() < 4 || &bytes[..4] != b"GGUF" {
        return Err(AppError::Verification(
            "Selected file is not a GGUF model.".into(),
        ));
    }
    let actual = hex::encode(Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(AppError::Verification(
            "Model checksum does not match the signed catalog.".into(),
        ));
    }
    let destination = dirs::data_local_dir()
        .ok_or_else(|| AppError::Filesystem("no local data directory is available".into()))?
        .join("PrivateAIConnectorWorkspace/models");
    fs::create_dir_all(&destination).map_err(|error| AppError::Filesystem(error.to_string()))?;
    let temporary = destination.join("recommended.gguf.partial");
    fs::write(&temporary, bytes).map_err(|error| AppError::Filesystem(error.to_string()))?;
    fs::rename(temporary, destination.join("recommended.gguf"))
        .map_err(|error| AppError::Filesystem(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsigned_catalog() {
        assert!(verify_catalog(br#"{"payload":"{}","signature":""}"#).is_err());
    }
}
