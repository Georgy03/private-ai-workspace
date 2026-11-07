import { useEffect, useState, type FormEvent } from "react";
import { AlertTriangle, ArrowUp, ArrowUpRight, Bell, Bot, CalendarClock, Check, CheckCheck, ChevronDown, LoaderCircle, ClipboardCheck, ClipboardList, Folder, FolderPlus, Layers, Mail, MessageSquareText, Paperclip, Plug, BarChart3, Trash2, X } from "lucide-react";
import { attachFile, cloudActions, cloudAgents, cloudConnectors, cloudInbox, cloudTasks, removeAttachment } from "./api";
import type { ChatThread } from "./Chat";
import { limitsLabel, NO_LIMITS } from "./limits";
import { ScopePanel } from "./ScopePanel";
import type { Agent, AgentTask, AttachedFile, Connection, PreparedItem, SearchLimits, Usage } from "./types";

type SourcesTab = "files" | "connectors";
const MAX_ATTACHMENTS = 3;

interface Overview {
  reports: AgentTask[];
  prepared: PreparedItem[];
  recent: AgentTask[];
  helpers: Agent[];
  connections: Connection[];
}

const EMPTY: Overview = { reports: [], prepared: [], recent: [], helpers: [], connections: [] };
const dollars = (micros: number) => "$" + (micros / 1_000_000).toFixed(2);
const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

