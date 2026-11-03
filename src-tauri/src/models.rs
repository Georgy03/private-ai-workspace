use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    Pending,
    Indexing,
    Ready,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Bedrock,
    Anthropic,
}

/// Where a passage came from. Local files never leave this device except as passages sent to the
/// AI model; cloud passages come from the firm's connected sources (Microsoft 365, Google Drive).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Local,
    Cloud,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CloudState {
    #[default]
    NotConfigured,
    Off,
    SignedOut,
    Ready,
    Unavailable,
}

/// The state of the firm cloud index, as shown in settings and beside each answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CloudStatus {
    pub state: CloudState,
    pub message: String,
    pub configured: bool,
    pub enabled: bool,
    pub signed_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    Ready,
    #[default]
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SourceLocation {
    pub page: Option<u32>,
    pub heading: Option<String>,
    pub sheet: Option<String>,
    pub cell_range: Option<String>,
    pub boxes: Vec<BoundingBox>,
}

impl SourceLocation {
    pub fn label(&self) -> String {
        if let Some(page) = self.page {
            return match &self.heading {
                Some(heading) => format!("p. {page} · {heading}"),
                None => format!("p. {page}"),
            };
        }
        if let Some(sheet) = &self.sheet {
            return match &self.cell_range {
                Some(range) => format!("{sheet} · {range}"),
                None => sheet.clone(),
            };
        }
        self.heading.clone().unwrap_or_else(|| "Document".into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedBlock {
    pub text: String,
    pub location: SourceLocation,
    pub ocr: bool,
    pub ocr_confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseRequest {
    pub file_name: String,
    pub extension: String,
    #[serde(with = "base64_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub blocks: Vec<ParsedBlock>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentResource {
    pub id: String,
    pub name: String,
    pub path: String,
    pub mime_type: String,
    pub size: u64,
    pub modified_at: i64,
    pub content_hash: String,
    pub status: ParseStatus,
    pub warning: Option<String>,
    pub chunk_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub citation_id: String,
    pub document_id: String,
    pub document_name: String,
    pub path: String,
    pub excerpt: String,
    pub location: SourceLocation,
    pub location_label: String,
    pub score: f64,
    pub ocr: bool,
    pub stale: bool,
    #[serde(default)]
    pub origin: Origin,
    /// Opens the original file in Google Drive or Microsoft 365. Cloud passages only.
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerClaim {
    pub text: String,
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    pub id: String,
    pub document_id: String,
    pub document_name: String,
    pub excerpt: String,
    pub location: SourceLocation,
    pub location_label: String,
    pub ocr: bool,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerResponse {
    pub claims: Vec<AnswerClaim>,
    pub citations: Vec<Citation>,
    pub abstained: bool,
    pub dropped_claims: u32,
    pub provider: ProviderStatus,
    pub privacy_receipt_id: String,
    pub receipt: PrivacyReceipt,
    #[serde(default)]
    pub cloud: CloudStatus,
    /// Something the person should know about how this answer was made, such as a document that
    /// was too long to read in full.
    #[serde(default)]
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub kind: ProviderKind,
    pub state: ProviderState,
    pub model: Option<String>,
    pub message: String,
    pub checked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub minimum_memory_gb: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalog {
    pub version: u32,
    pub models: Vec<ModelCatalogItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRuntimeStatus {
    pub runtime_present: bool,
    pub model_present: bool,
    pub model_bytes: u64,
    pub catalog_verified: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyReceipt {
    pub id: String,
    pub created_at: i64,
    pub provider: ProviderKind,
    pub model: Option<String>,
    pub document_ids: Vec<String>,
    pub passages_local: u32,
    #[serde(default)]
    pub passages_cloud: u32,
    /// Whether the question text was sent to the firm cloud index for this answer.
    #[serde(default)]
    pub cloud_question_sent: bool,
    pub state: ProviderState,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaskedCounts {
    #[serde(default)]
    pub name: u32,
    pub ssn: u32,
    pub ein: u32,
    pub account: u32,
    pub routing: u32,
    pub card: u32,
    pub email: u32,
    pub phone: u32,
}

impl MaskedCounts {
    pub fn total(&self) -> u32 {
        self.name
            + self.ssn
            + self.ein
            + self.account
            + self.routing
            + self.card
            + self.email
            + self.phone
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexJob {
    pub id: String,
    pub root: String,
    pub state: String,
    pub discovered: u32,
    pub completed: u32,
    pub failed: u32,
    pub current_file: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub roots: Vec<String>,
    pub documents: Vec<DocumentResource>,
    pub active_job: Option<IndexJob>,
    pub provider: ProviderStatus,
    pub model_runtime: ModelRuntimeStatus,
    pub sandbox_enforced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentPreview {
    pub document: DocumentResource,
    pub blocks: Vec<ParsedBlock>,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

impl Entity {
    /// Used to improve local entity retrieval. Entity aliases never leave the device.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        STANDARD.decode(value).map_err(serde::de::Error::custom)
    }
}
