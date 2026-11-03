import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  Agent,
  AgentDraft,
  AgentList,
  AgentTask,
  AgentVersion,
  AnswerResponse,
  CloudStatus,
  Connection,
  ConnectorsView,
  DocumentPreview,
  Entity,
  IndexJob,
  KnowledgeEntity,
  KnowledgeFact,
  KnowledgeOverview,
  NameOnlyFile,
  Usage,
  SearchHit,
  WorkspaceSnapshot,
} from "./types";

const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const browserSnapshot: WorkspaceSnapshot = {
  roots: ["Sample Client"],
  sandbox_enforced: false,
  provider: {
    kind: "bedrock",
    state: "ready",
    message: "Browser preview — sample answers",
  },
  model_runtime: {
    runtime_present: false,
    model_present: false,
    model_bytes: 0,
    catalog_verified: false,
    message: "Browser preview uses sample answers; install the desktop app for AI answers.",
  },
  documents: [
    {
      id: "sample-engagement",
      name: "Turnip Engagement.txt",
      path: "Sample Client/Turnip Engagement.txt",
      mime_type: "text/plain",
      size: 2480,
      modified_at: 1793145600,
      content_hash: "preview",
      status: "ready",
      chunk_count: 3,
    },
    {
      id: "sample-procedure",
      name: "Firm Procedures.md",
      path: "Sample Client/Firm Procedures.md",
      mime_type: "text/markdown",
      size: 3900,
      modified_at: 1793059200,
      content_hash: "preview",
      status: "ready",
      chunk_count: 5,
    },
    {
      id: "sample-missing",
      name: "Missing Documents.csv",
      path: "Sample Client/Missing Documents.csv",
      mime_type: "text/csv",
      size: 1280,
      modified_at: 1792972800,
      content_hash: "preview",
      status: "warning",
      warning: "One row has an unrecognized date",
      chunk_count: 2,
    },
  ],
};

const sampleBlocks: Record<string, DocumentPreview["blocks"]> = {
  "sample-engagement": [
    {
      text: "The engagement covers preparation of the 2026 federal and Illinois individual income tax returns. Payment is due within thirty days of the invoice.",
      location: { heading: "John Turnip engagement", boxes: [] },
      ocr: false,
    },
  ],
  "sample-procedure": [
    {
      text: "Confirm the engagement letter is signed before work begins. Store the final copy in the client-year folder.",
      location: { heading: "New client intake", boxes: [] },
      ocr: false,
    },
  ],
  "sample-missing": [
    {
      text: "Document: First Midwest 1099-INT · Status: Missing · Requested: 2026-10-20",
      location: { sheet: "CSV", cell_range: "rows 2-4", boxes: [] },
      ocr: false,
    },
  ],
};

export async function getWorkspace(): Promise<WorkspaceSnapshot> {
  return isTauri() ? invoke("get_workspace") : browserSnapshot;
}

export async function chooseFolder(): Promise<string | null> {
  if (!isTauri()) return null;
  const selection = await open({ directory: true, multiple: false });
  return typeof selection === "string" ? selection : null;
}

export async function startIndex(root: string): Promise<string> {
  return invoke("start_index", { root });
}

export async function installSample(): Promise<string> {
  if (!isTauri()) return "browser-sample";
  return invoke("install_sample");
}

export async function cancelIndex(): Promise<void> {
  if (isTauri()) await invoke("cancel_index");
}

let browserCloud: CloudStatus = {
  state: "ready",
  message: "Signed in. Searches include the firm cloud index.",
  configured: true,
  enabled: true,
  signed_in: true,
};

/** A sample passage that stands in for the firm's cloud index in the browser preview. */
function browserCloudHits(): SearchHit[] {
  if (browserCloud.state !== "ready") return [];
  return [
    {
      citation_id: "cloud:sample-checklist:0",
      document_id: "cloud:sample-checklist",
      document_name: "Engagement checklist.docx",
      path: "Firm cloud index",
      excerpt: "Cloud copy: the engagement checklist requires a signed engagement letter before any work begins.",
      location: { boxes: [] },
      location_label: "Cloud",
      score: 1,
      ocr: false,
      stale: false,
      origin: "cloud",
      link: "https://drive.google.com/open?id=sample-checklist",
    },
  ];
}

