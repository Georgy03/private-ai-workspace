import { useCallback, useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  Check,
  ChevronRight,
  Database,
  ExternalLink,
  FileSearch,
  Folder,
  FolderPlus,
  House,
  LoaderCircle,
  MessageSquareText,
  PanelRightClose,
  Pause,
  Network,
  Plug,
  Search,
  Settings,
  Trash2,
  X,
  Bot,
  FolderOpen,
  Moon,
  Sun,
} from "lucide-react";
import { ConnectorsPanel } from "./Connectors";
import { AgentsView } from "./Agents";
import { ChatView } from "./Chat";
import { RetentionPrompt } from "./Retention";
import { FileIcon } from "./FileIcon";
import { fileApp } from "./fileApp";
import { SearchView } from "./Search";
import { Home } from "./Home";
import { SpacesView } from "./Spaces";
import { setThemeChoice, shownTheme, themeChoice, type ThemeChoice } from "./theme";
import { SpaceView } from "./SpaceView";
import type { ChatThread } from "./Chat";
import type { AttachedFile, SearchLimits } from "./types";
import { KnowledgeView } from "./Knowledge";
import { Logo } from "./Logo";
import { Shimmer } from "./Shimmer";
import { PrismBackdrop } from "./PrismBackdrop";
import { SignInDialog } from "./SignInDialog";
import {
  cancelIndex,
  chooseFolder,
  cloudExpiring,
  clearLocalData,
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
  setCloudEnabled,
  removeRoot,
  startIndex,
} from "./api";
import type {
  AgentTask,
  Citation,
  CloudStatus,
  DocumentPreview,
  DocumentResource,
  IndexJob,
  Entity,
  Usage,
  WorkspaceSnapshot,
} from "./types";

type View = "ask" | "sources" | "knowledge" | "agents" | "search" | "chat" | "spaces";
type SourcesTab = "files" | "connectors";

