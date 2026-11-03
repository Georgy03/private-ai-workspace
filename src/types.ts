export type ParseStatus = "pending" | "indexing" | "ready" | "warning" | "failed";
export type ProviderKind = "bedrock" | "anthropic";
export type ProviderState = "ready" | "unavailable" | "failed";
/** Where a passage came from: this device, or the firm's cloud index (Microsoft 365, Google Drive). */
export type Origin = "local" | "cloud";
export type CloudState = "not_configured" | "off" | "signed_out" | "ready" | "unavailable";

export interface CloudStatus {
  state: CloudState;
  message: string;
  configured: boolean;
  enabled: boolean;
  signed_in: boolean;
}

export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SourceLocation {
  page?: number;
  heading?: string;
  sheet?: string;
  cell_range?: string;
  boxes: BoundingBox[];
}

export interface ParsedBlock {
  text: string;
  location: SourceLocation;
  ocr: boolean;
  ocr_confidence?: number;
}

export interface DocumentResource {
  id: string;
  name: string;
  path: string;
  mime_type: string;
  size: number;
  modified_at: number;
  content_hash: string;
  status: ParseStatus;
  warning?: string;
  chunk_count: number;
}

export interface SearchHit {
  citation_id: string;
  document_id: string;
  document_name: string;
  path: string;
  excerpt: string;
  location: SourceLocation;
  location_label: string;
  score: number;
  ocr: boolean;
  stale: boolean;
  origin: Origin;
  /** Opens the original file in Google Drive or Microsoft 365 (cloud passages only). */
  link?: string | null;
}

export interface ProviderStatus {
  kind: ProviderKind;
  state: ProviderState;
  model?: string;
  message: string;
  checked_at?: number;
}

export interface ModelRuntimeStatus {
  runtime_present: boolean;
  model_present: boolean;
  model_bytes: number;
  catalog_verified: boolean;
  message: string;
}

export interface AnswerClaim {
  text: string;
  citation_ids: string[];
}

export interface Citation {
  id: string;
  document_id: string;
  document_name: string;
  excerpt: string;
  location: SourceLocation;
  location_label: string;
  ocr: boolean;
  origin: Origin;
  link?: string | null;
}

export interface Entity {
  id: string;
  name: string;
  aliases: string[];
}

export interface PrivacyReceipt {
  id: string;
  created_at: number;
  provider: ProviderKind;
  model?: string;
  document_ids: string[];
  passages_local: number;
  passages_cloud: number;
  /** Whether the question text was sent to the firm cloud index for this answer. */
  cloud_question_sent: boolean;
  state: ProviderState;
  summary: string;
}

export interface AnswerResponse {
  claims: AnswerClaim[];
  citations: Citation[];
  abstained: boolean;
  dropped_claims: number;
  provider: ProviderStatus;
  privacy_receipt_id: string;
  receipt: PrivacyReceipt;
  cloud: CloudStatus;
  /** How this answer was made, when the person should know (for example, a document too long to read in full). */
  notice?: string | null;
}

export interface IndexJob {
  id: string;
  root: string;
  state: string;
  discovered: number;
  completed: number;
  failed: number;
  current_file?: string;
  started_at: number;
  finished_at?: number;
}

export interface WorkspaceSnapshot {
  roots: string[];
  documents: DocumentResource[];
  active_job?: IndexJob;
  provider: ProviderStatus;
  model_runtime: ModelRuntimeStatus;
  sandbox_enforced: boolean;
}

/** A connector the firm's server offers. */
export interface ConnectorInfo {
  kind: string;
  name: string;
  description: string;
  /** Whether the server is set up for it. An unavailable connector cannot be connected. */
  available: boolean;
}

/** A source the firm has connected. */
export interface Connection {
  id: string;
  kind: string;
  status: "active" | "needs_reauth" | "disabled";
  last_sync_epoch: number | null;
  last_error: string | null;
  documents: number;
  /** A sync is running now. */
  syncing?: boolean;
  sync_started_epoch?: number | null;
  /** Files the running (or last) sync has looked at, and how many could not be read. */
  sync_files_read?: number;
  sync_files_failed?: number;
  next_sync_epoch?: number | null;
  /** Files whose text can be searched. */
  readable?: number;
  /** Why files are stored by name only, with how many of each. */
  name_only_reasons?: Record<string, number>;
}