function browserCloudStatus(): CloudStatus {
  const { configured, enabled, signed_in } = browserCloud;
  const [state, message]: [CloudStatus["state"], string] = !configured
    ? ["not_configured", "The firm cloud index is not set up on this device."]
    : !enabled
      ? ["off", "The firm cloud index is switched off. Searches use local files only."]
      : !signed_in
        ? ["signed_out", "Sign in to search the firm cloud index."]
        : ["ready", "Signed in. Searches include the firm cloud index."];
  browserCloud = { ...browserCloud, state, message };
  return browserCloud;
}

export async function cloudStatus(): Promise<CloudStatus> {
  return isTauri() ? invoke("cloud_status") : browserCloudStatus();
}

export async function cloudSignIn(): Promise<CloudStatus> {
  if (isTauri()) return invoke("cloud_sign_in");
  browserCloud = { ...browserCloud, signed_in: true };
  return browserCloudStatus();
}

export async function cloudSignOut(): Promise<CloudStatus> {
  if (isTauri()) return invoke("cloud_sign_out");
  browserCloud = { ...browserCloud, signed_in: false };
  return browserCloudStatus();
}

export async function setCloudEnabled(enabled: boolean): Promise<CloudStatus> {
  if (isTauri()) return invoke("set_cloud_enabled", { enabled });
  browserCloud = { ...browserCloud, enabled };
  return browserCloudStatus();
}

// ---- Connected sources ----

let browserConnections: Connection[] = [];

const browserConnectors: ConnectorsView["connectors"] = [
  { kind: "google_drive", name: "Google Drive", description: "Docs, Sheets, Slides, PDFs and Office files in My Drive, shared drives and files shared with you.", available: true },
  { kind: "microsoft365", name: "Microsoft 365", description: "OneDrive and SharePoint document libraries: Word, Excel, PowerPoint and PDF files.", available: true },
];

let previewServerDown = false;

/** Test helper for the browser preview: make the firm's server look unreachable. */
export function setPreviewServerDown(down: boolean): void {
  previewServerDown = down;
}

export async function cloudConnectors(): Promise<ConnectorsView> {
  if (isTauri()) return invoke("cloud_connectors");
  if (previewServerDown) throw new Error("The firm cloud index is unavailable: it could not be reached");
  return { connectors: browserConnectors, connections: browserConnections };
}

/** Opens the provider's sign-in in the browser. The connection appears in the list once approved. */
export async function cloudConnect(kind: string, domains: string[] = [], reconnect?: string): Promise<void> {
  if (isTauri()) return invoke("cloud_connect", { kind, domains, reconnect: reconnect ?? null });
  // Browser preview: pretend the administrator approved shortly afterwards.
  setTimeout(() => {
    if (reconnect) {
      browserConnections = browserConnections.map((item) => (item.id === reconnect ? { ...item, status: "active", last_error: null } : item));
    } else {
      browserConnections = [...browserConnections, { id: `preview-${browserConnections.length + 1}`, kind, status: "active", last_sync_epoch: Math.floor(Date.now() / 1000), last_error: null, documents: 12 }];
    }
  }, 400);
}

export async function cloudDisconnect(id: string): Promise<void> {
  if (isTauri()) return invoke("cloud_disconnect", { id });
  browserConnections = browserConnections.filter((item) => item.id !== id);
}

export async function cloudConnectionFiles(id: string): Promise<NameOnlyFile[]> {
  if (isTauri()) return invoke("cloud_connection_files", { id });
  return [{ name: "Scanned letter.pdf", reason: "no_text", mime_type: "application/pdf" }];
}

let browserLimitCents: number | null = null;

function browserUsage(): Usage {
  const limit = (browserLimitCents ?? 1500) * 10_000;
  const used = 340_000;
  return { used_micros: used, limit_micros: limit, remaining_micros: Math.max(0, limit - used), answers: 24, period_end_epoch: Math.floor(Date.now() / 1000) + 86_400 * 12, limit_is_default: browserLimitCents === null };
}

/** This person's AI use this month, or null when the firm's server cannot be asked. */
export async function cloudUsage(): Promise<Usage | null> {
  if (isTauri()) return invoke("cloud_usage");
  return browserCloud.signed_in && browserCloud.enabled ? browserUsage() : null;
}

