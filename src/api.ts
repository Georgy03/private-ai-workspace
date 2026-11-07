import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  SpaceAddResult,
  SpaceFile,
  SpaceSavedChat,
  SpaceSummary,
  ZoomMeeting,
  PreparedAction,
  PreparedItem,
  Schedule,
  ScheduleDraft,
  Cadence,
  BrowseFilters,
  BrowseResult,
  Agent,
  AgentDraft,
  AgentList,
  AgentTask,
  AgentVersion,
  AnswerResponse,
  AttachedFile,
  ChatOptions,
  CloudStatus,
  SignInChoice,
  Connection,
  ConnectorsView,
  DocumentPreview,
  EmailDraft,
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

/** Stops using a folder and forgets what was indexed from it. The files stay on the computer. */
export async function removeRoot(path: string): Promise<void> {
  if (isTauri()) await invoke("remove_root", { path });
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

export async function cloudSignIn(choice: SignInChoice = { kind: "any" }): Promise<CloudStatus> {
  if (isTauri()) return invoke("cloud_sign_in", { choice });
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
  { kind: "google_drive", name: "Google Drive", description: "Docs, Sheets, Slides, PDFs and Office files in My Drive, shared drives and files shared with you. You can also save a report into your own Drive, after you have read and approved it.", available: true },
  { kind: "microsoft365", name: "Microsoft 365", description: "OneDrive and SharePoint document libraries: Word, Excel, PowerPoint and PDF files.", available: true },
  { kind: "dropbox", name: "Dropbox", description: "Client files and shared documents in Dropbox: Word, Excel, PowerPoint, PDF and text files, with the sharing Dropbox records.", available: true },
  { kind: "docusign", name: "DocuSign", description: "Contracts and agreements sent for signature: the signed documents and who has signed or is still outstanding.", available: true },
  { kind: "quickbooks", name: "QuickBooks Online", description: "A company's customers, vendors, invoices, bills, payments, expenses and key reports, kept up to date. Read only, and only you can search what you connect.", available: true },
  { kind: "zoom", name: "Zoom", description: "Create Zoom meetings for you, after you have read and approved them, and put the join link in your emails. It cannot read your meetings, recordings or contacts.", available: true },
  { kind: "outlook_mail", name: "Outlook email", description: "Send the emails your AI helpers prepare, from your own Outlook, after you have read them. It can only send what you approve, and cannot read your mail.", available: true },
];

let previewServerDown = false;
let previewCanShare = true;

/** Test helper for the browser preview: act as an ordinary employee, who may connect only for themselves. */
export function setPreviewCanShareWithWorkspace(can: boolean): void {
  previewCanShare = can;
}

/** Test helper for the browser preview: make the firm's server look unreachable. */
export function setPreviewServerDown(down: boolean): void {
  previewServerDown = down;
}

export async function cloudConnectors(): Promise<ConnectorsView> {
  if (isTauri()) return invoke("cloud_connectors");
  if (previewServerDown) throw new Error("The firm cloud index is unavailable: it could not be reached");
  return { connectors: browserConnectors, connections: browserConnections, can_share_with_workspace: previewCanShare };
}

/** Opens the provider's sign-in in the browser. The connection appears in the list once approved. */
export async function cloudConnect(kind: string, domains: string[] = [], reconnect?: string, scope?: "private" | "workspace"): Promise<void> {
  if (isTauri()) return invoke("cloud_connect", { kind, domains, reconnect: reconnect ?? null, scope: scope ?? null });
  // Browser preview: pretend the administrator approved shortly afterwards.
  setTimeout(() => {
    if (reconnect) {
      browserConnections = browserConnections.map((item) => (item.id === reconnect ? { ...item, status: "active", last_error: null } : item));
    } else {
      browserConnections = [...browserConnections, { id: `preview-${browserConnections.length + 1}`, kind, status: "active", last_sync_epoch: Math.floor(Date.now() / 1000), last_error: null, documents: ["outlook_mail", "zoom"].includes(kind) ? 0 : 12, scope: ["outlook_mail", "quickbooks", "zoom"].includes(kind) ? "private" : (scope ?? "workspace"), yours: true, label: kind === "outlook_mail" ? "you@outlook.example" : kind === "quickbooks" ? "Firm Books Ltd" : kind === "zoom" ? "you@zoom.example" : null }];
    }
  }, 400);
}

/** Changes who a connection's files are for. Going private hides them from everyone else at once. */
export async function cloudSetScope(id: string, scope: "private" | "workspace"): Promise<void> {
  if (isTauri()) return invoke("cloud_set_scope", { id, scope });
  browserConnections = browserConnections.map((item) => (item.id === id ? { ...item, scope } : item));
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

const AGENT_TOOLS = ["search_documents", "read_document", "lookup_knowledge", "prepare_email", "prepare_meeting"];

async function agentsCall<T>(method: string, path: string, body?: unknown, query?: [string, string][]): Promise<T> {
  return invoke<T>("cloud_agents_call", { method, path, query: query ?? null, body: body ?? null });
}

let previewAgents: Agent[] = [];
let previewRetentionDays = 90;
let previewEmailSending = false;
/** What the browser preview "saved" to Drive last, so a test can check it was exactly what was approved. */
export let lastPreviewDriveSave: { title: string; content: string } | null = null;

/** The Zoom meeting the browser preview "created" last, so a test can check it was what the person approved. */
export let lastPreviewZoom: { topic: string; start: string; duration_minutes: number; timezone: string; agenda: string } | null = null;

/** What the browser preview "sent" last from the chat, so a test can check it was exactly what was shown. */
export let lastPreviewEmail: { to: string[]; subject: string; body: string; confirmOutside: boolean } | null = null;

/** Test helper for the browser preview: the firm allows sending, and this person has Outlook connected. */
export function setPreviewEmailSending(on: boolean): void {
  previewEmailSending = on;
}
// One old report that is about to be deleted, so the question can be tried in the browser preview.
let previewExpiring: AgentTask[] = [{ id: "old-1", agent_id: "agent-1", agent_name: "John", requested_by: "you", title: "Men's retreat expense report", status: "done", turns: 3, cost_micros: 3000, created_epoch: Math.floor(Date.now() / 1000) - 86400 * 83, started_epoch: null, finished_epoch: Math.floor(Date.now() / 1000) - 86400 * 83, deletes_epoch: Math.floor(Date.now() / 1000) + 86400 * 6 }];
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
  return { agents: previewAgents, can_manage: true, retention_days: previewRetentionDays, email_sending_enabled: previewEmailSending };
}

function agentBody(draft: AgentDraft) {
  const { monthly_budget_dollars, ...rest } = draft;
  const body: Record<string, unknown> = { ...rest };
  if (monthly_budget_dollars !== undefined) body.monthly_budget_micros = monthly_budget_dollars === null ? null : Math.round(monthly_budget_dollars * 1_000_000);
  return body;
}

/**
 * Drafts a helper from a sentence, for the administrator to read and change. In the desktop app only
 * the sentence goes to Claude, never a document. The browser preview makes a plain draft.
 */
export async function draftHelper(description: string): Promise<AgentDraft> {
  if (isTauri()) return invoke("draft_helper", { description });
  const text = description.trim();
  if (text.length < 10) throw new Error("Say a little more about what the helper should do.");
  const email = /\bemail|write to|reply\b/i.test(text);
  return {
    name: "Alex",
    role: text.slice(0, 40),
    description: text.slice(0, 200),
    instructions: `You help the firm with this: ${text}. Search the firm's documents, read the most relevant ones, and write a clear report with a short summary first. Quote figures and dates exactly as written. If the documents do not answer, say so and say what you did find.`,
    tools: ["search_documents", "read_document", ...(email ? ["prepare_email"] : [])],
    model: "standard",
    effort: "medium",
  };
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
  previewTasks = [{ id, agent_id: agentId, agent_name: agent?.name ?? "Agent", requested_by: "you", title: instruction.slice(0, 80), status: "done", turns: 2, cost_micros: 4200, created_epoch: now, started_epoch: now, finished_epoch: now, instruction, result: "## Findings\n\nThis is a **sample report** from the browser preview.\n\n| Category | Budgeted | Actual |\n|---|---|---|\n| Food | $2,500.00 | $2,325.48 |\n| Supplies | $2,395.00 | $1,681.27 |\n\n- Total spent: $4,006.75", sources: [], checks: { figures_total: 5, figures_found: 4, figures_missing: ["$4,006.75"], quotes_total: 0, quotes_found: 0, quotes_missing: [] }, events: [{ kind: "created", summary: `${agent?.name ?? "Agent"} was given the task`, epoch: now }, { kind: "tool", summary: "Searched documents (3 found)", epoch: now }, { kind: "finished", summary: "Finished the report", epoch: now }] }, ...previewTasks];
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

const PREVIEW_FILES = [
  { document_id: "p1", name: "Retreat 26 Small group leader guide.docx", source: "google_drive", file_type: "document", app: "google_docs" as const, modified_epoch: Math.floor(Date.now() / 1000) - 86400 * 3, link: "https://drive.google.com/open?id=p1", excerpt: "The retreat budget covers lodging, meals and materials for every small group leader.", location_label: "Page 2", score: 1 },
  { document_id: "p2", name: "Retreat budget 2026.xlsx", source: "microsoft365", file_type: "spreadsheet", app: "excel" as const, modified_epoch: Math.floor(Date.now() / 1000) - 86400 * 40, link: null, excerpt: "Total retreat cost: 18,400. Lodging 9,200, meals 6,100, materials 3,100.", location_label: "Sheet Budget", score: 0.8 },
  { document_id: "p3", name: "Retreat announcement.pdf", source: "google_drive", file_type: "pdf", app: "pdf" as const, modified_epoch: Math.floor(Date.now() / 1000) - 86400 * 400, link: "https://drive.google.com/open?id=p3", excerpt: "Join us this spring for the men's retreat.", location_label: null, score: 0.6 },
];

/** Search of the firm's connected sources: one row per file, with filters and counts for the side panel. */
export async function cloudSearchBrowse(query: string, filters: BrowseFilters): Promise<BrowseResult> {
  if (isTauri()) return invoke("cloud_search_browse", { query, filters });
  const pass = (row: (typeof PREVIEW_FILES)[number], skip?: "sources" | "types") =>
    (!filters.since_epoch || row.modified_epoch >= filters.since_epoch) &&
    (skip === "sources" || !filters.sources?.length || filters.sources.includes(row.source)) &&
    (skip === "types" || !filters.types?.length || filters.types.includes(row.file_type));
  const count = (values: string[]) => Object.entries(values.reduce<Record<string, number>>((all, v) => ({ ...all, [v]: (all[v] ?? 0) + 1 }), {})).map(([value, n]) => ({ value, count: n }));
  const matches = PREVIEW_FILES.filter((row) => `${row.name} ${row.excerpt}`.toLowerCase().includes(query.toLowerCase().split(/\s+/)[0] ?? ""));
  const results = matches.filter((row) => pass(row));
  return {
    results, total: results.length,
    facets: { sources: count(matches.filter((r) => pass(r, "sources")).map((r) => r.source)), types: count(matches.filter((r) => pass(r, "types")).map((r) => r.file_type)) },
  };
}

// ---- Recurring tasks ----

let previewSchedules: Schedule[] = [];

/** Minutes east of UTC right now, so schedules run at the person's own clock time. */
export const localOffsetMinutes = () => -new Date().getTimezoneOffset();

export async function cloudSchedules(): Promise<Schedule[]> {
  if (isTauri()) return (await agentsCall<{ schedules: Schedule[] }>("GET", "/v1/schedules", undefined, [["offset", String(localOffsetMinutes())]])).schedules;
  return previewSchedules;
}

export async function cloudScheduleCreate(draft: ScheduleDraft): Promise<string> {
  const budget = draft.monthly_budget_dollars === null ? null : Math.round(draft.monthly_budget_dollars * 1_000_000);
  if (isTauri()) {
    return (await agentsCall<{ id: string }>("POST", "/v1/schedules", { agent_id: draft.agent_id, name: draft.name, instruction: draft.instruction, cadence: draft.cadence, monthly_budget_micros: budget })).id;
  }
  const id = `schedule-${previewSchedules.length + 1}`;
  const agent = previewAgents.find((a) => a.id === draft.agent_id);
  previewSchedules = [...previewSchedules, { id, agent_id: draft.agent_id, agent_name: agent?.name ?? "Agent", name: draft.name, instruction: draft.instruction, cadence: draft.cadence, status: "active", pause_reason: null, monthly_budget_micros: budget, spent_this_month_micros: 0, next_run_epoch: Math.floor(Date.now() / 1000) + 86400 * 3, last_run_epoch: null, last_task_id: null, last_note: null, waiting_for_review: 0 }];
  return id;
}

export async function cloudScheduleUpdate(id: string, change: { status?: "active" | "paused"; name?: string; instruction?: string; cadence?: Cadence }): Promise<void> {
  if (isTauri()) {
    await agentsCall("PUT", `/v1/schedules/${id}`, change);
    return;
  }
  previewSchedules = previewSchedules.map((s) => (s.id === id ? { ...s, ...change, pause_reason: change.status === "active" ? null : s.pause_reason } : s));
}

export async function cloudScheduleRemove(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/schedules/${id}/remove`);
    return;
  }
  previewSchedules = previewSchedules.filter((s) => s.id !== id);
}

export async function cloudScheduleRun(id: string): Promise<string> {
  if (isTauri()) return (await agentsCall<{ id: string }>("POST", `/v1/schedules/${id}/run`)).id;
  const schedule = previewSchedules.find((s) => s.id === id);
  const task = await cloudTaskStart(schedule?.agent_id ?? "", schedule?.instruction ?? "");
  previewTasks = previewTasks.map((t) => (t.id === task ? { ...t, title: `${schedule?.name ?? "Report"}`, schedule_id: id, review_status: "pending" } : t));
  previewSchedules = previewSchedules.map((s) => (s.id === id ? { ...s, last_run_epoch: Math.floor(Date.now() / 1000), last_task_id: task, waiting_for_review: s.waiting_for_review + 1 } : s));
  return task;
}

/** Finished scheduled reports waiting for this person to approve, edit or reject them. */
export async function cloudInbox(): Promise<AgentTask[]> {
  if (isTauri()) return (await agentsCall<{ tasks: AgentTask[] }>("GET", "/v1/inbox")).tasks;
  return previewTasks.filter((t) => t.review_status === "pending");
}

export async function cloudTaskReview(id: string, action: "approve" | "reject", editedResult?: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/tasks/${id}/review`, { action, edited_result: editedResult ?? null });
    return;
  }
  previewTasks = previewTasks.map((t) => (t.id === id ? { ...t, review_status: action === "approve" ? "approved" : "rejected", edited_result: editedResult ?? t.edited_result } : t));
}


