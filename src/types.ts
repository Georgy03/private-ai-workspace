export type ParseStatus = "pending" | "indexing" | "ready" | "warning" | "failed";
export type ProviderKind = "bedrock" | "anthropic";
export type ProviderState = "ready" | "unavailable" | "failed";
/** Where a passage came from: this device, or the firm's cloud index (Microsoft 365, Google Drive). */
export type Origin = "local" | "cloud";
export type CloudState = "not_configured" | "off" | "signed_out" | "ready" | "unavailable";

/** How the person chose to sign in to the firm cloud (matches SignInChoice in src-tauri/src/cloud.rs). */
export type SignInChoice =
  | { kind: "any" }
  | { kind: "google" }
  | { kind: "password" }
  | { kind: "microsoft"; email: string };

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
  /** "xlsx" or "docx" when the person asked the chat to make that kind of file. */
  file?: "xlsx" | "docx" | null;
  /** What was done to answer, one plain sentence per step, in order. */
  work?: string[];
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
  /** Who the files are for: everyone at the firm (each seeing what they may open), or only the person who connected it. */
  scope?: "workspace" | "private";
  /** This person connected it. */
  yours?: boolean;
  /** For a mailbox connected for sending, the address mail is sent from. */
  label?: string | null;
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
  /** This person may connect a source for the whole workspace. Anyone may connect one for themselves. */
  can_share_with_workspace?: boolean;
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
  /** How long reports are kept; 0 means until their owner deletes them. */
  retention_days: number;
  /** The firm lets people send the emails their helpers prepare from the app. Off until an administrator says so. */
  email_sending_enabled?: boolean;
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
  /** How the report held up against the text the agent read. */
  checks?: ReportChecks | null;
  /** For a report made by a schedule: waiting for its owner, approved or rejected. */
  review_status?: "pending" | "approved" | "rejected" | null;
  /** The owner's own corrected version of the report. */
  edited_result?: string | null;
  schedule_id?: string | null;
  /** When this work will be deleted, if it will be. */
  deletes_epoch?: number | null;
  /** What the owner said when asked: keep it, or let it run out. */
  retention_choice?: "keep" | "expire" | null;
  /** Its content has been deleted. */
  deleted?: boolean;
  error?: string | null;
  events?: TaskEvent[];
}

// ---- Filtered search ----

export interface BrowseFilters {
  since_epoch?: number;
  sources?: string[];
  types?: string[];
}

export interface BrowseRow {
  document_id: string;
  name: string;
  source: string;
  file_type: string;
  app: import("./fileApp").FileApp;
  modified_epoch: number;
  link: string | null;
  excerpt: string;
  location_label: string | null;
  score: number;
}

export interface BrowseResult {
  results: BrowseRow[];
  total: number;
  facets: { sources: { value: string; count: number }[]; types: { value: string; count: number }[] };
}

export interface ReportChecks {
  figures_total: number;
  figures_found: number;
  figures_missing: string[];
  quotes_total: number;
  quotes_found: number;
  quotes_missing: string[];
}

// ---- Recurring tasks ----

export interface Cadence {
  kind: "daily" | "weekly" | "monthly";
  /** 0 is Monday. */
  weekday: number;
  /** 1 to 28. */
  day: number;
  /** Minutes after midnight, in the person's own time. */
  minutes: number;
  /** Minutes east of UTC. */
  offset: number;
}

export interface Schedule {
  id: string;
  agent_id: string;
  agent_name: string;
  name: string;
  instruction: string;
  cadence: Cadence;
  status: "active" | "paused";
  pause_reason: string | null;
  monthly_budget_micros: number | null;
  spent_this_month_micros: number;
  next_run_epoch: number | null;
  last_run_epoch: number | null;
  last_task_id: string | null;
  last_note: string | null;
  waiting_for_review: number;
}

export interface ScheduleDraft {
  agent_id: string;
  name: string;
  instruction: string;
  cadence: Cadence;
  monthly_budget_dollars: number | null;
}

// ---- Emails and meetings an agent prepared for the person to send or book themselves ----

export type PreparedAction =
  | { kind: "email"; to: string[]; cc: string[]; subject: string; body: string }
  | { kind: "meeting"; title: string; start: string; end: string; attendees: string[]; location: string; notes: string };

export type PreparedItem = PreparedAction & {
  id: string;
  task_id: string;
  task_title: string;
  agent_name: string;
  status: "proposed" | "approved" | "dismissed" | "sent";
  edited: boolean;
  /** Addresses outside the person's own organisation, to look at twice. */
  outside: string[];
  created_epoch: number;
};

/** An email written in the chat from the person's request, shown as a preview before it is sent. */
/** A Zoom meeting made from the app, which can be moved or cancelled. `start` is "2026-11-10T10:00:00" in its own time zone. */
export interface ZoomMeeting {
  id: string;
  topic: string;
  start: string;
  timezone: string;
  duration_minutes: number;
  join_url: string;
  meeting_id: string;
}

/** A workspace: everything about one client or matter in one place. */
export interface SpaceSummary {
  id: string;
  name: string;
  description: string;
  /** Files on this computer. */
  files: number;
  /** Documents pinned from the firm cloud index. */
  cloud_files: number;
  chats: number;
  /** About how much room it takes on this computer: the search index of its files and its chats. */
  stored_bytes: number;
  created_at: number;
  updated_at: number;
}

export interface SpaceFile {
  kind: "local" | "cloud";
  id: string;
  name: string;
  size: number;
  status: "ready" | "warning" | "failed" | "missing";
  warning: string | null;
  /** For a pinned cloud document, the connected source it came from. */
  source: string | null;
  added_at: number;
}

export interface SpaceSavedChat {
  id: string;
  title: string;
  updated_at: number;
}

/** What happened when files were added to a workspace. */
export interface SpaceAddResult {
  added: number;
  failed: { name: string; reason: string }[];
}

export interface EmailDraft {
  to: string[];
  cc: string[];
  subject: string;
  body: string;
  /** When the request gave a date and a time: the meeting the email is about, so a Zoom link can be made. */
  meeting?: { topic: string; start: string; duration_minutes: number } | null;
}

// ---- Chat tools ----

/** A file attached to a chat from this computer: read on this device and kept only in memory. */
export interface AttachedFile {
  id: string;
  name: string;
  characters: number;
  warnings: string[];
}

/** A file the person tagged, so the question is answered from all of it. */
export interface TaggedFile {
  id: string;
  name: string;
  origin: "local" | "cloud";
}

/** What a question was limited to, if anything. */
export type ChatScope = "local" | "cloud" | null;

/** Where a question looks: everywhere, only this device, or only the firm cloud, with the firm cloud
 *  optionally limited to some connected sources and kinds of file. */
export interface SearchLimits {
  scope: ChatScope;
  sources: string[];
  types: string[];
}

export interface ChatOptions {
  /** Ask inside a workspace: only the files that were put in it are looked at. */
  workspace?: string;
  attachments?: string[];
  tagged?: TaggedFile | null;
  scope?: ChatScope;
  /** With the firm cloud: only these connected sources, and only these kinds of file. */
  sources?: string[];
  types?: string[];
}