function ago(epochMs: number): string {
  const minutes = Math.max(0, Math.round((Date.now() - epochMs) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return minutes + " min ago";
  const hours = Math.round(minutes / 60);
  return hours < 24 ? hours + " h ago" : Math.round(hours / 24) + " d ago";
}

const daysLeft = (epoch: number | null | undefined) => (epoch ? Math.max(1, Math.ceil((epoch * 1000 - Date.now()) / 86_400_000)) : 1);

function soft<T>(promise: Promise<T>, fallback: T): Promise<T> {
  return promise.catch(() => fallback);
}

/** What a question typed on Home carries into the chat. */
export interface HomeAsk {
  text: string;
  attachments: AttachedFile[];
  limits: SearchLimits;
}

/**
 * The Home page: one place to ask, what needs the person, and where they were. It does not answer
 * questions itself: a question typed here opens in Chat.
 */
export function Home({ greeting, cloudReady, signedIn, hasDocuments, documentCount, usage, threads, expiring, onAsk, onOpenChat, onOpenAgents, onSources, onAddFolder, onAddSample }: {
  greeting: string;
  cloudReady: boolean;
  /** Connected to the firm cloud, even if it cannot be reached right now. */
  signedIn: boolean;
  hasDocuments: boolean;
  documentCount: number;
  usage: Usage | null;
  threads: ChatThread[];
  expiring: AgentTask[];
  onAsk: (ask: HomeAsk) => void;
  onOpenChat: (id: number) => void;
  onOpenAgents: (taskId?: string) => void;
  onSources: (tab: SourcesTab) => void;
  onAddFolder: () => void;
  onAddSample: () => void;
}) {
  const [text, setText] = useState("");
  const [helper, setHelper] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<AttachedFile[]>([]);
  const [limits, setLimits] = useState<SearchLimits>(NO_LIMITS);
  const [menuOpen, setMenuOpen] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [attaching, setAttaching] = useState(false);
  const [overview, setOverview] = useState<Overview>(EMPTY);

  // What is waiting, read when the cloud is usable. Each part is optional: one that cannot be read
  // leaves the rest of the page as it is.
  useEffect(() => {
    let alive = true;
    if (!cloudReady) return;
    void Promise.all([
      soft(cloudInbox(), [] as AgentTask[]),
      soft(cloudActions(), [] as PreparedItem[]),
      soft(cloudTasks(), [] as AgentTask[]),
      soft(cloudAgents().then((list) => list.agents.filter((agent) => agent.status === "available")), [] as Agent[]),
      soft(cloudConnectors().then((view) => view.connections), [] as Connection[]),
    ]).then(([reports, prepared, tasks, helpers, connections]) => {
      if (!alive) return;
      const recent = tasks.filter((task) => task.status === "done").sort((a, b) => (b.finished_epoch ?? 0) - (a.finished_epoch ?? 0)).slice(0, 4);
      setOverview({ reports, prepared, recent, helpers, connections });
    });
    return () => { alive = false; };
  }, [cloudReady]);

  const shown = cloudReady ? overview : EMPTY;
  const canAsk = hasDocuments || signedIn;
  const chosen = shown.helpers.find((agent) => agent.id === helper) ?? null;
  const firstRun = !hasDocuments && !signedIn;
  const reconnect = shown.connections.filter((connection) => connection.status === "needs_reauth");
  const attention = shown.reports.length + shown.prepared.length + expiring.length + reconnect.length;
  const chats = [...threads].sort((a, b) => b.startedAt - a.startedAt).slice(0, 4);
  const scopeLabel = limitsLabel(limits);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const question = text.trim();
    if (question.length < 3 || !canAsk) return;
    onAsk({ text: chosen ? "@" + chosen.name + " " + question : question, attachments, limits });
    setText("");
    setAttachments([]);
  };

  const attach = () => {
    setProblem(null);
    setAttaching(true);
    attachFile().then(
      (file) => { setAttaching(false); if (file) setAttachments((all) => [...all, file]); },
      (caught) => { setAttaching(false); setProblem(message(caught)); },
    );
  };
  const detach = (id: string) => {
    setAttachments((all) => all.filter((file) => file.id !== id));
    void removeAttachment(id).catch(() => undefined);
  };

  return (
    <div className="home-scroll">
      <section className="home-dash">
        <h1 className="home-greeting">{greeting}</h1>

        {firstRun && (
          <section className="home-start" aria-label="Get started">
            <h2>Get started</h2>
            <ol>
              <li><b>1</b><span><strong>Connect a source</strong><small>Google Drive, Microsoft 365, Dropbox, QuickBooks and more</small></span><button className="secondary" onClick={() => onSources("connectors")}><Plug size={14} /> Connect</button></li>
              <li><b>2</b><span><strong>Or add files from this computer</strong><small>Only folders you choose are indexed, and they stay on this device</small></span><button className="secondary" onClick={onAddFolder}><FolderPlus size={14} /> Choose a folder</button><button className="secondary" onClick={onAddSample}>Use sample files</button></li>
              <li><b>3</b><span><strong>Ask your first question</strong><small>Answers cite the files they came from</small></span></li>
            </ol>
          </section>
        )}

        <form className="home-ask" onSubmit={submit}>
          <textarea
            aria-label="Ask your workspace"
            rows={3}
            value={text}
            disabled={!canAsk}
            placeholder={!canAsk ? "Connect a source or add a folder to start asking…" : chosen ? "Give " + chosen.name + " a task…" : "Ask anything about your work…"}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }}
          />
          {(attachments.length > 0 || attaching) && (
            <ul className="home-files" aria-label="Included with your question">
              {attachments.map((file) => (
                <li key={file.id}><Check size={12} /> <em>Attached</em> {file.name}<button type="button" aria-label={"Remove " + file.name} onClick={() => detach(file.id)}><X size={12} /></button></li>
              ))}
              {attaching && <li className="attaching" role="status"><LoaderCircle size={12} className="spin" /> Attaching your file…</li>}
            </ul>
          )}
          {problem && <p className="home-problem" role="alert">{problem}</p>}
          <div className="home-ask-foot">
            <button type="button" className="home-tool" aria-label="Attach a file" title="Attach a file from this computer" disabled={!canAsk || attachments.length >= MAX_ATTACHMENTS} onClick={attach}><Paperclip size={18} /></button>
            <span className="home-divider" aria-hidden="true" />
            <div className="home-scope">
              <button type="button" className="home-tool wide" aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen(!menuOpen)}>
                <Layers size={16} /> {scopeLabel} <ChevronDown size={14} />
              </button>
              {menuOpen && (
                <>
                  <div className="home-scope-shade" onClick={() => setMenuOpen(false)} />
                  <div className="home-scope-pop">
                    <ScopePanel limits={limits} cloudReady={cloudReady} onChange={setLimits} onClose={() => setMenuOpen(false)} />
                  </div>
                </>
              )}
            </div>
            <button className="home-send" type="submit" aria-label="Ask" disabled={!canAsk || text.trim().length < 3}><ArrowUp size={18} /></button>
          </div>
        </form>

        {shown.helpers.length > 0 && (
          <div className="home-helpers" role="group" aria-label="Give a task to a helper">
            <span>Give a task to</span>
            {shown.helpers.map((agent) => (
              <button key={agent.id} className="home-helper" aria-pressed={helper === agent.id} onClick={() => setHelper(helper === agent.id ? null : agent.id)}>
                <Bot size={16} /> <strong>{agent.name}</strong><em>{agent.role}</em><ArrowUpRight size={13} />
              </button>
            ))}
          </div>
        )}

        {cloudReady && (
          <section className={"home-attention " + (attention > 0 ? "has-items" : "")} aria-label="Needs your attention">
            <header>
              <span className="home-badge">{attention === 0 ? <CheckCheck size={18} /> : <Bell size={17} />}</span>
              <div><h2>Needs your attention</h2><p>{attention === 0 ? "Nothing is waiting for you." : attention + (attention === 1 ? " thing is" : " things are") + " waiting for you."}</p></div>
              <span className="home-count">{attention}</span>
            </header>
            {attention > 0 && (
              <ul className="home-list">
                {shown.reports.map((task) => (
                  <li key={"report-" + task.id}><button onClick={() => onOpenAgents(task.id)}><span className="home-icon"><ClipboardCheck size={17} /></span><span><strong>{task.title}</strong><small>Report from {task.agent_name} is ready for you to review</small></span><ArrowUpRight size={15} /></button></li>
                ))}
                {shown.prepared.map((item) => (
                  <li key={"prepared-" + item.id}><button onClick={() => onOpenAgents(item.task_id)}><span className="home-icon">{item.kind === "email" ? <Mail size={17} /> : <CalendarClock size={17} />}</span><span><strong>{item.kind === "email" ? item.subject : item.title}</strong><small>{item.agent_name} prepared {item.kind === "email" ? "an email" : "a meeting"} for you to approve</small></span><ArrowUpRight size={15} /></button></li>
                ))}
                {expiring.map((task) => (
                  <li key={"expiring-" + task.id}><button onClick={() => onOpenAgents(task.id)}><span className="home-icon"><Trash2 size={17} /></span><span><strong>{task.title}</strong><small>This report will be deleted in {daysLeft(task.deletes_epoch)} day{daysLeft(task.deletes_epoch) === 1 ? "" : "s"} unless you keep it</small></span><ArrowUpRight size={15} /></button></li>
                ))}
                {reconnect.map((connection) => (
                  <li key={"reconnect-" + connection.id}><button onClick={() => onSources("connectors")}><span className="home-icon warn"><AlertTriangle size={17} /></span><span><strong>{connection.label ?? connection.kind.replace(/_/g, " ")}</strong><small>Needs you to sign in again</small></span><ArrowUpRight size={15} /></button></li>
                ))}
              </ul>
            )}
          </section>
        )}

        {shown.recent.length > 0 && (
          <section className="home-section" aria-label="Recent reports">
            <h2>Recent reports <span className="home-count small">{shown.recent.length}</span></h2>
            <ul className="home-rows">
              {shown.recent.map((task) => (
                <li key={task.id}>
                  <button onClick={() => onOpenAgents(task.id)}>
                    <span className="home-tile">{/mail/i.test(task.title) ? <Mail size={20} /> : <ClipboardList size={20} />}</span>
                    <span><strong>{task.title}</strong><small>{task.agent_name}{task.finished_epoch ? " · " + ago(task.finished_epoch * 1000) : ""}</small></span>
                    <ArrowUpRight size={17} />
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}

        {chats.length > 0 && (
          <section className="home-section" aria-label="Recent chats">
            <h2>Recent chats <span className="home-count small">{chats.length}</span></h2>
            <ul className="home-rows">
              {chats.map((thread) => (
                <li key={thread.id}>
                  <button onClick={() => onOpenChat(thread.id)}>
                    <span className="home-tile"><MessageSquareText size={20} /></span>
                    <span><strong>{thread.title}</strong><small>{ago(thread.startedAt)}</small></span>
                    <ArrowUpRight size={17} />
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}

        {!firstRun && (
          <p className="home-status">
            <span><Folder size={15} /> {documentCount.toLocaleString()} file{documentCount === 1 ? "" : "s"} on this device</span>
            {cloudReady && <span><Plug size={15} /> {shown.connections.length} source{shown.connections.length === 1 ? "" : "s"} connected</span>}
            {usage && <span><BarChart3 size={15} /> {dollars(usage.used_micros)} of {dollars(usage.limit_micros)} used this month</span>}
          </p>
        )}
      </section>
    </div>
  );
}