/** Sets the firm's monthly AI limit per person, in cents; null returns to the default. Administrators only. */
export async function cloudSetUsageLimit(cents: number | null): Promise<Usage> {
  if (isTauri()) return invoke("cloud_set_usage_limit", { cents });
  browserLimitCents = cents;
  return browserUsage();
}

// ---- Company knowledge ----

let previewKnowledgeOn = false;
let previewProfileConfirmed = false;
let previewFacts: KnowledgeFact[] = [
  {
    id: "f1", kind: "policy", statement: "Late invoices accrue interest at 1.5% a month after 30 days.", subject: null,
    status: "suggested", needs_review: false, stale: false, verified_by: null, verified_age_days: null,
    evidence: [{ document_id: "d1", document_name: "Engagement letter - Whitfield.docx", quote: "Late invoices accrue interest at 1.5% a month after 30 days.", link: "https://drive.google.com/open?id=d1", source_changed: false }],
  },
  {
    id: "f2", kind: "fact", statement: "Change orders must be approved in writing by the customer.", subject: null,
    status: "verified", needs_review: true, stale: false, verified_by: "boss", verified_age_days: 12,
    evidence: [{ document_id: "d1", document_name: "Engagement letter - Whitfield.docx", quote: "Change orders must be approved in writing by the customer.", link: null, source_changed: true }],
  },
];
const previewEntities: KnowledgeEntity[] = [
  { id: "e1", kind: "organization", name: "Summit Supply Co.", roles: ["vendor"], status: "suggested", mentions: 3, evidence: [{ document_id: "d2", document_name: "Materials invoice.pdf", quote: "Summit Supply Co.", link: null, source_changed: false }] },
];

function previewOverview(): KnowledgeOverview {
  const unverified = previewFacts.filter((f) => f.status === "suggested").length;
  return {
    enabled: previewKnowledgeOn, analysis_available: true, stale_after_days: 180,
    overview: {
      documents_total: 120, documents_analysed: previewKnowledgeOn ? 84 : 0, documents_failed: 1,
      document_types: [["contract", 22], ["invoice", 31], ["proposal", 9]], entities_by_kind: [["organization", 18], ["person", 41]],
      entities_unverified: previewEntities.filter((e) => e.status === "suggested").length,
      facts_total: previewFacts.length, facts_verified: previewFacts.filter((f) => f.status === "verified").length, facts_unverified: unverified,
      facts_needing_review: previewFacts.filter((f) => f.needs_review).length, facts_stale: 0, relationships: 25,
    },
    profile: previewKnowledgeOn
      ? { summary: "A residential roofing company that replaces and repairs roofs for homeowners.", services: ["Roof replacement", "Roof repair"], terminology: [{ term: "CO", meaning: "change order" }], status: previewProfileConfirmed ? "verified" : "suggested", based_on: 84, model: "claude-haiku-5-5", generated_epoch: Math.floor(Date.now() / 1000), verified_by: previewProfileConfirmed ? "boss" : null }
      : null,
  };
}

export async function cloudKnowledge(): Promise<KnowledgeOverview> {
  if (isTauri()) return invoke("cloud_knowledge");
  return previewOverview();
}

export async function cloudKnowledgeEnable(enabled: boolean): Promise<void> {
  if (isTauri()) {
    await invoke("cloud_knowledge_enable", { enabled });
    return;
  }
  previewKnowledgeOn = enabled;
}

export async function cloudKnowledgeFacts(options: { status?: string; kind?: string; reviewOnly?: boolean } = {}): Promise<KnowledgeFact[]> {
  if (isTauri()) {
    const reply = await invoke<{ facts: KnowledgeFact[] }>("cloud_knowledge_list", { what: "facts", status: options.status ?? null, kind: options.kind ?? null, query: null, reviewOnly: options.reviewOnly ?? null });
    return reply.facts;
  }
  return previewFacts.filter((f) => (options.status ? f.status === options.status : f.status !== "rejected") && (!options.reviewOnly || f.status === "suggested" || f.needs_review));
}

export async function cloudKnowledgeEntities(options: { status?: string; kind?: string; query?: string } = {}): Promise<KnowledgeEntity[]> {
  if (isTauri()) {
    const reply = await invoke<{ entities: KnowledgeEntity[] }>("cloud_knowledge_list", { what: "entities", status: options.status ?? null, kind: options.kind ?? null, query: options.query ?? null, reviewOnly: null });
    return reply.entities;
  }
  return previewEntities.filter((e) => e.status !== "rejected");
}