/** One person's AI use this month, in millionths of a dollar. */
export interface Usage {
  used_micros: number;
  limit_micros: number;
  remaining_micros: number;
  answers: number;
  /** When the month ends and the allowance starts again (seconds since 1970). */
  period_end_epoch: number;
  /** The limit is the server's default, not one the firm chose. */
  limit_is_default: boolean;
}

/** A quote from a document that supports a piece of company knowledge. */
export interface KnowledgeEvidence {
  document_id: string;
  document_name: string;
  quote: string;
  link: string | null;
  /** The document has changed since this quote was taken. */
  source_changed: boolean;
}

export type KnowledgeStatus = "suggested" | "verified" | "rejected" | "outdated";

export interface KnowledgeFact {
  id: string;
  kind: "fact" | "procedure" | "decision" | "policy";
  statement: string;
  subject: string | null;
  status: KnowledgeStatus;
  needs_review: boolean;
  /** Confirmed, but not re-confirmed for longer than the freshness period. */
  stale: boolean;
  verified_by: string | null;
  verified_age_days: number | null;
  evidence: KnowledgeEvidence[];
}

export interface KnowledgeEntity {
  id: string;
  kind: "person" | "organization" | "project" | "product" | "location" | "other";
  name: string;
  roles: string[];
  status: KnowledgeStatus;
  mentions: number;
  evidence: KnowledgeEvidence[];
}

export interface KnowledgeProfile {
  summary: string;
  services: string[];
  terminology: { term: string; meaning: string }[];
  status: "suggested" | "verified";
  based_on: number;
  model: string;
  generated_epoch: number;
  verified_by: string | null;
}

export interface KnowledgeOverview {
  enabled: boolean;
  /** Whether the server has a model to read documents with. */
  analysis_available: boolean;
  stale_after_days: number;
  overview: {
    documents_total: number;
    documents_analysed: number;
    documents_failed: number;
    document_types: [string, number][];
    entities_by_kind: [string, number][];
    entities_unverified: number;
    facts_total: number;
    facts_verified: number;
    facts_unverified: number;
    facts_needing_review: number;
    facts_stale: number;
    relationships: number;
  };
  profile: KnowledgeProfile | null;
}

export interface NameOnlyFile {
  name: string;
  reason: string;
  mime_type: string;
}

export interface ConnectorsView {
  connectors: ConnectorInfo[];
  connections: Connection[];
}

export interface DocumentPreview {
  document: DocumentResource;
  blocks: ParsedBlock[];
  stale: boolean;
  /** A single passage from the firm cloud index, not a file on this device. */
  cloud?: boolean;
  /** Opens the original file, for a cloud passage. */
  link?: string | null;
}

// ---- Shared agents ----

export interface Agent {
  id: string;
  name: string;
  role: string;
  description: string;
  icon: string;
  color: string;
  status: "available" | "paused" | "archived";
  version: number;
  tools: string[];
  model: "standard" | "advanced";
  effort: "low" | "medium" | "high";
  audience: string[];
  /** Only administrators receive these. */
  instructions?: string;
  monthly_budget_micros: number | null;
  spent_this_month_micros: number;
  max_concurrent: number;
  running_now: number;
}

export interface AgentList {
  agents: Agent[];
  can_manage: boolean;
}

export interface AgentDraft {
  name: string;
  role: string;
  description: string;
  instructions: string;
  tools: string[];
  model: "standard" | "advanced";
  effort: "low" | "medium" | "high";
  status?: Agent["status"];
  audience?: string[];
  /** In dollars; null removes the limit. */
  monthly_budget_dollars?: number | null;
  max_concurrent?: number;
  note?: string;
}

export interface AgentVersion {
  version: number;
  name: string;
  instructions: string;
  tools: string[];
  model: string;
  effort: string;
  note: string;
  created_by: string;
  created_epoch: number;
  current: boolean;
}

export interface TaskEvent {
  kind: string;
  summary: string;
  epoch: number;
}

export interface AgentTask {
  id: string;
  agent_id: string;
  agent_name: string;
  requested_by: string;
  title: string;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  turns: number;
  cost_micros: number;
  created_epoch: number;
  started_epoch: number | null;
  finished_epoch: number | null;
  /** Only the person who asked receives these. */
  instruction?: string;
  result?: string;
  sources?: { document_id: string; name: string; link: string | null }[];
  error?: string | null;
  events?: TaskEvent[];
}
