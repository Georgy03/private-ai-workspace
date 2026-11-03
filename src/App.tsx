import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowUp,
  BookOpen,
  CalendarClock,
  Check,
  ChevronRight,
  ClipboardList,
  CircleCheck,
  Cloud,
  Database,
  ExternalLink,
  File,
  FileSearch,
  FileText,
  Folder,
  FolderPlus,
  HardDrive,
  House,
  LoaderCircle,
  LockKeyhole,
  MessageSquareText,
  PanelRightClose,
  Pause,
  Network,
  Plug,
  Plus,
  ReceiptText,
  Search,
  Settings,
  ShieldCheck,
  Sparkles,
  Trash2,
  X,
  Bot,
} from "lucide-react";
import { ConnectorsPanel } from "./Connectors";
import { AgentsView } from "./Agents";
import { KnowledgeView } from "./Knowledge";
import { Logo } from "./Logo";
import {
  answerQuestion,
  cancelIndex,
  chooseFolder,
  clearLocalData,
  cloudConnectors,
  cloudSetUsageLimit,
  cloudSignIn,
  cloudSignOut,
  cloudStatus,
  cloudUsage,
  listEntities,
  openDocumentLink,
  removeEntity,
  saveEntity,
  getDocumentPreview,
  getWorkspace,
  installSample,
  onIndexProgress,
  searchDocuments,
  setCloudEnabled,
  startIndex,
} from "./api";
import type {
  AnswerResponse,
  Citation,
  CloudStatus,
  Connection,
  DocumentPreview,
  DocumentResource,
  IndexJob,
  Entity,
  SearchHit,
  Usage,
  WorkspaceSnapshot,
} from "./types";

type View = "ask" | "sources" | "knowledge" | "agents";
type SourcesTab = "files" | "connectors";

const initialCloud: CloudStatus = {
  state: "not_configured",
  message: "",
  configured: false,
  enabled: true,
  signed_in: false,
};

interface HistoryItem {
  id: number;
  question: string;
  answer: AnswerResponse;
  hits: SearchHit[];
}

const MAX_QUESTION_CHARS = 3000;
const MAX_HISTORY = 20;

const initialWorkspace: WorkspaceSnapshot = {
  roots: [],
  documents: [],
  provider: {
    kind: "bedrock",
    state: "unavailable",
    message: "Loading AI status. Search is ready.",
  },
  model_runtime: {
    runtime_present: false,
    model_present: false,
    model_bytes: 0,
    catalog_verified: false,
    message: "Search-only mode until the AI model is available.",
  },
  sandbox_enforced: false,
};