const initialCloud: CloudStatus = {
  state: "not_configured",
  message: "",
  configured: false,
  enabled: true,
  signed_in: false,
};

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
  // The chats are kept here so they survive going to another page and back, and so Home can list them.
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const [activeChat, setActiveChat] = useState<number | null>(null);
  // The workspace that is open, if any.
  const [openSpace, setOpenSpace] = useState<string | null>(null);
  // Light, dark, or the computer's own setting.
  const [theme, setTheme] = useState<ThemeChoice>(themeChoice);
  const changeTheme = (choice: ThemeChoice) => { setThemeChoice(choice); setTheme(choice); };
  // A question typed on Home, handed to the chat to ask.
  const [pending, setPending] = useState<{ id: number; text: string; attachments: AttachedFile[]; limits: SearchLimits } | null>(null);
  const [preview, setPreview] = useState<DocumentPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [cloud, setCloud] = useState<CloudStatus>(initialCloud);
  const [usage, setUsage] = useState<Usage | null>(null);

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

  // Old reports about to be deleted: the person is asked whether to keep them, once the cloud is usable.
  const [expiring, setExpiring] = useState<AgentTask[]>([]);
  // A task to show when the Agents page opens: set when someone follows a link from the chat.
  const [agentsOpenTask, setAgentsOpenTask] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    if (!cloudReady) return;
    void cloudExpiring().then((items) => {
      if (!disposed) setExpiring(items);
    }).catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, [cloudReady]);

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

  const removeFolder = async (root: string) => {
    try {
      await removeRoot(root);
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

  const goToSources = (tab: SourcesTab) => {
    setSourcesTab(tab);
    setView("sources");
  };

  if (loading) {
    return (
      <div className="loading-screen">
        <div className="brand-mark"><Logo size={44} /></div>
        <Logo size={40} mark thinking />
        <Shimmer>Opening encrypted workspace…</Shimmer>
      </div>
    );
  }

  return (
    <div className="app-shell">
      <Sidebar
        workspace={workspace}
        usage={shownUsage}
        view={view}
        sourcesTab={sourcesTab}
        onHome={() => setView("ask")}
        onSources={goToSources}
        onKnowledge={() => setView("knowledge")}
        onChat={() => setView("chat")}
        onSpaces={() => { setOpenSpace(null); setView("spaces"); }}
        onAgents={() => { setAgentsOpenTask(null); setView("agents"); }}
        onSearch={() => setView("search")}
        cloudReady={cloud.state === "ready"}
        dark={shownTheme(theme) === "dark"}
        onToggleTheme={() => changeTheme(shownTheme(theme) === "dark" ? "light" : "dark")}
        onOpenSettings={() => setSettingsOpen(true)}
      />

      <main className="main-panel">
        {(view === "ask" || view === "chat") && <PrismBackdrop />}
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
          <Home
            greeting={greeting()}
            cloudReady={cloud.state === "ready"}
            signedIn={cloud.signed_in}
            hasDocuments={workspace.documents.length > 0}
            documentCount={workspace.documents.length}
            usage={shownUsage}
            threads={threads}
            expiring={expiring}
            onAsk={(ask) => { setActiveChat(null); setPending({ id: Date.now(), ...ask }); setView("chat"); }}
            onOpenChat={(id) => { setActiveChat(id); setView("chat"); }}
            onOpenAgents={(taskId) => { setAgentsOpenTask(taskId ?? null); setView("agents"); }}
            onSources={goToSources}
            onAddFolder={addFolder}
            onAddSample={addSample}
          />
        ) : view === "knowledge" ? (
          <KnowledgeView cloud={cloud} onOpenLink={openLink} />
        ) : view === "chat" ? (
          <ChatView
            hasDocuments={workspace.documents.length > 0 || cloud.signed_in}
            documents={workspace.documents}
            cloudReady={cloud.state === "ready"}
            threads={threads}
            setThreads={setThreads}
            activeId={activeChat}
            setActiveId={setActiveChat}
            pending={pending}
            onPendingUsed={() => setPending(null)}
            onCitation={(citation) =>
              citation.document_id.startsWith("web:")
                ? (citation.link ? openLink(citation.link) : undefined)
                : citation.origin === "cloud"
                ? openCloudPassage(citation.document_name, citation.excerpt, citation.location_label, citation.link)
                : citation.document_id.startsWith("attachment:")
                  ? undefined
                  : void openDocument(citation.document_id)
            }
            onOpenLink={openLink}
            onOpenAgents={(taskId) => { setAgentsOpenTask(taskId ?? null); setView("agents"); }}
            onAnswered={(answered) => {
              setCloud(answered.cloud);
              void cloudUsage().then(setUsage).catch(() => undefined);
            }}
          />
        ) : view === "spaces" ? (
          openSpace ? (
            <SpaceView
              key={openSpace}
              id={openSpace}
              cloud={cloud}
              onBack={() => setOpenSpace(null)}
              onGone={() => setOpenSpace(null)}
              onCitation={(citation) =>
                citation.document_id.startsWith("web:")
                  ? (citation.link ? openLink(citation.link) : undefined)
                  : citation.origin === "cloud"
                  ? openCloudPassage(citation.document_name, citation.excerpt, citation.location_label, citation.link)
                  : void openDocument(citation.document_id)
              }
              onOpenLink={openLink}
              onOpenAgents={(taskId) => { setAgentsOpenTask(taskId ?? null); setView("agents"); }}
              onAnswered={(answered) => {
                setCloud(answered.cloud);
                void cloudUsage().then(setUsage).catch(() => undefined);
              }}
            />
          ) : (
            <SpacesView onOpen={setOpenSpace} />
          )
        ) : view === "search" ? (
          <SearchView cloud={cloud} onOpenLink={openLink} />
        ) : view === "agents" ? (
          <AgentsView cloud={cloud} onOpenLink={openLink} openTaskId={agentsOpenTask} />
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
            onRemoveFolder={(root) => void removeFolder(root)}
            indexing={workspace.active_job?.state === "running"}
          />
        )}
      </main>

      <SourceViewer
        preview={preview}
        citations={[]}
        onClose={() => setPreview(null)}
        onOpenLink={openLink}
      />

      {settingsOpen && (
        <SettingsDialog
          cloud={cloud}
          theme={theme}
          onTheme={changeTheme}
          usage={shownUsage}
          onUsage={setUsage}
          onCloud={setCloud}
          onClose={() => setSettingsOpen(false)}
          onSaved={refresh}
          onError={(message) => setError(message)}
        />
      )}

      {expiring.length > 0 && <RetentionPrompt items={expiring} onDone={() => setExpiring([])} />}
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
  onChat,
  onSpaces,
  onSearch,
  cloudReady,
  dark,
  onToggleTheme,
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
  onChat: () => void;
  onSpaces: () => void;
  onSearch: () => void;
  cloudReady: boolean;
  dark: boolean;
  onToggleTheme: () => void;
  onOpenSettings: () => void;
}) {
  const job = workspace.active_job;
  const indexing = job?.state === "running";
  const percent = job && job.discovered ? Math.round(((job.completed + job.failed) / job.discovered) * 100) : 0;
  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-mark"><Logo size={34} /></div>
        <div className="brand-name"><strong>Prism</strong> <span>Workspace</span></div>
      </div>
      <nav>
        <button className={view === "ask" ? "active" : ""} onClick={onHome}>
          <House size={18} /> Home
        </button>
        <button className={view === "chat" ? "active" : ""} onClick={onChat}>
          <MessageSquareText size={18} /> Chat
        </button>
        <button className={view === "spaces" ? "active" : ""} onClick={onSpaces}>
          <FolderOpen size={18} /> Workspaces
        </button>
        <button className={view === "sources" && sourcesTab === "files" ? "active" : ""} onClick={() => onSources("files")}>
          <Database size={18} /> Sources
          <span className="nav-count">{workspace.documents.length}</span>
        </button>
        <button className={view === "sources" && sourcesTab === "connectors" ? "active" : ""} onClick={() => onSources("connectors")}>
          <Plug size={18} /> Connectors
        </button>
        {cloudReady && (
          <button className={view === "search" ? "active" : ""} onClick={onSearch}>
            <Search size={18} /> Search
          </button>
        )}
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
      <div className="sidebar-spacer" />
      <div className="status-card">
        <span className={`status-dot ${indexing ? "busy" : "ok"}`} />
        <div>
          <strong>{indexing ? <Shimmer>Indexing files</Shimmer> : "Local index ready"}</strong>
          <small>{indexing ? `${percent}% done` : `${workspace.documents.length.toLocaleString()} files, encrypted on this device`}</small>
        </div>
      </div>
      {workspace.provider.state !== "ready" && <p className="provider-note" title={workspace.provider.message}>Search-only mode: the AI model is not available.</p>}
      {usage && <UsageMeter usage={usage} />}
      <button className="settings-button" aria-pressed={dark} onClick={onToggleTheme}>{dark ? <Sun size={18} /> : <Moon size={18} />} {dark ? "Light mode" : "Dark mode"}</button>
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

function IndexProgress({ job, stopping, onCancel }: { job: IndexJob; stopping: boolean; onCancel: () => void }) {
  const progress = job.discovered ? Math.round(((job.completed + job.failed) / job.discovered) * 100) : 0;
  const stopped = job.state === "cancelled";
  return (
    <div className="index-progress">
      <div className="progress-track"><div style={{ width: `${progress}%` }} /></div>
      {stopped ? <Check size={15} /> : <LoaderCircle size={15} className="spin" />}
      <span>{stopped ? "Indexing stopped. Completed files remain searchable." : stopping ? <Shimmer>Stopping indexing…</Shimmer> : <Shimmer>{`Indexing ${job.current_file ?? "approved folder"}`}</Shimmer>}</span>
      <small>{job.completed + job.failed} / {job.discovered || "…"}</small>
      {!stopped && <button onClick={onCancel} disabled={stopping}><Pause size={14} /> {stopping ? "Stopping…" : "Stop"}</button>}
    </div>
  );
}

function greeting(now = new Date()): string {
  const hour = now.getHours();
  return hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
}

function SourcesView({ documents, roots, tab, onTab, cloud, onCloud, onOpen, onAddFolder, onRemoveFolder, indexing }: {
  documents: DocumentResource[];
  roots: string[];
  tab: SourcesTab;
  onTab: (tab: SourcesTab) => void;
  cloud: CloudStatus;
  onCloud: (status: CloudStatus) => void;
  onOpen: (document: DocumentResource) => void;
  onAddFolder: () => void;
  onRemoveFolder: (root: string) => void;
  indexing: boolean;
}) {
  const [filter, setFilter] = useState("");
  const [removing, setRemoving] = useState<string | null>(null);
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
      {roots.length > 0 && (
        <ul className="folder-list" aria-label="Approved folders">
          {roots.map((root) => (
            <li key={root} title={root}>
              <Folder size={15} /> <span>{root}</span>
              {removing === root ? (
                <>
                  <small>Stops using this folder and removes its files from the index. The files stay on your computer.</small>
                  <button className="danger" disabled={indexing} onClick={() => { setRemoving(null); onRemoveFolder(root); }}>Remove</button>
                  <button className="secondary" onClick={() => setRemoving(null)}>Cancel</button>
                </>
              ) : (
                <button className="secondary" aria-label={`Remove folder ${root.split(/[\\/]/).pop()}`} disabled={indexing} title={indexing ? "Wait for indexing to finish" : undefined} onClick={() => setRemoving(root)}><Trash2 size={13} /> Remove</button>
              )}
            </li>
          ))}
        </ul>
      )}
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
    if (host.endsWith("dropbox.com")) return "Dropbox";
    if (host.endsWith("docusign.com")) return "DocuSign";
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

function SettingsDialog({ cloud, theme, onTheme, usage, onUsage, onCloud, onClose, onSaved, onError }: {
  cloud: CloudStatus;
  theme: ThemeChoice;
  onTheme: (choice: ThemeChoice) => void;
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

  const [signingIn, setSigningIn] = useState(false);
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
      {signingIn && <SignInDialog onClose={() => setSigningIn(false)} onSignIn={async (choice) => onCloud(await cloudSignIn(choice))} />}
      <div className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <header><div><span className="modal-icon"><Settings size={19} /></span><div><strong>Workspace settings</strong><span>Local search and AI answers</span></div></div><button onClick={onClose}><X size={19} /></button></header>
        <section className="provider-fields cloud-settings"><label id="appearance-label">Appearance</label>
          <div className="theme-choice" role="radiogroup" aria-labelledby="appearance-label">
            {([["system", "System"], ["light", "Light"], ["dark", "Dark"]] as const).map(([value, label]) => (
              <button key={value} type="button" role="radio" aria-checked={theme === value} className={theme === value ? "active" : ""} onClick={() => onTheme(value)}>{label}</button>
            ))}
          </div>
          <p className="field-hint">System follows the setting on this computer.</p>
        </section>
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
              : <button className="secondary" disabled={!cloud.configured || cloudBusy} onClick={() => setSigningIn(true)}>Sign in</button>}
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

function FileTypeIcon({ mime }: { mime: string }) {
  return <span className="result-icon small"><FileIcon app={fileApp("", mime)} size={32} /></span>;
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