/** Confirms, rejects or corrects (with `text`) a fact or an entity. Administrators only. */
export async function cloudKnowledgeReview(what: "fact" | "entity", id: string, action: "confirm" | "reject" | "correct", text?: string): Promise<void> {
  if (isTauri()) {
    await invoke("cloud_knowledge_review", { what, id, action, text: text ?? null });
    return;
  }
  if (what === "fact") {
    previewFacts = previewFacts.map((f) => (f.id === id ? { ...f, statement: action === "correct" && text ? text : f.statement, status: action === "reject" ? "rejected" : "verified", needs_review: false } : f));
  } else {
    const index = previewEntities.findIndex((e) => e.id === id);
    if (index >= 0) previewEntities[index] = { ...previewEntities[index], status: action === "reject" ? "rejected" : "verified" };
  }
}

export async function cloudKnowledgeProfileReview(summary?: string, services?: string[]): Promise<void> {
  if (isTauri()) {
    await invoke("cloud_knowledge_profile_review", { summary: summary ?? null, services: services ?? null });
    return;
  }
  previewProfileConfirmed = true;
}

// ---- Shared agents ----

const AGENT_TOOLS = ["search_documents", "read_document", "lookup_knowledge"];

async function agentsCall<T>(method: string, path: string, body?: unknown, query?: [string, string][]): Promise<T> {
  return invoke<T>("cloud_agents_call", { method, path, query: query ?? null, body: body ?? null });
}

let previewAgents: Agent[] = [];
let previewTasks: AgentTask[] = [];
const previewVersions: Record<string, AgentVersion[]> = {};

function draftToAgent(draft: AgentDraft, id: string, version: number, old?: Agent): Agent {
  return {
    id, version, name: draft.name, role: draft.role, description: draft.description, icon: "search", color: "blue",
    status: draft.status ?? old?.status ?? "available", tools: draft.tools, model: draft.model, effort: draft.effort,
    audience: draft.audience ?? old?.audience ?? ["everyone"], instructions: draft.instructions,
    monthly_budget_micros: draft.monthly_budget_dollars === undefined ? (old?.monthly_budget_micros ?? null) : draft.monthly_budget_dollars === null ? null : Math.round(draft.monthly_budget_dollars * 1_000_000),
    spent_this_month_micros: old?.spent_this_month_micros ?? 0, max_concurrent: draft.max_concurrent ?? old?.max_concurrent ?? 3, running_now: 0,
  };
}

export async function cloudAgents(): Promise<AgentList> {
  if (isTauri()) return agentsCall("GET", "/v1/agents");
  return { agents: previewAgents, can_manage: true };
}

function agentBody(draft: AgentDraft) {
  const { monthly_budget_dollars, ...rest } = draft;
  const body: Record<string, unknown> = { ...rest };
  if (monthly_budget_dollars !== undefined) body.monthly_budget_micros = monthly_budget_dollars === null ? null : Math.round(monthly_budget_dollars * 1_000_000);
  return body;
}

export async function cloudAgentCreate(draft: AgentDraft): Promise<string> {
  if (isTauri()) return (await agentsCall<{ id: string }>("POST", "/v1/agents", agentBody(draft))).id;
  const id = `agent-${previewAgents.length + 1}`;
  previewAgents = [...previewAgents, draftToAgent(draft, id, 1)];
  previewVersions[id] = [{ version: 1, name: draft.name, instructions: draft.instructions, tools: draft.tools, model: draft.model, effort: draft.effort, note: "First version", created_by: "you", created_epoch: Math.floor(Date.now() / 1000), current: true }];
  return id;
}

export async function cloudAgentSave(id: string, draft: AgentDraft): Promise<void> {
  if (isTauri()) {
    await agentsCall("PUT", `/v1/agents/${id}`, agentBody(draft));
    return;
  }
  const old = previewAgents.find((a) => a.id === id);
  if (!old) return;
  const changed = old.instructions !== draft.instructions || old.name !== draft.name || old.role !== draft.role || old.model !== draft.model || old.effort !== draft.effort;
  const version = changed ? old.version + 1 : old.version;
  previewAgents = previewAgents.map((a) => (a.id === id ? draftToAgent(draft, id, version, old) : a));
  if (changed) previewVersions[id] = [{ version, name: draft.name, instructions: draft.instructions, tools: draft.tools, model: draft.model, effort: draft.effort, note: draft.note ?? "", created_by: "you", created_epoch: Math.floor(Date.now() / 1000), current: true }, ...(previewVersions[id] ?? []).map((v) => ({ ...v, current: false }))];
}