export default function App() {
  const [workspace, setWorkspace] = useState(initialWorkspace);
  const [view, setView] = useState<View>("ask");
  const [sourcesTab, setSourcesTab] = useState<SourcesTab>("files");
  const [query, setQuery] = useState("");
  const [searchHits, setSearchHits] = useState<SearchHit[]>([]);
  const [answer, setAnswer] = useState<AnswerResponse | null>(null);
  const [asked, setAsked] = useState<string | null>(null);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [activeHistory, setActiveHistory] = useState<number | null>(null);
  const [preview, setPreview] = useState<DocumentPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [receiptOpen, setReceiptOpen] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [cloud, setCloud] = useState<CloudStatus>(initialCloud);
  const [usage, setUsage] = useState<Usage | null>(null);
  const [advanced, setAdvanced] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const snapshot = await getWorkspace();
      setWorkspace(snapshot);
    } catch (caught) {
      setError(toMessage(caught));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    void cloudStatus()
      .then((status) => {
        if (!disposed) setCloud(status);
      })
      .catch(() => undefined);
    void getWorkspace()
      .then((snapshot) => {
        if (!disposed) setWorkspace(snapshot);
      })
      .catch((caught) => {
        if (!disposed) setError(toMessage(caught));
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    let unlisten: (() => void) | undefined;
    void onIndexProgress((job) => {
      setWorkspace((current) => ({ ...current, active_job: job }));
      if (job.state !== "running") {
        setStopping(false);
        void refresh();
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  // The monthly allowance is read when the cloud index becomes usable, and again after each answer.
  const cloudReady = cloud.state === "ready";
  const shownUsage = cloudReady ? usage : null;
  useEffect(() => {
    let disposed = false;
    if (!cloudReady) return;
    void cloudUsage().then((next) => {
      if (!disposed) setUsage(next);
    }).catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, [cloudReady]);

  const ask = async (event: FormEvent, retryText?: string, forceAdvanced?: boolean) => {
    event.preventDefault();
    const text = (retryText ?? query).trim();
    if (!text || asking) return;
    setAsking(true);
    setError(null);
    setAnswer(null);
    setSearchHits([]);
    setActiveHistory(null);
    setAsked(text);
    setQuery("");
    try {
      // One request does both searches. A second search per question would send the question to
      // the firm cloud index twice.
      const nextAnswer = await answerQuestion(text, forceAdvanced ?? advanced);
      const item = { id: Date.now(), question: text, answer: nextAnswer, hits: [] };
      setAnswer(nextAnswer);
      setCloud(nextAnswer.cloud);
      setActiveHistory(item.id);
      setHistory((current) => [item, ...current].slice(0, MAX_HISTORY));
      void cloudUsage().then(setUsage).catch(() => undefined);
    } catch (caught) {
      // The answer failed, but the search may still have something useful to show.
      setSearchHits(await searchDocuments(text).catch(() => []));
      setQuery(text);
      setAsked(null);
      setError(toMessage(caught));
    } finally {
      setAsking(false);
    }
  };

  const newQuestion = () => {
    setAnswer(null);
    setSearchHits([]);
    setAsked(null);
    setQuery("");
    setActiveHistory(null);
    setView("ask");
  };

  const openHistory = (item: HistoryItem) => {
    setAnswer(item.answer);
    setSearchHits(item.hits);
    setAsked(item.question);
    setActiveHistory(item.id);
    setView("ask");
  };

  const addFolder = async () => {
    try {
      const root = await chooseFolder();
      if (!root) return;
      await startIndex(root);
      await refresh();
    } catch (caught) {
      setError(toMessage(caught));
    }
  };

  const stopIndex = async () => {
    setStopping(true);
    try {
      await cancelIndex();
    } catch (caught) {
      setStopping(false);
      setError(toMessage(caught));
    }
  };

  const addSample = async () => {
    try {
      await installSample();
      await refresh();
    } catch (caught) {
      setError(toMessage(caught));
    }
  };

  /** A passage from the firm cloud index has no file on this device, so it is shown on its own. */
  const openCloudPassage = (name: string, text: string, label: string, link?: string | null) =>
    setPreview({
      document: {
        id: `cloud:${name}`,
        name,
        path: label,
        mime_type: "",
        size: 0,
        modified_at: 0,
        content_hash: "",
        status: "ready",
        chunk_count: 1,
      },
      blocks: [{ text, location: { boxes: [] }, ocr: false }],
      stale: false,
      cloud: true,
      link,
    });

  const openLink = (url: string) => {
    openDocumentLink(url).catch((caught) => setError(toMessage(caught)));
  };

  const openDocument = async (documentId: string) => {
    try {
      setPreview(await getDocumentPreview(documentId));
    } catch (caught) {
      setError(toMessage(caught));
    }
  };

  const visibleCitations = useMemo(() => answer?.citations ?? [], [answer]);
  const conversing = asking || answer !== null;

  const goToSources = (tab: SourcesTab) => {
    setSourcesTab(tab);
    setView("sources");
  };

  if (loading) {
    return (
      <div className="loading-screen">
        <div className="brand-mark"><Logo size={44} /></div>
        <LoaderCircle className="spin" size={24} />
        <span>Opening encrypted workspace…</span>
      </div>
    );
  }

  return (
    <div className={`app-shell ${view === "ask" && conversing ? "with-history" : ""}`}>
      <Sidebar
        workspace={workspace}
        usage={shownUsage}
        view={view}
        sourcesTab={sourcesTab}
        onHome={() => { newQuestion(); }}
        onSources={goToSources}
        onKnowledge={() => setView("knowledge")}
        onAgents={() => setView("agents")}
        cloudReady={cloud.state === "ready"}
        onAddFolder={addFolder}
        onAddSample={addSample}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <main className="main-panel">
        <Topbar workspace={workspace} view={view} cloud={cloud} onCloud={() => setSettingsOpen(true)} />
        {(workspace.active_job?.state === "running" || workspace.active_job?.state === "cancelled") && (
          <IndexProgress job={workspace.active_job} stopping={stopping} onCancel={() => void stopIndex()} />
        )}
        {error && (
          <div className="error-banner">
            <AlertTriangle size={17} />
            <span>{error}</span>
            <button onClick={() => setError(null)} aria-label="Dismiss"><X size={16} /></button>
          </div>
        )}

        {view === "ask" ? (
          <AskWorkspace
            query={query}
            onQuery={setQuery}
            onSubmit={ask}
            advanced={advanced}
            onAdvanced={setAdvanced}
            asking={asking}
            question={asked}
            answer={answer}
            searchHits={searchHits}
            hasDocuments={workspace.documents.length > 0}
            documentCount={workspace.documents.length}
            cloud={cloud}
            history={history}
            onOpenHistory={openHistory}
            onSources={goToSources}
            onAddFolder={addFolder}
            onAddSample={addSample}
            onCitation={(citation) =>
              citation.origin === "cloud"
                ? openCloudPassage(citation.document_name, citation.excerpt, citation.location_label, citation.link)
                : void openDocument(citation.document_id)
            }
            onHit={(hit) =>
              hit.origin === "cloud"
                ? openCloudPassage(hit.document_name, hit.excerpt, hit.location_label, hit.link)
                : void openDocument(hit.document_id)
            }
            onReceipt={() => setReceiptOpen(true)}
            onOpenLink={openLink}
          />
        ) : view === "knowledge" ? (
          <KnowledgeView cloud={cloud} onOpenLink={openLink} />
        ) : view === "agents" ? (
          <AgentsView cloud={cloud} onOpenLink={openLink} />
        ) : (
          <SourcesView
            documents={workspace.documents}
            roots={workspace.roots}
            tab={sourcesTab}
            onTab={setSourcesTab}
            cloud={cloud}
            onCloud={setCloud}
            onOpen={(document) => void openDocument(document.id)}
            onAddFolder={addFolder}
          />
        )}
      </main>

      {view === "ask" && conversing && (
        <HistoryPanel
          items={history}
          activeId={activeHistory}
          disabled={asking}
          onNew={newQuestion}
          onOpen={openHistory}
        />
      )}

      <SourceViewer
        preview={preview}
        citations={visibleCitations}
        onClose={() => setPreview(null)}
        onOpenLink={openLink}
      />

      {settingsOpen && (
        <SettingsDialog
          cloud={cloud}
          usage={shownUsage}
          onUsage={setUsage}
          onCloud={setCloud}
          onClose={() => setSettingsOpen(false)}
          onSaved={refresh}
          onError={(message) => setError(message)}
        />
      )}

      {receiptOpen && answer && (
        <ReceiptDialog answer={answer} onClose={() => setReceiptOpen(false)} />
      )}
    </div>
  );
}

function Sidebar({
  workspace,
  usage,
  view,
  sourcesTab,
  onHome,
  onSources,
  onKnowledge,
  onAgents,
  cloudReady,
  onAddFolder,
  onAddSample,
  onOpenSettings,
}: {
  workspace: WorkspaceSnapshot;
  usage: Usage | null;
  view: View;
  sourcesTab: SourcesTab;
  onHome: () => void;
  onSources: (tab: SourcesTab) => void;
  onKnowledge: () => void;
  onAgents: () => void;
  cloudReady: boolean;
  onAddFolder: () => void;
  onAddSample: () => void;
  onOpenSettings: () => void;
}) {
  const job = workspace.active_job;
  const indexing = job?.state === "running";
  const percent = job && job.discovered ? Math.round(((job.completed + job.failed) / job.discovered) * 100) : 0;
  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-mark"><Logo /></div>
        <div><strong>Private AI</strong><span>Connector Workspace</span></div>
      </div>
      <nav>
        <button className={view === "ask" ? "active" : ""} onClick={onHome}>
          <House size={18} /> Home
        </button>
        <button className={view === "sources" && sourcesTab === "files" ? "active" : ""} onClick={() => onSources("files")}>
          <Database size={18} /> Sources
          <span className="nav-count">{workspace.documents.length}</span>
        </button>
        <button className={view === "sources" && sourcesTab === "connectors" ? "active" : ""} onClick={() => onSources("connectors")}>
          <Plug size={18} /> Connectors
        </button>
        {cloudReady && (
          <button className={view === "knowledge" ? "active" : ""} onClick={onKnowledge}>
            <Network size={18} /> Knowledge
          </button>
        )}
        {cloudReady && (
          <button className={view === "agents" ? "active" : ""} onClick={onAgents}>
            <Bot size={18} /> Agents
          </button>
        )}
      </nav>
      <div className="sidebar-section">
        <div className="section-label"><span>Approved folders</span><button onClick={onAddFolder} aria-label="Add folder"><Plus size={14} /></button></div>
        {workspace.roots.length === 0 ? (
          <button className="empty-folder" onClick={onAddFolder}><FolderPlus size={17} /> Add a private folder</button>
        ) : (
          workspace.roots.map((root) => (
            <div className="root-row" key={root} title={root}>
              <Folder size={16} /> <span>{root.split(/[\\/]/).pop()}</span><Check size={13} />
            </div>
          ))
        )}
        <button className="sample-link" onClick={onAddSample}>Try the fictional sample client</button>
      </div>
      <div className="sidebar-spacer" />
      <div className="status-card">
        <span className={`status-dot ${indexing ? "busy" : "ok"}`} />
        <div>
          <strong>{indexing ? "Indexing files" : "Local index ready"}</strong>
          <small>{indexing ? `${percent}% done` : `${workspace.documents.length.toLocaleString()} files, encrypted on this device`}</small>
        </div>
      </div>
      {usage && <UsageMeter usage={usage} />}
      <button className="settings-button" onClick={onOpenSettings}><Settings size={18} /> Settings</button>
    </aside>
  );
}

const dollars = (micros: number) => `$${(micros / 1_000_000).toFixed(2)}`;

/** How much of this month's AI allowance has been used. */
function UsageMeter({ usage }: { usage: Usage }) {
  const share = usage.limit_micros > 0 ? Math.min(1, usage.used_micros / usage.limit_micros) : 1;
  const level = usage.remaining_micros <= 0 ? "full" : share >= 0.8 ? "high" : "ok";
  return (
    <div className={`usage-meter ${level}`} aria-label="AI allowance this month">
      <div className="usage-head"><strong>AI this month</strong><span>{dollars(usage.used_micros)} of {dollars(usage.limit_micros)}</span></div>
      <div className="usage-track"><div style={{ width: `${Math.round(share * 100)}%` }} /></div>
      {usage.remaining_micros <= 0 && <small>Allowance used. Search still works.</small>}
    </div>
  );
}

const CLOUD_CHIP: Record<string, string> = {
  ready: "Cloud index: on",
  signed_out: "Cloud index: sign in",
  off: "Cloud index: off",
  unavailable: "Cloud index: unavailable",
};

function Topbar({ workspace, view, cloud, onCloud }: { workspace: WorkspaceSnapshot; view: View; cloud: CloudStatus; onCloud: () => void }) {
  return (
    <header className="topbar">
      <h2>{view === "ask" ? "Home" : view === "knowledge" ? "Knowledge" : view === "agents" ? "Agents" : "Sources"}</h2>
      <div className="topbar-meta">
        <span>{workspace.documents.length} sources</span>
        <span className="separator" />
        <LockKeyhole size={14} /> Encrypted index
        <span className="separator" />
        {CLOUD_CHIP[cloud.state] && (
          <>
            <button className={`cloud-chip ${cloud.state}`} title={cloud.message} onClick={onCloud}>{CLOUD_CHIP[cloud.state]}</button>
            <span className="separator" />
          </>
        )}
        <span title={workspace.provider.message}>{workspace.provider.state !== "ready" ? "Search-only mode" : workspace.provider.kind === "anthropic" ? "Claude API (test mode)" : "Claude on Bedrock"}</span>
      </div>
    </header>
  );
}

function IndexProgress({ job, stopping, onCancel }: { job: IndexJob; stopping: boolean; onCancel: () => void }) {
  const progress = job.discovered ? Math.round(((job.completed + job.failed) / job.discovered) * 100) : 0;
  const stopped = job.state === "cancelled";
  return (
    <div className="index-progress">
      <div className="progress-track"><div style={{ width: `${progress}%` }} /></div>
      {stopped ? <Check size={15} /> : <LoaderCircle size={15} className="spin" />}
      <span>{stopped ? "Indexing stopped. Completed files remain searchable." : stopping ? "Stopping indexing…" : `Indexing ${job.current_file ?? "approved folder"}`}</span>
      <small>{job.completed + job.failed} / {job.discovered || "…"}</small>
      {!stopped && <button onClick={onCancel} disabled={stopping}><Pause size={14} /> {stopping ? "Stopping…" : "Stop"}</button>}
    </div>
  );
}

const ACTIONS = [
  { label: "What is still missing for this client?", icon: ClipboardList, tone: "amber" },
  { label: "When is payment due?", icon: CalendarClock, tone: "blue" },
  { label: "How do we review a new engagement?", icon: BookOpen, tone: "green" },
  { label: "Summarize the engagement letter", icon: FileText, tone: "pink" },
] as const;

function greeting(now = new Date()): string {
  const hour = now.getHours();
  return hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
}

const SERVICE_NAMES: Record<string, string> = { google_drive: "Google Drive", microsoft365: "Microsoft 365" };

/** What the workspace can read right now: this device, plus each connected cloud source. */
function ConnectedSources({ cloud, documentCount, onSources }: { cloud: CloudStatus; documentCount: number; onSources: (tab: SourcesTab) => void }) {
  const [connections, setConnections] = useState<Connection[]>([]);
  const usable = cloud.configured && cloud.signed_in && cloud.enabled;
  useEffect(() => {
    let disposed = false;
    if (!usable) return;
    void cloudConnectors()
      .then((view) => {
        if (!disposed) setConnections(view.connections);
      })
      .catch(() => {
        if (!disposed) setConnections([]);
      });
    return () => {
      disposed = true;
    };
  }, [usable]);
  const shown = usable ? connections : [];
  return (
    <section className="connected-sources" aria-label="Connected sources">
      <h2>Your connected sources</h2>
      <div className="connected-row">
        <div className="connected-item">
          <span className="connected-icon"><HardDrive size={20} /></span>
          <div><strong>Local files</strong><small>{documentCount.toLocaleString()} files</small></div>
          <CircleCheck size={16} className="ok" />
        </div>
        {shown.map((connection) => (
          <div className="connected-item" key={connection.id}>
            <span className="connected-icon"><Cloud size={20} /></span>
            <div>
              <strong>{SERVICE_NAMES[connection.kind] ?? connection.kind}</strong>
              <small>{connection.documents.toLocaleString()} files</small>
            </div>
            {connection.status === "active" ? <CircleCheck size={16} className="ok" /> : <AlertTriangle size={16} className="warn" />}
          </div>
        ))}
        <button className="add-source" onClick={() => onSources("connectors")}><Plus size={15} /> Add source</button>
      </div>
    </section>
  );
}

const THINKING_STEPS = [
  "Searching your local index…",
  "Sending the matching text to Claude…",
  "Claude is writing your answer…",
];

function ThinkingIndicator() {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    const timer = window.setInterval(() => setSeconds((current) => current + 1), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const step = Math.min(Math.floor(seconds / 3), THINKING_STEPS.length - 1);
  return (
    <div className="assistant-row thinking" role="status" aria-live="polite">
      <div className="assistant-avatar"><Sparkles size={16} /></div>
      <div className="thinking-body">
        <div className="thinking-head">
          <span className="typing-dots" aria-hidden="true"><i /><i /><i /></span>
          <strong key={step}>{THINKING_STEPS[step]}</strong>
          <small>{seconds}s</small>
        </div>
        <div className="thinking-skeleton" aria-hidden="true"><span /><span /><span /></div>
        {seconds >= 20 && <p className="thinking-note">Long documents can take a minute or two.</p>}
      </div>
    </div>
  );
}

function AskWorkspace({
  query,
  onQuery,
  onSubmit,
  advanced,
  onAdvanced,
  asking,
  question,
  answer,
  searchHits,
  hasDocuments,
  documentCount,
  cloud,
  history,
  onOpenHistory,
  onSources,
  onAddFolder,
  onAddSample,
  onCitation,
  onHit,
  onReceipt,
  onOpenLink,
}: {
  query: string;
  onQuery: (value: string) => void;
  onSubmit: (event: FormEvent, retryText?: string, forceAdvanced?: boolean) => void;
  advanced: boolean;
  onAdvanced: (on: boolean) => void;
  asking: boolean;
  question: string | null;
  answer: AnswerResponse | null;
  searchHits: SearchHit[];
  hasDocuments: boolean;
  documentCount: number;
  cloud: CloudStatus;
  history: HistoryItem[];
  onOpenHistory: (item: HistoryItem) => void;
  onSources: (tab: SourcesTab) => void;
  onAddFolder: () => void;
  onAddSample: () => void;
  onCitation: (citation: Citation) => void;
  onHit: (hit: SearchHit) => void;
  onReceipt: () => void;
  onOpenLink: (url: string) => void;
}) {
  const conversing = asking || answer !== null;
  const input = useRef<HTMLTextAreaElement>(null);
  const focusInput = (text?: string) => {
    if (text !== undefined) onQuery(text);
    window.setTimeout(() => input.current?.focus(), 0);
  };
  const composer = (
    <form className={`composer ${asking ? "busy" : ""}`} onSubmit={onSubmit}>
      <div className="composer-card">
        <div className="composer-row">
          <textarea
            ref={input}
            value={query}
            rows={conversing ? 2 : 3}
            maxLength={MAX_QUESTION_CHARS}
            onChange={(event) => onQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }}
            placeholder={hasDocuments ? "Ask anything about your work…" : "Add a folder or try the sample client…"}
            disabled={!hasDocuments || asking}
            aria-label="Ask your workspace"
          />
          <button disabled={!query.trim() || asking || !hasDocuments} aria-label="Search workspace">
            {asking ? <LoaderCircle className="spin" size={17} /> : <ArrowUp size={18} />}
          </button>
        </div>
        <div className="composer-foot">
          <label className={`advanced-toggle ${advanced ? "on" : ""}`} title="Uses Claude Sonnet, which is more thorough and costs about twenty times as much per answer as the everyday model.">
            <input type="checkbox" checked={advanced} disabled={asking} onChange={(event) => onAdvanced(event.target.checked)} />
            <Sparkles size={13} /> Advanced analysis
          </label>
          <span>{query.length.toLocaleString()} / {MAX_QUESTION_CHARS.toLocaleString()}</span>
        </div>
        {advanced && (
          <p className="advanced-note">
            <AlertTriangle size={13} /> Advanced analysis uses a stronger model that reads more carefully. It uses about 20 times as much of your monthly AI allowance per answer. Turn it off for everyday questions.
          </p>
        )}
      </div>
    </form>
  );
  return (
    <div className="ask-workspace">
      <div className="chat-scroll">
        <div className={`chat-inner ${conversing ? "conversation" : ""}`}>
          {!conversing && (
            <section className="welcome home">
              <h1>{greeting()}</h1>
              <p>Ask a question and get answers with a clear path back to every source.</p>
              <div className="home-composer">
                {composer}
                <div className="source-chips" aria-label="Where answers come from">
                  <button type="button" onClick={() => onSources("files")}><HardDrive size={14} /> Files <em>{documentCount.toLocaleString()}</em></button>
                  <button type="button" onClick={() => onSources("connectors")}>
                    <Cloud size={14} /> {cloud.state === "ready" ? "Firm cloud index" : cloud.state === "off" ? "Firm cloud index (off)" : "Add a cloud index"}
                  </button>
                </div>
              </div>
              {hasDocuments && (
                <div className="suggestion-pills">
                  {ACTIONS.map(({ label, icon: Icon }) => (
                    <button className="pill" key={label} onClick={() => focusInput(label)}>
                      <Icon size={15} /> {label}
                    </button>
                  ))}
                </div>
              )}
              <div className="home-grid">
                <section className="home-card" aria-label="Recent activity">
                  <header><h2>Recent activity</h2></header>
                  {history.length === 0 ? (
                    <p className="home-empty">Questions you ask appear here. They are kept only until you close the app.</p>
                  ) : (
                    <ul className="activity-list">
                      {history.slice(0, 4).map((item) => (
                        <li key={item.id}>
                          <button onClick={() => onOpenHistory(item)}>
                            <span className="activity-icon"><MessageSquareText size={16} /></span>
                            <span className="activity-text">
                              <strong>{item.question}</strong>
                              <small>{item.answer.abstained ? "No supported answer found" : `Answered · ${item.answer.citations.length} source${item.answer.citations.length === 1 ? "" : "s"}`}</small>
                            </span>
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>
                <section className="home-card" aria-label="Quick actions">
                  <header><h2>Quick actions</h2></header>
                  <div className="quick-grid">
                    <button onClick={() => focusInput()} disabled={!hasDocuments}>
                      <Search size={18} /><strong>Search your knowledge</strong><small>Find answers across your files and the firm cloud</small>
                    </button>
                    <button onClick={() => focusInput("Summarize ")} disabled={!hasDocuments}>
                      <FileText size={18} /><strong>Summarize a file</strong><small>Name a document to get its key points</small>
                    </button>
                    <button onClick={onAddFolder}>
                      <FolderPlus size={18} /><strong>Add a folder</strong><small>Choose more files to search on this device</small>
                    </button>
                    <button onClick={() => onSources("connectors")}>
                      <Plug size={18} /><strong>Connect a source</strong><small>Google Drive or Microsoft 365</small>
                    </button>
                  </div>
                </section>
              </div>
              <ConnectedSources cloud={cloud} documentCount={documentCount} onSources={onSources} />
            </section>
          )}

          {!hasDocuments && (
            <div className="onboarding-card">
              <div className="onboarding-icon"><FolderPlus size={27} /></div>
              <div><h2>Choose what Private AI can search</h2><p>Only folders you approve are indexed. The index stays on this device; text matching your questions is sent to Claude.</p></div>
              <button className="primary" onClick={onAddFolder}>Choose a folder</button>
              <button className="secondary" onClick={onAddSample}>Use sample files</button>
            </div>
          )}

          {conversing && question && <div className="user-row"><div className="user-bubble">{question}</div></div>}

          {asking && <ThinkingIndicator />}

          {answer && (
            <div className="assistant-row">
              <div className="assistant-avatar"><Sparkles size={16} /></div>
              <section className="answer-area">
                <div className="answer-heading">
                  <div><span>{answer.abstained ? "No supported answer found" : "Answer from your workspace"}</span></div>
                  <button onClick={onReceipt}><ReceiptText size={15} /> Privacy receipt</button>
                </div>
                {answer.notice && <p className="cloud-note"><AlertTriangle size={14} /> {answer.notice}</p>}
                {(answer.cloud.state === "unavailable" || answer.cloud.state === "signed_out") && (
                  <p className="cloud-note"><AlertTriangle size={14} /> {answer.cloud.message}</p>
                )}
                {answer.abstained ? (
<>                  <p className="abstention">{answer.dropped_claims > 0 ? "The AI response was not shown because it did not cite the indexed sources in the required format. The closest passages are below." : "I couldn’t find enough matching information to answer safely. Try a specific file name, client, clause, or identifier."}</p>
                    {!advanced && question && (
                      <button className="retry-advanced" onClick={() => onSubmit({ preventDefault: () => undefined } as unknown as FormEvent, question, true)}>
                        <Sparkles size={14} /> Try again with advanced analysis <small>uses about 20× more of your allowance</small>
                      </button>
                    )}</>
                ) : (
                  <div className="claims">
                    {answer.claims.map((claim, index) => (
                      <AnswerText key={`${claim.text}-${index}`} text={claim.text} />
                    ))}
                  </div>
                )}
                {answer.dropped_claims > 0 && <p className="abstention">{answer.dropped_claims} statement{answer.dropped_claims === 1 ? "" : "s"} removed because the citation or quote could not be verified.</p>}
                {answer.citations.length > 0 && (
                  <section className="answer-sources" aria-label="Documents used for this answer">
                    <h2>Documents used for this answer</h2>
                    <div className="citation-grid">
                      {answer.citations.map((citation, index) => (
                        <div className="citation-card" key={citation.id}>
                          <button onClick={() => onCitation(citation)}>
                            <span className="citation-index">{index + 1}</span>
                            <div><strong>{citation.document_name}{citation.origin === "cloud" && <em className="origin-badge">Cloud</em>}</strong><span>{citation.location_label}</span></div>
                            <ChevronRight size={16} />
                          </button>
                          {citation.link && <OpenLink link={citation.link} name={citation.document_name} onOpen={onOpenLink} />}
                        </div>
                      ))}
                    </div>
                  </section>
                )}
              </section>
            </div>
          )}

          {!answer && searchHits.length > 0 && (
            <section className="search-results">
              <div className="result-title"><span>Closest passages</span><small>{searchHits.length} results</small></div>
              {searchHits.slice(0, 8).map((hit) => (
                <div className="result-item" key={hit.citation_id}>
                  <button className="result-row" onClick={() => onHit(hit)}>
                    <FileTypeIcon mime={hit.path} />
                    <div><strong>{hit.document_name}{hit.origin === "cloud" && <em className="origin-badge">Cloud</em>}</strong><p>{hit.excerpt}</p><span>{hit.location_label}{hit.ocr ? " · OCR" : ""}</span></div>
                    <ChevronRight size={17} />
                  </button>
                  {hit.link && <OpenLink link={hit.link} name={hit.document_name} onOpen={onOpenLink} />}
                </div>
              ))}
            </section>
          )}
        </div>
      </div>

      {conversing && (
        <div className="composer-dock">
          {composer}
          <p className="dock-note">Answers can be wrong. Check the cited sources before relying on them.</p>
        </div>
      )}
    </div>
  );
}

function HistoryPanel({ items, activeId, disabled, onNew, onOpen }: {
  items: HistoryItem[];
  activeId: number | null;
  disabled: boolean;
  onNew: () => void;
  onOpen: (item: HistoryItem) => void;
}) {
  return (
    <aside className="history-panel" aria-label="Recent questions">
      <div className="history-title"><strong>Recent questions</strong><span>{items.length}</span></div>
      <button className="history-card new" onClick={onNew} disabled={disabled}><Plus size={15} /> New question</button>
      {items.map((item) => (
        <button
          className={`history-card ${item.id === activeId ? "active" : ""}`}
          key={item.id}
          onClick={() => onOpen(item)}
          disabled={disabled}
        >
          <strong>{item.question}</strong>
          <span>{item.answer.abstained ? "No supported answer found" : (item.answer.claims[0]?.text ?? "").slice(0, 110)}</span>
        </button>
      ))}
      {items.length === 0 && <p className="history-empty">Questions you ask appear here. They are kept only until you close the app.</p>}
    </aside>
  );
}

function AnswerText({ text }: { text: string }) {
  const blocks = text
    .replace(/\r\n/g, "\n")
    .split(/\n\s*\n/)
    .map((block) => block.trim().replace(/^#{1,6}\s+/, ""))
    .filter(Boolean);

  return (
    <article className="answer-copy">
      {blocks.map((block, index) => {
        const heading = block.match(/^([A-Z][A-Z\s&/-]{2,}):?\s+(.+)$/);
        const standaloneHeading = /^(overview|key details|background|what happened|why it matters|next steps)$/i.test(block);
        if (standaloneHeading) {
          return <h2 key={`${block}-${index}`}>{block}</h2>;
        }
        if (heading) {
          return (
            <section key={`${block}-${index}`}>
              <h2>{toHeading(heading[1])}</h2>
              <p>{heading[2]}</p>
            </section>
          );
        }
        return <p key={`${block}-${index}`}>{block}</p>;
      })}
    </article>
  );
}

function toHeading(value: string) {
  return value
    .toLowerCase()
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function SourcesView({ documents, roots, tab, onTab, cloud, onCloud, onOpen, onAddFolder }: {
  documents: DocumentResource[];
  roots: string[];
  tab: SourcesTab;
  onTab: (tab: SourcesTab) => void;
  cloud: CloudStatus;
  onCloud: (status: CloudStatus) => void;
  onOpen: (document: DocumentResource) => void;
  onAddFolder: () => void;
}) {
  const [filter, setFilter] = useState("");
  const [shown, setShown] = useState(100);
  // Thousands of rows in the DOM at once freeze the window, so list a page at a time.
  const matching = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return needle ? documents.filter((document) => `${document.name} ${document.path}`.toLowerCase().includes(needle)) : documents;
  }, [documents, filter]);
  return (
    <div className="sources-view">
      <div className="page-heading"><div><span className="eyebrow">{tab === "files" ? "Local knowledge" : "Firm cloud index"}</span><h1>Sources</h1><p>{tab === "files" ? `${roots.length} approved folders · ${documents.length} indexed files` : "Accounts your firm has connected, and the connectors you can add"}</p></div>{tab === "files" && <button className="primary" onClick={onAddFolder}><FolderPlus size={16} /> Add folder</button>}</div>
      <div className="source-tabs" role="tablist" aria-label="Source types">
        <button role="tab" aria-selected={tab === "files"} className={tab === "files" ? "active" : ""} onClick={() => onTab("files")}>Files on this device</button>
        <button role="tab" aria-selected={tab === "connectors"} className={tab === "connectors" ? "active" : ""} onClick={() => onTab("connectors")}>Connectors</button>
      </div>
      {tab === "connectors" ? <ConnectorsPanel cloud={cloud} onCloud={onCloud} /> : <>
      <input className="source-filter" aria-label="Filter sources" value={filter} onChange={(event) => { setFilter(event.target.value); setShown(100); }} placeholder="Filter by file name or folder" />
      <div className="source-table">
        <div className="table-header"><span>Name</span><span>Status</span><span>Passages</span><span>Modified</span><span /></div>
        {matching.slice(0, shown).map((document) => (
          <button className="table-row" key={document.id} onClick={() => onOpen(document)}>
            <span className="file-cell"><FileTypeIcon mime={document.name} /><span><strong>{document.name}</strong><small>{document.path}</small></span></span>
            <span><StatusBadge status={document.status} /></span>
            <span>{document.chunk_count}</span>
            <span>{formatDate(document.modified_at)}</span>
            <span><ChevronRight size={16} /></span>
          </button>
        ))}
        {matching.length > shown && <button className="secondary show-more" onClick={() => setShown((count) => count + 100)}>Show 100 more ({matching.length - shown} hidden)</button>}
        {matching.length === 0 && <div className="table-empty"><FileSearch size={28} /><span>{documents.length === 0 ? "No indexed sources yet." : "No files match this filter."}</span></div>}
      </div>
      </>}
    </div>
  );
}

/** Where a link leads, in words a person recognises. */
function serviceName(link: string): string {
  try {
    const host = new URL(link).hostname;
    return host.endsWith("google.com") ? "Google Drive" : "Microsoft 365";
  } catch {
    return "the original";
  }
}

function OpenLink({ link, name, onOpen }: { link: string; name: string; onOpen: (url: string) => void }) {
  return (
    <button className="open-link" onClick={() => onOpen(link)} aria-label={`Open ${name} in ${serviceName(link)}`}>
      <ExternalLink size={12} /> Open in {serviceName(link)}
    </button>
  );
}

function SourceViewer({ preview, citations, onClose, onOpenLink }: { preview: DocumentPreview | null; citations: Citation[]; onClose: () => void; onOpenLink: (url: string) => void }) {
  if (!preview) return null;
  const cited = new Set(citations.filter((citation) => citation.document_id === preview.document.id).map((citation) => citation.excerpt));
  return (
    <aside className="source-viewer">
      <header><div><span>Source</span><strong>{preview.document.name}</strong></div><button onClick={onClose}><PanelRightClose size={19} /></button></header>
      {preview.stale && <div className="stale-warning"><AlertTriangle size={15} /> This file changed after indexing. Reindex before relying on it.</div>}
      {preview.document.warning && <div className="source-warning"><AlertTriangle size={15} /> {preview.document.warning}</div>}
      {preview.link && <div className="source-open"><OpenLink link={preview.link} name={preview.document.name} onOpen={onOpenLink} /></div>}
      <div className="source-meta">{preview.cloud ? <span>Passage from the firm cloud index</span> : <><span>{preview.document.mime_type}</span><span>{formatBytes(preview.document.size)}</span></>}</div>
      <div className="document-content">
        {preview.blocks.map((block, index) => (
          <article className={cited.has(block.text) ? "highlighted" : ""} key={`${index}-${block.text.slice(0, 12)}`}>
            <div className="block-location">
              {block.location.page ? `Page ${block.location.page}` : block.location.heading ?? `${block.location.sheet ?? "Document"} ${block.location.cell_range ?? ""}`}
              {block.ocr && <span>OCR</span>}
            </div>
            <p>{block.text}</p>
          </article>
        ))}
        {preview.blocks.length === 0 && <div className="no-preview">No readable passages were extracted from this file.</div>}
      </div>
    </aside>
  );
}

function SettingsDialog({ cloud, usage, onUsage, onCloud, onClose, onSaved, onError }: {
  cloud: CloudStatus;
  usage: Usage | null;
  onUsage: (usage: Usage) => void;
  onCloud: (status: CloudStatus) => void;
  onClose: () => void;
  onSaved: () => Promise<void>;
  onError: (message: string) => void;
}) {
  const [entities, setEntities] = useState<Entity[]>([]);
  const [entityName, setEntityName] = useState("");
  const [entityAliases, setEntityAliases] = useState("");
  const [cloudBusy, setCloudBusy] = useState(false);
  const [limitText, setLimitText] = useState("");

  const saveLimit = async (cents: number | null) => {
    try {
      onUsage(await cloudSetUsageLimit(cents));
      setLimitText("");
    } catch (caught) {
      onError(toMessage(caught));
    }
  };
  const limitCents = Math.round(Number(limitText) * 100);
  const limitValid = limitText.trim() !== "" && Number.isFinite(limitCents) && limitCents >= 0 && limitCents <= 10_000_000;

  const changeCloud = async (action: () => Promise<CloudStatus>) => {
    setCloudBusy(true);
    try {
      onCloud(await action());
    } catch (caught) {
      onError(toMessage(caught));
    } finally {
      setCloudBusy(false);
    }
  };

  useEffect(() => {
    listEntities().then(setEntities).catch((caught) => onError(toMessage(caught)));
  }, [onError]);

  const addEntity = async () => {
    try {
      const aliases = entityAliases.split(",").map((alias) => alias.trim()).filter(Boolean);
      const saved = await saveEntity(entityName, aliases);
      setEntities((current) => [...current.filter((item) => item.id !== saved.id), saved].sort((a, b) => a.name.localeCompare(b.name)));
      setEntityName("");
      setEntityAliases("");
    } catch (caught) {
      onError(toMessage(caught));
    }
  };

  const dropEntity = async (id: string) => {
    try {
      await removeEntity(id);
      setEntities((current) => current.filter((item) => item.id !== id));
    } catch (caught) {
      onError(toMessage(caught));
    }
  };

  const clear = async () => {
    if (!window.confirm("Remove every local index entry and approved folder? Source files will not be deleted.")) return;
    try {
      await clearLocalData();
      await onSaved();
      onClose();
    } catch (caught) {
      onError(toMessage(caught));
    }
  };

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <header><div><span className="modal-icon"><Settings size={19} /></span><div><strong>Workspace settings</strong><span>Local search and AI answers</span></div></div><button onClick={onClose}><X size={19} /></button></header>
        <section className="provider-fields cloud-settings"><label>Firm cloud index</label>
          <p className="field-hint">{cloud.configured ? cloud.message : "Not set up on this device. Your firm's administrator provides the server address."}</p>
          <p className="field-hint">Searching it sends your question text to your firm's cloud service. Your local files are never uploaded to it.</p>
          <div className="cloud-actions">
            <label className="toggle">
              <input type="checkbox" checked={cloud.enabled} disabled={!cloud.configured || cloudBusy} onChange={(event) => void changeCloud(() => setCloudEnabled(event.target.checked))} />
              Search the firm cloud index
            </label>
            {cloud.signed_in
              ? <button className="secondary" disabled={cloudBusy} onClick={() => void changeCloud(cloudSignOut)}>Sign out</button>
              : <button className="secondary" disabled={!cloud.configured || cloudBusy} onClick={() => void changeCloud(cloudSignIn)}>{cloudBusy ? "Waiting for sign-in…" : "Sign in"}</button>}
          </div>
        </section>
        {usage && (
          <section className="provider-fields cloud-settings"><label>AI allowance</label>
            <p className="field-hint">
              You have used {dollars(usage.used_micros)} of {dollars(usage.limit_micros)} of AI answers this month ({usage.answers} {usage.answers === 1 ? "answer" : "answers"})
              {usage.limit_is_default ? ", the standard limit" : ""}. It starts again on {new Date(usage.period_end_epoch * 1000).toLocaleDateString(undefined, { month: "long", day: "numeric" })}. Searching never uses it.
            </p>
            <div className="cloud-actions">
              <input type="number" min="0" step="1" aria-label="Monthly AI limit per person in dollars" placeholder="Dollars per person per month" value={limitText} onChange={(event) => setLimitText(event.target.value)} />
              <button className="secondary" disabled={!limitValid} onClick={() => void saveLimit(limitCents)}>Set limit</button>
              {!usage.limit_is_default && <button className="secondary" onClick={() => void saveLimit(null)}>Use standard limit</button>}
            </div>
            <p className="field-hint">Only a firm administrator can change the limit. It applies to everyone in the firm.</p>
          </section>
        )}
        <section className="provider-fields"><label>Names to help local search</label>
          <p className="field-hint">Clients, spouses, entities and matters improve matching across folders. This list stays on this device, but names that appear in your questions or documents are sent to Claude with them.</p>
          {entities.map((entity) => <div className="entity-row" key={entity.id}><span><strong>{entity.name}</strong>{entity.aliases.length > 0 && <small> · {entity.aliases.join(", ")}</small>}</span><button aria-label={`Remove ${entity.name}`} onClick={() => dropEntity(entity.id)}><X size={14} /></button></div>)}
          <div className="entity-add"><input aria-label="Entity name" value={entityName} onChange={(event) => setEntityName(event.target.value)} placeholder="John Turnip" /><input aria-label="Aliases" value={entityAliases} onChange={(event) => setEntityAliases(event.target.value)} placeholder="Aliases, comma separated" /><button className="secondary" onClick={addEntity} disabled={entityName.trim().length < 2}>Add</button></div>
        </section>
        <section className="danger-zone"><div><strong>Clear local workspace</strong><span>Deletes the encrypted index and receipts, not source files.</span></div><button onClick={clear}><Trash2 size={15} /> Clear</button></section>
        <footer><button className="primary" onClick={onClose}>Done</button></footer>
      </div>
    </div>
  );
}

function ReceiptDialog({ answer, onClose }: { answer: AnswerResponse; onClose: () => void }) {
  const receipt = answer.receipt;
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal receipt-modal" onMouseDown={(event) => event.stopPropagation()}>
        <header><div><span className="modal-icon verified"><ReceiptText size={19} /></span><div><strong>Privacy receipt</strong><span>What happened for this answer</span></div></div><button onClick={onClose}><X size={19} /></button></header>
        <div className="receipt-status"><ShieldCheck size={24} /><div><strong>{!receipt.model ? (receipt.cloud_question_sent ? "Your question was sent to the firm cloud index" : "Nothing left this device") : receipt.provider === "anthropic" ? "Sent to the Claude API (test mode)" : "Sent to Amazon Bedrock"}</strong><span>{receipt.summary}</span></div></div>
        <dl><div><dt>Provider</dt><dd>{!receipt.model ? "None" : receipt.provider === "anthropic" ? "Claude API, no zero data retention" : "Claude on Amazon Bedrock"}</dd></div><div><dt>Model</dt><dd>{receipt.model ?? "No model"}</dd></div><div><dt>Local passages</dt><dd>{receipt.passages_local}</dd></div><div><dt>Cloud passages</dt><dd>{receipt.passages_cloud}</dd></div><div><dt>Question sent to cloud index</dt><dd>{receipt.cloud_question_sent ? "Yes" : "No"}</dd></div><div><dt>Time</dt><dd>{new Date(receipt.created_at * 1000).toLocaleString()}</dd></div><div><dt>Receipt ID</dt><dd className="mono">{receipt.id}</dd></div></dl>
        <footer><button className="primary" onClick={onClose}>Done</button></footer>
      </div>
    </div>
  );
}

function FileTypeIcon({ mime }: { mime: string }) {
  const extension = mime.split(".").pop()?.toUpperCase();
  return <span className={`file-icon ext-${extension?.toLowerCase()}`}><File size={18} /><small>{extension?.slice(0, 4)}</small></span>;
}

function StatusBadge({ status }: { status: DocumentResource["status"] }) {
  return <span className={`status-badge ${status}`}>{status === "ready" ? <Check size={12} /> : status === "warning" ? <AlertTriangle size={12} /> : <LoaderCircle size={12} />}{status}</span>;
}

function formatDate(timestamp: number) {
  return timestamp ? new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(new Date(timestamp * 1000)) : "—";
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function toMessage(error: unknown) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something went wrong. No source files were changed.";
}