// ---- Keeping and deleting reports ----

/** Reports that will be deleted within a week and that the person has not yet decided about. */
export async function cloudExpiring(): Promise<AgentTask[]> {
  if (isTauri()) return (await agentsCall<{ tasks: AgentTask[] }>("GET", "/v1/tasks/expiring")).tasks;
  return previewExpiring;
}

export async function cloudTaskKeep(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/tasks/${id}/keep`);
    return;
  }
  previewExpiring = previewExpiring.filter((t) => t.id !== id);
  previewTasks = previewTasks.map((t) => (t.id === id ? { ...t, retention_choice: "keep", deletes_epoch: null } : t));
}

/** The person was asked and chose to let it be deleted on its date: it is not asked about again. */
export async function cloudTaskLetExpire(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/tasks/${id}/let-expire`);
    return;
  }
  previewExpiring = previewExpiring.filter((t) => t.id !== id);
}

export async function cloudTaskDelete(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/tasks/${id}/delete`);
    return;
  }
  previewExpiring = previewExpiring.filter((t) => t.id !== id);
  previewTasks = previewTasks.filter((t) => t.id !== id);
}

/** Administrators only. 0 keeps reports until their owners delete them. */
export async function cloudRetentionSet(days: number): Promise<void> {
  if (isTauri()) {
    await agentsCall("PUT", "/v1/agents/retention", { days });
    return;
  }
  previewRetentionDays = days;
}

// ---- Emails and meetings an agent prepared ----

const initialPreviewActions = (): PreparedItem[] => [
  { id: "act-1", task_id: "task-1", task_title: "Write to the client about the budget", agent_name: "John", kind: "email", to: ["client@other.example"], cc: [], subject: "Retreat budget update", body: "Hello,\n\nHere is where the retreat budget stands: spent $5,670.77 against $6,245.00.\n\nRegards", status: "proposed", edited: false, outside: ["client@other.example"], created_epoch: Math.floor(Date.now() / 1000) - 600 },
  { id: "act-2", task_id: "task-1", task_title: "Write to the client about the budget", agent_name: "John", kind: "meeting", title: "Budget review", start: "2026-11-05T14:00", end: "2026-11-05T15:00", attendees: ["bob@firm.com"], location: "", notes: "Go through the retreat expenses.", status: "proposed", edited: false, outside: [], created_epoch: Math.floor(Date.now() / 1000) - 500 },
];
let previewActions: PreparedItem[] = initialPreviewActions();

/** Test helper for the browser preview: another draft appears, newest first. */
export function addPreviewAction(item: PreparedItem): void {
  previewActions = [item, ...previewActions];
}

/** Test helper for the browser preview: what agents prepared goes back to the two examples. */
export function resetPreviewActions(): void {
  previewActions = initialPreviewActions();
}

/** What agents have prepared and is waiting for this person to review. */
export async function cloudActions(): Promise<PreparedItem[]> {
  if (isTauri()) return (await agentsCall<{ actions: PreparedItem[] }>("GET", "/v1/actions")).actions;
  return previewActions.filter((item) => item.status === "proposed");
}

/** Approves it, with any changes. This sends nothing: the person's own mail or calendar is opened next. */
export async function cloudActionApprove(id: string, payload?: PreparedAction): Promise<PreparedAction> {
  if (isTauri()) return (await agentsCall<{ payload: PreparedAction }>("POST", `/v1/actions/${id}/approve`, { payload: payload ?? null })).payload;
  const found = previewActions.find((item) => item.id === id);
  previewActions = previewActions.map((item) => (item.id === id ? { ...item, ...(payload ?? {}), status: "approved" } as PreparedItem : item));
  return (payload ?? found) as PreparedAction;
}

export async function cloudActionDismiss(id: string): Promise<void> {
  if (isTauri()) {
    await agentsCall("POST", `/v1/actions/${id}/dismiss`);
    return;
  }
  previewActions = previewActions.map((item) => (item.id === id ? { ...item, status: "dismissed" } : item));
}

/** Sends an email an agent prepared, from the person's own Outlook, exactly as they read it. */
export async function cloudActionSend(id: string, payload?: PreparedAction, confirmOutside = false): Promise<{ from: string | null }> {
  if (isTauri()) return agentsCall<{ from: string | null }>("POST", `/v1/actions/${id}/send`, { payload: payload ?? null, confirm_outside: confirmOutside });
  previewActions = previewActions.map((item) => (item.id === id ? ({ ...item, ...(payload ?? {}), status: "sent" } as PreparedItem) : item));
  return { from: "you@outlook.example" };
}

/** Administrators only: whether helpers' emails may be sent from the app at all. */
export async function cloudEmailSending(enabled: boolean): Promise<void> {
  if (isTauri()) {
    await agentsCall("PUT", "/v1/agents/email-sending", { enabled });
    return;
  }
  previewEmailSending = enabled;
}

/**
 * Saves a file into the person's own Google Drive, through the firm's service and the Drive API.
 * Call it only from the approval box, after they have read exactly this text. Returns a link to the
 * file.
 */
export async function cloudDriveSave(title: string, content: string): Promise<{ saved: boolean; link?: string | null }> {
  if (isTauri()) return invoke("cloud_drive_save", { title, content });
  if (!browserConnections.some((item) => item.kind === "google_drive" && item.yours && item.status === "active")) throw new Error("connect your own Google Drive first: open Connectors, Google Drive");
  lastPreviewDriveSave = { title, content };
  return { saved: true, link: "https://docs.google.com/document/d/preview/edit" };
}

/** Opens the person's own mail or calendar with a draft filled in. Only compose and new-event pages open. */
export async function openPreparedLink(url: string): Promise<void> {
  if (isTauri()) return invoke("open_prepared_link", { url });
  window.open(url, "_blank", "noopener,noreferrer");
}

/** Test helper for the browser preview: what the last "save as" would have written. */
export let lastPreviewExport: { format: string; title: string; text: string } | null = null;

/**
 * Saves a report as a Word or Excel file. The desktop app opens its own save window and returns the
 * saved place, or null if the person cancelled. The browser preview only remembers the request.
 */
export async function exportReport(format: "docx" | "xlsx", title: string, text: string): Promise<string | null> {
  if (isTauri()) return invoke<string | null>("export_report", { format, title, text });
  lastPreviewExport = { format, title, text };
  return `Preview/${title}.${format}`;
}

export async function openCalendarFile(text: string): Promise<void> {
  if (isTauri()) return invoke("open_calendar_file", { text });
}

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

/**
 * Writes an email from the person's request ("draft an email to schedule a Zoom meeting on Nov 10 at
 * 10am"), for them to read and change. Only the request and today's date go to the model.
 */
export async function draftEmail(request: string): Promise<EmailDraft> {
  const today = new Date().toLocaleDateString("en-US", { weekday: "long", year: "numeric", month: "long", day: "numeric" });
  if (isTauri()) return invoke("draft_email", { request, today });
  const address = request.match(/[\w.+-]+@[\w-]+\.[\w.-]+/)?.[0] ?? "";
  const date = request.match(/\b(nov(?:ember)?)\.?\s*(\d{1,2})\b/i);
  const time = request.match(/\b(\d{1,2})(?::(\d{2}))?\s*(am|pm)\b/i);
  const when = date ? `November ${date[2]}${time ? ` at ${time[1]}:${time[2] ?? "00"} ${time[3].toUpperCase()}` : ""}` : "a time that suits you";
  const zoom = /zoom|meeting|\bmeet\b|\bcall\b/i.test(request);
  // A date without a year means the next one that comes.
  let meeting: EmailDraft["meeting"] = null;
  if (zoom && date && time) {
    const month = 10;
    const now = new Date();
    let year = now.getFullYear();
    if (new Date(year, month, Number(date[2]), 23, 59) < now) year += 1;
    const hour = (Number(time[1]) % 12) + (time[3].toLowerCase() === "pm" ? 12 : 0);
    meeting = { topic: `Zoom meeting on ${when}`, start: `${year}-${String(month + 1).padStart(2, "0")}-${date[2].padStart(2, "0")}T${String(hour).padStart(2, "0")}:${time[2] ?? "00"}`, duration_minutes: 30 };
  }
  return {
    meeting,
    to: address ? [address] : [],
    cc: [],
    subject: zoom ? `Zoom meeting on ${when}` : "Following up",
    body: zoom
      ? `Hello,\n\nI would like to arrange a Zoom meeting on ${when}. Would that work for you? If not, please suggest another time that suits you.\n\nOnce it is confirmed I will send the Zoom link.\n\nBest regards,`
      : `Hello,\n\nI am writing to follow up on ${request.replace(/^(please\s+)?(draft|write|compose)\s+(an?\s+)?e-?mail\s+(to\s+)?/i, "").trim() || "our conversation"}.\n\nBest regards,`,
  };
}

let previewZoomMeetings: ZoomMeeting[] = [];
/** What the browser preview last moved or cancelled in "Zoom", so a test can check it was what the person chose. */
export let lastPreviewZoomChange: { id: string; action: "moved" | "cancelled"; start?: string; duration_minutes?: number } | null = null;

/** The Zoom meetings made from the app that are still on: the only ones it can change. */
export async function zoomMeetings(): Promise<ZoomMeeting[]> {
  if (isTauri()) return (await invoke<{ meetings: ZoomMeeting[] }>("cloud_zoom_meetings")).meetings;
  return previewZoomMeetings;
}

/** Moves one of those meetings, after the person chose "Save new time". The link stays the same. */
export async function rescheduleZoomMeeting(id: string, start: string, durationMinutes: number, timezone: string): Promise<void> {
  if (isTauri()) {
    await invoke("cloud_zoom_reschedule", { id, start, durationMinutes, timezone });
    return;
  }
  if (!previewZoomMeetings.some((meeting) => meeting.id === id)) throw new Error("not found");
  previewZoomMeetings = previewZoomMeetings.map((meeting) => (meeting.id === id ? { ...meeting, start: `${start}:00`, duration_minutes: durationMinutes, timezone } : meeting));
  lastPreviewZoomChange = { id, action: "moved", start, duration_minutes: durationMinutes };
}

/** Cancels one of those meetings, after the person confirmed. */
export async function cancelZoomMeeting(id: string): Promise<void> {
  if (isTauri()) {
    await invoke("cloud_zoom_cancel", { id });
    return;
  }
  if (!previewZoomMeetings.some((meeting) => meeting.id === id)) throw new Error("not found");
  previewZoomMeetings = previewZoomMeetings.filter((meeting) => meeting.id !== id);
  lastPreviewZoomChange = { id, action: "cancelled" };
}

/**
 * Creates a Zoom meeting in the person's own Zoom, after they chose "Add a Zoom link", and returns the
 * link to join it. The browser preview makes a made-up link.
 */
export async function createZoomMeeting(meeting: { topic: string; start: string; duration_minutes: number; timezone: string; agenda: string }): Promise<{ join_url: string; meeting_id?: string | null; passcode?: string | null }> {
  if (isTauri()) return invoke("cloud_zoom_meeting", { topic: meeting.topic, start: meeting.start, durationMinutes: meeting.duration_minutes, timezone: meeting.timezone, agenda: meeting.agenda });
  if (!browserConnections.some((item) => item.kind === "zoom" && item.status === "active")) throw new Error("connect your Zoom first: open Connectors, Zoom");
  lastPreviewZoom = meeting;
  previewZoomMeetings = [...previewZoomMeetings, { id: `00000000-0000-4000-8000-${String(previewZoomMeetings.length + 1).padStart(12, "0")}`, topic: meeting.topic, start: `${meeting.start}:00`, timezone: meeting.timezone, duration_minutes: meeting.duration_minutes, join_url: "https://us06web.zoom.us/j/81234567890?pwd=preview", meeting_id: "81234567890" }];
  return { join_url: "https://us06web.zoom.us/j/81234567890?pwd=preview", meeting_id: "81234567890", passcode: "preview" };
}

/**
 * Sends the email in the preview from the person's own Outlook, after they pressed Send. The desktop
 * app refuses anything not sent from that button.
 */
export async function sendChatEmail(draft: { to: string[]; cc: string[]; subject: string; body: string }, confirmOutside = false): Promise<{ sent: boolean; from?: string | null }> {
  if (isTauri()) return invoke("cloud_send_email", { to: draft.to, cc: draft.cc, subject: draft.subject, body: draft.body, confirmOutside });
  if (!previewEmailSending) throw new Error("sending email from the app is switched off for your firm: ask an administrator");
  if (!browserConnections.some((item) => item.kind === "outlook_mail" && item.status === "active")) throw new Error("connect your Outlook first: open Connectors, Outlook email");
  if (draft.to.some((address) => !address.toLowerCase().endsWith("@firm.com")) && !confirmOutside) throw new Error("confirm that you have checked the people outside your organisation");
  lastPreviewEmail = { to: draft.to, subject: draft.subject, body: draft.body, confirmOutside };
  return { sent: true, from: "you@outlook.example" };
}

/**
 * The web tab: regular Claude, for research or any question. It searches the web when that helps and
 * uses none of the person's documents. `earlier` is the earlier answers from the web tab. The browser
 * preview answers with a made-up reply and made-up pages.
 */
export async function askWeb(question: string, advanced = false, earlier: { question: string; answer: string; web?: boolean }[] = [], attachments: string[] = []): Promise<AnswerResponse> {
  if (isTauri()) return invoke("ask_web", { question, advanced, earlier, attachments });
  const pages = [
    { title: "Men's retreat Scripture ideas", url: "https://example.org/mens-retreat-verses", cited: "Proverbs 27:17: As iron sharpens iron, so one man sharpens another." },
    { title: "Planning a faith retreat", url: "https://example.org/planning-a-retreat", cited: "Build in time for prayer, worship and honest conversation." },
  ];
  const text = `Here is what I found for “${question}”.\n\n- **Proverbs 27:17** — iron sharpens iron, so one man sharpens another.\n- **Hebrews 10:24–25** — spur one another on toward love and good deeds, and keep meeting together.\n- **Ecclesiastes 4:9–10** — two are better than one, because they have a good return for their labor.`;
  return {
    claims: [{ text, citation_ids: pages.map((_, index) => `web:${index}`) }],
    citations: pages.map((page, index) => ({ id: `web:${index}`, document_id: `web:${index}`, document_name: page.title, excerpt: page.cited, location: {}, location_label: "Web", ocr: false, origin: "cloud" as const, link: page.url })),
    abstained: false,
    dropped_claims: 0,
    provider: browserSnapshot.provider,
    privacy_receipt_id: "browser-preview",
    receipt: { id: "browser-preview", created_at: Math.floor(Date.now() / 1000), provider: "bedrock", document_ids: [], passages_local: 0, passages_cloud: 0, cloud_question_sent: false, state: "ready", summary: "Web tab (browser preview): no request was made, and none of your documents were used." },
    cloud: browserCloudStatus(),
    work: ["Asked in the web tab, without your indexed documents.", ...(attachments.length ? [`Included the ${attachments.length} file(s) you attached.`] : []), "Searched the web 2 times.", "Cited 2 web page(s)."],
  } as unknown as AnswerResponse;
}

/** `earlier` is the last few exchanges of a chat, so a follow-up question can be understood. */
/**
 * Asks a question. With `report` (a report's ID), it is answered only from that report and the files
 * it was drawn from, using whatever the person may open.
 */
/** `advanced` turns on the stronger model (Claude Sonnet), which uses about twenty times as much of the monthly allowance. */
export async function answerQuestion(question: string, advanced = false, earlier: { question: string; answer: string; web?: boolean }[] = [], report?: string, options: ChatOptions = {}): Promise<AnswerResponse> {
  if (isTauri()) {
    return invoke("answer_question", { question, advanced, earlier, report: report ?? null, attachments: options.attachments ?? null, tagged: options.tagged ?? null, scope: options.scope ?? null, sources: options.sources ?? null, types: options.types ?? null, workspace: options.workspace ?? null });
  }
  if (options.workspace) return previewSpaceAnswer(options.workspace, question);
  if (report) {
    const task = previewTasks.find((item) => item.id === report);
    const text = task?.edited_result ?? task?.result ?? "";
    const source = task?.sources?.[0];
    const known = /figure|number|check|biggest|total|cost|budget|retreat|point|revenue|report/i.test(question);
    const citations = [
      { id: "report", document_id: "report", document_name: `The report “${task?.title ?? "Report"}”`, excerpt: text.slice(0, 200), location: {}, location_label: "Report", ocr: false, origin: "cloud" as const, link: null },
      ...(source ? [{ id: `cloud:${source.document_id}`, document_id: `cloud:${source.document_id}`, document_name: source.name, excerpt: "", location: {}, location_label: "Cloud", ocr: false, origin: "cloud" as const, link: source.link }] : []),
    ];
    return {
      claims: known ? [{ text: `The report states this on its first lines, and ${source?.name ?? "the file it used"} is where it was taken from.`, citation_ids: citations.map((c) => c.id) }] : [],
      citations: known ? citations : [],
      abstained: !known,
      dropped_claims: 0,
      provider: browserSnapshot.provider,
      privacy_receipt_id: "browser-preview",
      receipt: { id: "browser-preview", created_at: Math.floor(Date.now() / 1000), provider: "bedrock", document_ids: [], passages_local: 0, passages_cloud: citations.length, cloud_question_sent: true, state: "ready", summary: "Browser preview; no remote request was made." },
      cloud: browserCloudStatus(),
    } as unknown as AnswerResponse;
  }
  const wantsFile = /\b(create|make|build|generate)\b/i.test(question) && /excel|spreadsheet|sheet|word/i.test(question);
  const subject = wantsFile ? question.replace(/\b(create|make|build|generate|me|a|an|excel|spreadsheet|sheet|word|document|file)\b/gi, " ") : question;
  // Limits to sources and kinds of file mean the firm cloud only. The preview's cloud files are Google Drive's.
  const kindOf = (name: string) => (/\.(docx?|txt|md)$/i.test(name) ? "document" : /\.(xlsx?|csv)$/i.test(name) ? "spreadsheet" : /\.pptx?$/i.test(name) ? "presentation" : /\.pdf$/i.test(name) ? "pdf" : "other");
  const narrowed = (options.sources?.length ?? 0) + (options.types?.length ?? 0) > 0;
  const scoped = (await searchDocuments(subject)).filter((hit) => !options.scope || hit.origin === options.scope).filter(
    (hit) => !narrowed || (hit.origin === "cloud" && (!options.sources?.length || options.sources.includes("google_drive")) && (!options.types?.length || options.types.includes(kindOf(hit.document_name)))),
  );
  const chosen = options.tagged ? [options.tagged.name] : (options.attachments ?? []).map((id) => previewAttachments.get(id) ?? "Attached file");
  const hits = scoped.slice(0, 4);
  const chosenHits: SearchHit[] = chosen.map((name, index) => ({ citation_id: `file:${index}`, document_id: `attachment:${index}`, document_name: name, path: "Chosen file", excerpt: `All of ${name} was read for this answer.`, location: { boxes: [] }, location_label: "Chosen file", score: 1, ocr: false, stale: false, origin: "local" as const, link: null }));
  const shown = [...chosenHits, ...hits];
  return {
    claims: shown.map((hit) => ({ text: hit.excerpt, citation_ids: [hit.citation_id] })),
    citations: shown.map((hit) => ({
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
    abstained: shown.length === 0,
    dropped_claims: 0,
    provider: browserSnapshot.provider,
    privacy_receipt_id: "browser-preview",
    receipt: {
      id: "browser-preview",
      created_at: Math.floor(Date.now() / 1000),
      provider: "bedrock",
      document_ids: [...new Set(shown.map((hit) => hit.document_id))],
      passages_local: shown.filter((hit) => hit.origin === "local").length,
      passages_cloud: shown.filter((hit) => hit.origin === "cloud").length,
      cloud_question_sent: false,
      state: "ready",
      summary: "Browser preview; no remote request was made.",
    },
    cloud: browserCloudStatus(),
    work: [
      `Searched ${options.scope === "local" ? "on this device only" : options.scope === "cloud" || narrowed ? "in the firm cloud only" : "on this device and in the firm cloud"} for “${subject.trim()}”.`,
      `Found ${scoped.filter((hit) => hit.origin === "local").length} passage(s) on this device and ${scoped.filter((hit) => hit.origin === "cloud").length} in the firm cloud.`,
      ...(chosen.length ? [`Included the file(s) you chose: ${chosen.join(", ")}.`] : []),
      `Picked the ${hits.length} most relevant passages out of ${scoped.length}.`,
      `Sent your question and ${shown.length} passage(s) from ${new Set(shown.map((hit) => hit.document_id)).size} document(s) to Claude Haiku 5.5.`,
      "Checked each statement against its source; every one is supported.",
      `Cited ${shown.length} source(s).`,
    ],
    file: /\b(create|make|build|generate)\b/i.test(question) ? (/excel|spreadsheet|sheet/i.test(question) ? "xlsx" : /word/i.test(question) ? "docx" : null) : null,
  };
}

const previewAttachments = new Map<string, string>();

// ---- Workspaces ----

interface PreviewSpace {
  summary: Omit<SpaceSummary, "files" | "cloud_files" | "chats" | "stored_bytes">;
  files: SpaceFile[];
  chats: Map<string, { title: string; body: string; updated_at: number }>;
}
let previewSpaces: PreviewSpace[] = [];
let previewSpaceNumber = 0;
const SAMPLE_CASE_FILES = ["Retainer agreement.pdf", "Intake notes.docx", "Correspondence with opposing counsel.docx", "Court filing.pdf", "Medical records summary.pdf"];

const spaceSummary = (space: PreviewSpace): SpaceSummary => ({
  ...space.summary,
  files: space.files.filter((file) => file.kind === "local").length,
  cloud_files: space.files.filter((file) => file.kind === "cloud").length,
  chats: space.chats.size,
  stored_bytes: space.files.filter((file) => file.kind === "local").length * 24_000 + [...space.chats.values()].reduce((total, chat) => total + chat.body.length, 0),
});
const spaceOf = (id: string) => {
  const space = previewSpaces.find((item) => item.summary.id === id);
  if (!space) throw new Error("That workspace no longer exists.");
  return space;
};
const touch = (space: PreviewSpace) => { space.summary.updated_at = Math.floor(Date.now() / 1000); };

/** The browser preview answers from the names of a workspace's files only. */
function previewSpaceAnswer(id: string, question: string): AnswerResponse {
  const space = spaceOf(id);
  const shown = space.files.slice(0, 3).map((file, index) => ({ id: `ws:${index}`, document_id: file.kind === "cloud" ? `cloud:${file.id}` : file.id, document_name: file.name, excerpt: `From ${file.name}: the passage that bears on “${question.trim().slice(0, 60)}”.`, location: { boxes: [] }, location_label: "Page 1", ocr: false, origin: (file.kind === "cloud" ? "cloud" : "local") as "cloud" | "local", link: null }));
  return {
    claims: shown.map((citation) => ({ text: citation.excerpt, citation_ids: [citation.id] })),
    citations: shown,
    abstained: shown.length === 0,
    dropped_claims: 0,
    provider: browserSnapshot.provider,
    privacy_receipt_id: "browser-preview",
    receipt: { id: "browser-preview", created_at: Math.floor(Date.now() / 1000), provider: "bedrock", document_ids: shown.map((citation) => citation.document_id), passages_local: shown.filter((c) => c.origin === "local").length, passages_cloud: shown.filter((c) => c.origin === "cloud").length, cloud_question_sent: false, state: "ready", summary: "Browser preview; no remote request was made." },
    cloud: browserCloudStatus(),
    work: [`Looked only in the workspace “${space.summary.name}”: ${space.files.length} file(s).`],
  } as unknown as AnswerResponse;
}

export async function spaces(): Promise<SpaceSummary[]> {
  if (isTauri()) return invoke("workspace_list");
  return previewSpaces.map(spaceSummary).sort((a, b) => b.updated_at - a.updated_at || a.name.localeCompare(b.name));
}

export async function spaceCreate(name: string, description: string): Promise<SpaceSummary> {
  if (isTauri()) return invoke("workspace_create", { name, description });
  const clean = name.split(/\s+/).filter(Boolean).join(" ");
  if (!clean || clean.length > 80) throw new Error("Give the workspace a name of 1 to 80 characters.");
  if (previewSpaces.some((item) => item.summary.name.toLowerCase() === clean.toLowerCase())) throw new Error("You already have a workspace with that name.");
  const now = Math.floor(Date.now() / 1000);
  const space: PreviewSpace = { summary: { id: `space-${++previewSpaceNumber}`, name: clean, description: description.trim(), created_at: now, updated_at: now }, files: [], chats: new Map() };
  previewSpaces = [...previewSpaces, space];
  return spaceSummary(space);
}

export async function spaceUpdate(id: string, name: string, description: string): Promise<SpaceSummary> {
  if (isTauri()) return invoke("workspace_update", { id, name, description });
  const space = spaceOf(id);
  const clean = name.split(/\s+/).filter(Boolean).join(" ");
  if (!clean || clean.length > 80) throw new Error("Give the workspace a name of 1 to 80 characters.");
  if (previewSpaces.some((item) => item.summary.id !== id && item.summary.name.toLowerCase() === clean.toLowerCase())) throw new Error("You already have a workspace with that name.");
  space.summary.name = clean;
  space.summary.description = description.trim();
  touch(space);
  return spaceSummary(space);
}

export async function spaceDelete(id: string): Promise<number> {
  if (isTauri()) return invoke("workspace_delete", { id });
  const space = spaceOf(id);
  previewSpaces = previewSpaces.filter((item) => item !== space);
  return space.files.filter((file) => file.kind === "local").length;
}

export async function spaceFiles(id: string): Promise<SpaceFile[]> {
  if (isTauri()) return invoke("workspace_files", { id });
  return [...spaceOf(id).files].sort((a, b) => b.added_at - a.added_at);
}

let previewFileNumber = 0;
function previewAdd(id: string, count: number): SpaceAddResult {
  const space = spaceOf(id);
  for (let index = 0; index < count; index += 1) {
    const name = SAMPLE_CASE_FILES[previewFileNumber % SAMPLE_CASE_FILES.length];
    previewFileNumber += 1;
    space.files.push({ kind: "local", id: `local-${previewFileNumber}`, name, size: 180_000, status: "ready", warning: null, source: null, added_at: Math.floor(Date.now() / 1000) + previewFileNumber });
  }
  touch(space);
  return { added: count, failed: [] };
}

/** Lets the person pick files from this computer. The desktop app opens its own window; it returns nothing if they cancelled. */
export async function spaceAddFiles(id: string): Promise<SpaceAddResult | null> {
  if (isTauri()) return invoke("workspace_add_files", { id });
  return previewAdd(id, 2);
}

/** Lets the person pick a folder and adds the readable files in it. */
export async function spaceAddFolder(id: string): Promise<SpaceAddResult | null> {
  if (isTauri()) return invoke("workspace_add_folder", { id });
  return previewAdd(id, 3);
}

/** Pins a document found in the firm cloud index. Only its name and ID are kept on this computer. */
export async function spaceAddCloud(id: string, document: string, name: string, source: string): Promise<void> {
  if (isTauri()) return invoke("workspace_add_cloud", { id, document, name, source });
  const space = spaceOf(id);
  if (!space.files.some((file) => file.kind === "cloud" && file.id === document)) space.files.push({ kind: "cloud", id: document, name, size: 0, status: "ready", warning: null, source, added_at: Math.floor(Date.now() / 1000) + ++previewFileNumber });
  touch(space);
}

export async function spaceRemoveFile(id: string, kind: "local" | "cloud", reference: string): Promise<boolean> {
  if (isTauri()) return invoke("workspace_remove_file", { id, kind, reference });
  const space = spaceOf(id);
  space.files = space.files.filter((file) => !(file.kind === kind && file.id === reference));
  touch(space);
  return kind === "local";
}

export async function spaceChats(id: string): Promise<SpaceSavedChat[]> {
  if (isTauri()) return invoke("workspace_chats", { id });
  return [...spaceOf(id).chats].map(([chat, value]) => ({ id: chat, title: value.title, updated_at: value.updated_at })).sort((a, b) => b.updated_at - a.updated_at);
}

export async function spaceChat(id: string, chat: string): Promise<string | null> {
  if (isTauri()) return invoke("workspace_chat", { id, chat });
  return spaceOf(id).chats.get(chat)?.body ?? null;
}

export async function spaceSaveChat(id: string, chat: string, title: string, body: string): Promise<void> {
  if (isTauri()) return invoke("workspace_save_chat", { id, chat, title, body });
  spaceOf(id).chats.set(chat, { title, body, updated_at: Math.floor(Date.now() / 1000) });
}

export async function spaceDeleteChat(id: string, chat: string): Promise<void> {
  if (isTauri()) return invoke("workspace_delete_chat", { id, chat });
  spaceOf(id).chats.delete(chat);
}

/** Start the browser preview's workspaces from nothing again, for tests. */
export function resetPreviewSpaces() {
  previewSpaces = [];
}

/**
 * Lets the person pick a file from this computer to ask about. The desktop app opens its own window
 * and reads the file in its isolated parser; it returns nothing if they cancelled. The browser
 * preview pretends a file was picked.
 */
export async function attachFile(): Promise<AttachedFile | null> {
  if (isTauri()) return invoke<AttachedFile | null>("attach_file");
  const id = `attach-${previewAttachments.size + 1}`;
  const name = "Example contract.pdf";
  previewAttachments.set(id, name);
  return { id, name, characters: 4200, warnings: [] };
}

export async function removeAttachment(id: string): Promise<void> {
  if (isTauri()) return invoke("remove_attachment", { id });
  previewAttachments.delete(id);
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