export async function cloudAgentVersions(id: string): Promise<AgentVersion[]> {
  if (isTauri()) return (await agentsCall<{ versions: AgentVersion[] }>("GET", `/v1/agents/${id}/versions`)).versions;
  return previewVersions[id] ?? [];
}

export async function cloudAgentRollback(id: string, version: number): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/agents/${id}/rollback`, { version });
    return;
  }
  const target = (previewVersions[id] ?? []).find((v) => v.version === version);
  const old = previewAgents.find((a) => a.id === id);
  if (!target || !old) return;
  const next = old.version + 1;
  previewAgents = previewAgents.map((a) => (a.id === id ? { ...a, version: next, name: target.name, instructions: target.instructions, tools: target.tools, model: target.model as Agent["model"], effort: target.effort as Agent["effort"] } : a));
  previewVersions[id] = [{ ...target, version: next, note: `Rolled back to version ${version}`, current: true }, ...previewVersions[id].map((v) => ({ ...v, current: false }))];
}

export async function cloudTaskStart(agentId: string, instruction: string): Promise<string> {
  if (isTauri()) return (await agentsCall<{ id: string }>("POST", `/v1/agents/${agentId}/tasks`, { instruction })).id;
  const agent = previewAgents.find((a) => a.id === agentId);
  const id = `task-${previewTasks.length + 1}`;
  const now = Math.floor(Date.now() / 1000);
  previewTasks = [{ id, agent_id: agentId, agent_name: agent?.name ?? "Agent", requested_by: "you", title: instruction.slice(0, 80), status: "done", turns: 2, cost_micros: 4200, created_epoch: now, started_epoch: now, finished_epoch: now, instruction, result: "## Findings\n\nThis is a sample report from the browser preview. In the app, the agent searches your documents and cites them.", sources: [], events: [{ kind: "created", summary: `${agent?.name ?? "Agent"} was given the task`, epoch: now }, { kind: "tool", summary: "Searched documents (3 found)", epoch: now }, { kind: "finished", summary: "Finished the report", epoch: now }] }, ...previewTasks];
  return id;
}

export async function cloudTasks(everyone = false): Promise<AgentTask[]> {
  if (isTauri()) return (await agentsCall<{ tasks: AgentTask[] }>("GET", "/v1/tasks", undefined, everyone ? [["everyone", "true"]] : undefined)).tasks;
  return previewTasks;
}

export async function cloudTask(id: string): Promise<AgentTask> {
  if (isTauri()) return agentsCall("GET", `/v1/tasks/${id}`);
  const found = previewTasks.find((t) => t.id === id);
  if (!found) throw new Error("That task no longer exists.");
  return found;
}

export async function cloudTaskCancel(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/tasks/${id}/cancel`);
    return;
  }
  previewTasks = previewTasks.map((t) => (t.id === id && (t.status === "queued" || t.status === "running") ? { ...t, status: "cancelled" } : t));
}

export { AGENT_TOOLS };

export async function cloudSyncNow(id: string): Promise<void> {
  if (isTauri()) return invoke("cloud_sync_now", { id });
  browserConnections = browserConnections.map((item) => (item.id === id ? { ...item, last_sync_epoch: Math.floor(Date.now() / 1000) } : item));
}

/** Test helper for the browser preview: put a connection into a known state. */
export function setPreviewConnections(connections: Connection[]): void {
  browserConnections = connections;
}

export async function searchDocuments(query: string): Promise<SearchHit[]> {
  if (isTauri()) return invoke("search_documents", { query });
  const stopWords = new Set(["a", "about", "all", "an", "and", "any", "are", "be", "can", "could", "did", "do", "does", "find", "for", "from", "give", "has", "have", "how", "in", "is", "it", "me", "my", "of", "on", "please", "show", "tell", "the", "there", "this", "to", "was", "were", "what", "when", "where", "which", "who", "with", "you"]);
  const terms = query
    .toLowerCase()
    .split(/[^\p{L}\p{N}_-]+/u)
    .filter((term) => term.length >= 2 && !stopWords.has(term));
  return Object.entries(sampleBlocks)
    .flatMap<SearchHit>(([documentId, blocks]) =>
      blocks.map((block, index) => {
        const document = browserSnapshot.documents.find((item) => item.id === documentId)!;
        const locationLabel = block.location.heading ?? `${block.location.sheet ?? "Document"} · ${block.location.cell_range ?? ""}`;
        return {
          citation_id: `${documentId}:${index}`,
          document_id: documentId,
          document_name: document.name,
          path: document.path,
          excerpt: block.text,
          location: block.location,
          location_label: locationLabel,
          score: terms.reduce((score, term) => score + (block.text.toLowerCase().includes(term) ? 1 : 0), 0),
          ocr: false,
          stale: false,
          origin: "local",
        };
      }),
    )
    .concat(browserCloudHits())
    .filter((hit) => {
      const haystack = `${hit.document_name} ${hit.excerpt}`.toLowerCase();
      return terms.length > 0 && terms.some((term) => haystack.includes(term));
    })
    .sort((left, right) => right.score - left.score);
}

/** `advanced` turns on the stronger model (Claude Sonnet), which uses about twenty times as much of the monthly allowance. */
export async function answerQuestion(question: string, advanced = false): Promise<AnswerResponse> {
  if (isTauri()) return invoke("answer_question", { question, advanced });
  const hits = (await searchDocuments(question)).slice(0, 4);
  return {
    claims: hits.map((hit) => ({ text: hit.excerpt, citation_ids: [hit.citation_id] })),
    citations: hits.map((hit) => ({
      id: hit.citation_id,
      document_id: hit.document_id,
      document_name: hit.document_name,
      excerpt: hit.excerpt,
      location: hit.location,
      location_label: hit.location_label,
      ocr: hit.ocr,
      origin: hit.origin,
      link: hit.link,
    })),
    abstained: hits.length === 0,
    dropped_claims: 0,
    provider: browserSnapshot.provider,
    privacy_receipt_id: "browser-preview",
    receipt: {
      id: "browser-preview",
      created_at: Math.floor(Date.now() / 1000),
      provider: "bedrock",
      document_ids: [...new Set(hits.map((hit) => hit.document_id))],
      passages_local: hits.filter((hit) => hit.origin === "local").length,
      passages_cloud: hits.filter((hit) => hit.origin === "cloud").length,
      cloud_question_sent: false,
      state: "ready",
      summary: "Browser preview; no remote request was made.",
    },
    cloud: browserCloudStatus(),
  };
}

/** Opens a cloud document in the browser. The desktop app refuses any address that is not a Google Drive or Microsoft 365 page. */
export async function openDocumentLink(url: string): Promise<void> {
  if (isTauri()) return invoke("open_document_link", { url });
  window.open(url, "_blank", "noopener,noreferrer");
}

export async function getDocumentPreview(documentId: string): Promise<DocumentPreview> {
  if (isTauri()) return invoke("document_preview", { documentId });
  const document = browserSnapshot.documents.find((item) => item.id === documentId);
  if (!document) throw new Error("Document not found");
  return { document, blocks: sampleBlocks[documentId] ?? [], stale: false };
}

let browserEntities: Entity[] = [];

export async function listEntities(): Promise<Entity[]> {
  return isTauri() ? invoke("list_entities") : browserEntities;
}

export async function saveEntity(name: string, aliases: string[]): Promise<Entity> {
  if (isTauri()) return invoke("save_entity", { name, aliases });
  const entity = { id: name.toLowerCase(), name, aliases };
  browserEntities = [...browserEntities.filter((item) => item.id !== entity.id), entity];
  return entity;
}

export async function removeEntity(id: string): Promise<void> {
  if (isTauri()) await invoke("remove_entity", { id });
  else browserEntities = browserEntities.filter((item) => item.id !== id);
}

export async function clearLocalData(): Promise<void> {
  if (isTauri()) await invoke("clear_local_data");
}

export async function onIndexProgress(callback: (job: IndexJob) => void) {
  if (!isTauri()) return () => undefined;
  return listen<IndexJob>("index-progress", (event) => callback(event.payload));
}
