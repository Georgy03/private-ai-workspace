import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Inbox, LoaderCircle, Plus, Sparkles, Square, X } from "lucide-react";
import {
  AGENT_TOOLS,
  cloudAgentCreate,
  cloudAgentRollback,
  cloudAgentSave,
  cloudAgentVersions,
  cloudAgents,
  cloudActions,
  cloudConnectors,
  cloudEmailSending,
  cloudInbox,
  cloudRetentionSet,
  cloudTaskDelete,
  cloudTaskKeep,
  cloudSchedules,
  cloudTaskReview,
  cloudTask,
  cloudTaskCancel,
  cloudTaskStart,
  cloudTasks,
  draftHelper,
} from "./api";
import { TEMPLATES, type AgentTemplate } from "./agentTemplates";
import { Markdown } from "./Markdown";
import { Shimmer } from "./Shimmer";
import { ExportButtons } from "./ExportButtons";
import { ReportCard } from "./ReportCard";
import { ActionsSection, type Sender } from "./Actions";
import { InboxSection, SchedulesSection } from "./Schedules";
import type { Agent, AgentDraft, AgentList, AgentTask, AgentVersion, CloudStatus, Connection, PreparedItem, Schedule } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const dollars = (micros: number) => `$${(micros / 1_000_000).toFixed(micros < 100_000 ? 3 : 2)}`;
const when = (epoch: number) => new Date(epoch * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
const active = (task: AgentTask) => task.status === "queued" || task.status === "running";
const STATUS_TEXT: Record<AgentTask["status"], string> = { queued: "Getting ready", running: "Working on it…", done: "Ready to read", failed: "Something went wrong", cancelled: "Stopped" };
const TOOL_TEXT: Record<string, string> = {
  search_documents: "Search documents",
  prepare_email: "Prepare emails for you to check and send yourself (it can never send)",
  prepare_meeting: "Prepare meetings for you to check and add to your calendar yourself (it can never book)",
  read_document: "Read whole documents",
  lookup_knowledge: "Look up confirmed company knowledge",
};

const PLANNED = [
  { name: "Alex", role: "Operations" },
  { name: "Taylor", role: "Finance" },
  { name: "Casey", role: "Communications" },
];

/** Approve, edit or reject a report a schedule made. Only its owner sees this. */
function ReviewBar({ task, onDecided, onProblem }: { task: AgentTask; onDecided: () => void; onProblem: (text: string) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(task.edited_result ?? task.result ?? "");
  const [busy, setBusy] = useState(false);
  if (task.review_status !== "pending") {
    return <p className={`review-state ${task.review_status}`}>{task.review_status === "approved" ? "You approved this report" : "You rejected this report"}{task.edited_result ? " (with your edits)" : ""}.</p>;
  }
  const decide = (action: "approve" | "reject", edited?: string) => {
    setBusy(true);
    cloudTaskReview(task.id, action, edited).then(
      () => { setBusy(false); onDecided(); },
      (caught) => { setBusy(false); onProblem(message(caught)); },
    );
  };
  return editing ? (
    <div className="review-edit">
      <textarea aria-label="Edit the report" rows={14} value={draft} onChange={(event) => setDraft(event.target.value)} />
      <div className="knowledge-actions">
        <button className="primary" disabled={busy || !draft.trim()} onClick={() => decide("approve", draft)}>Approve with my edits</button>
        <button className="secondary" onClick={() => setEditing(false)}>Cancel</button>
      </div>
    </div>
  ) : (
    <div className="review-bar" role="group" aria-label="Review this report">
      <span>This report is waiting for your decision.</span>
      <button className="primary" disabled={busy || !task.result} onClick={() => decide("approve")}>Approve</button>
      <button className="secondary" disabled={!task.result} onClick={() => { setDraft(task.edited_result ?? task.result ?? ""); setEditing(true); }}>Edit</button>
      <button className="secondary" disabled={busy} onClick={() => decide("reject")}>Reject</button>
    </div>
  );
}

/** When this report will be deleted, and the owner's choice to keep or delete it now. */
function Retention({ task, onChanged, onGone, onProblem }: { task: AgentTask; onChanged: () => void; onGone: () => void; onProblem: (text: string) => void }) {
  const [confirming, setConfirming] = useState(false);
  if (task.deleted) return <p className="knowledge-note">This report has been deleted.</p>;
  if (!task.instruction || active(task)) return null; // only the owner, and only once it has finished
  const fail = (caught: unknown) => onProblem(message(caught));
  return (
    <div className="retention-row">
      <span>
        {task.retention_choice === "keep" ? "Kept until you delete it."
          : task.deletes_epoch ? `This report and its conversation will be deleted on ${new Date(task.deletes_epoch * 1000).toLocaleDateString([], { month: "long", day: "numeric", year: "numeric" })}.`
          : "Kept until you delete it."}
      </span>
      {task.retention_choice !== "keep" && task.deletes_epoch && <button className="secondary" onClick={() => void cloudTaskKeep(task.id).then(onChanged, fail)}>Keep it</button>}
      {confirming ? (
        <>
          <button className="danger" onClick={() => void cloudTaskDelete(task.id).then(onGone, fail)}>Delete for good</button>
          <button className="secondary" onClick={() => setConfirming(false)}>Cancel</button>
        </>
      ) : (
        <button className="ghost" onClick={() => setConfirming(true)}>Delete now</button>
      )}
    </div>
  );
}

/** The report and how it was made. Only the person who asked sees the report itself. */
function TaskDetail({ id, onClose, onOpenLink, onChanged }: { id: string; onClose: () => void; onOpenLink: (url: string) => void; onChanged: () => void }) {
  const [task, setTask] = useState<AgentTask | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      cloudTask(id).then(
        (next) => alive && (setTask(next), setProblem(null)),
        (caught) => alive && setProblem(message(caught)),
      );
    void load();
    const timer = window.setInterval(() => {
      if (!task || active(task)) void load();
    }, 2500);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [id, task]);

  return (
    <section className="agent-panel" aria-label="Task details">
      <header>
        <h2>{task?.title ?? "Task"}</h2>
        <button className="icon-button" onClick={onClose} aria-label="Close task"><X size={16} /></button>
      </header>
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}
      {task && (
        <>
          <p className="agent-meta">
            {task.agent_name} · {active(task) ? <Shimmer>{STATUS_TEXT[task.status]}</Shimmer> : STATUS_TEXT[task.status]} · {dollars(task.cost_micros)} · {task.turns} step{task.turns === 1 ? "" : "s"}
            {active(task) && <LoaderCircle size={13} className="spin" />}
          </p>
          {task.error && <p className="knowledge-error" role="alert">{task.error}</p>}
          {task.instruction && <blockquote className="agent-ask">{task.instruction}</blockquote>}
          {task.review_status && <ReviewBar task={task} onDecided={() => { setTask(null); onChanged(); }} onProblem={setProblem} />}
          {task.result ? (
            <>
              <ReportCard task={task} onOpenLink={onOpenLink} />
              <h3>Full report</h3>
              <div className="agent-report"><Markdown text={task.edited_result ?? task.result} /></div>
              <ExportButtons title={task.title} text={task.edited_result ?? task.result} />
            </>
          ) : !task.instruction && <p className="knowledge-note">The request and the report are private to the person who asked.</p>}
          <Retention task={task} onChanged={() => { setTask(null); onChanged(); }} onGone={onClose} onProblem={setProblem} />
          <h3>What it did</h3>
          <ol className="agent-timeline">
            {(task.events ?? []).map((event, index) => (
              <li key={index}><time>{when(event.epoch)}</time> {event.summary}</li>
            ))}
          </ol>
          {active(task) && (
            <button className="secondary" onClick={() => void cloudTaskCancel(id).then(onChanged, (caught) => setProblem(message(caught)))}>
              <Square size={13} /> Stop
            </button>
          )}
        </>
      )}
    </section>
  );
}

/** What the form holds while it is being filled in. Model and effort start unchosen on a blank helper. */
type EditorDraft = Omit<AgentDraft, "model" | "effort"> & { model: AgentDraft["model"] | ""; effort: AgentDraft["effort"] | "" };

const BLANK: EditorDraft = { name: "", role: "", description: "", instructions: "", tools: [], model: "", effort: "", status: "available", audience: ["everyone"], monthly_budget_dollars: null };

function Editor({ agent, template, onSaved, onClose }: { agent: Agent | null; template: AgentTemplate | null; onSaved: () => void; onClose: () => void }) {
  const [draft, setDraft] = useState<EditorDraft>(
    agent
      ? { name: agent.name, role: agent.role, description: agent.description, instructions: agent.instructions ?? "", tools: agent.tools, model: agent.model, effort: agent.effort, status: agent.status, audience: agent.audience, max_concurrent: agent.max_concurrent, monthly_budget_dollars: agent.monthly_budget_micros === null ? null : agent.monthly_budget_micros / 1_000_000 }
      : template
        ? { ...BLANK, ...template.draft }
        : BLANK,
  );
  const [versions, setVersions] = useState<AgentVersion[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const agentId = agent?.id;

  useEffect(() => {
    if (!agentId) return;
    let alive = true;
    void cloudAgentVersions(agentId).then((list) => alive && setVersions(list), () => undefined);
    return () => { alive = false; };
  }, [agentId, agent?.version]);

  const set = <K extends keyof EditorDraft>(key: K, value: EditorDraft[K]) => setDraft((old) => ({ ...old, [key]: value }));
  const toggleTool = (tool: string) => set("tools", draft.tools.includes(tool) ? draft.tools.filter((t) => t !== tool) : [...draft.tools, tool]);
  const everyone = (draft.audience ?? ["everyone"]).includes("everyone");

  const ready = draft.name.trim() && draft.instructions.trim() && draft.tools.length > 0 && draft.model && draft.effort;

  const save = () => {
    if (!draft.model || !draft.effort) return;
    const complete: AgentDraft = { ...draft, model: draft.model, effort: draft.effort };
    setBusy(true);
    (agentId ? cloudAgentSave(agentId, complete) : cloudAgentCreate(complete).then(() => undefined)).then(
      () => { setBusy(false); onSaved(); },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  return (
    <section className="agent-panel" aria-label={agent ? `Edit ${agent.name}` : "New helper"}>
      <header>
        <h2>{agent ? `Edit ${agent.name}` : "New helper"}</h2>
        <button className="icon-button" onClick={onClose} aria-label="Close editor"><X size={16} /></button>
      </header>
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}
      {template && <p className="knowledge-note">{template.id === "described" ? "Drafted from your description. Read it, change anything you like, then create it." : `Based on the “${template.title}” template. Change anything you like.`}</p>}
      <label>Name<input value={draft.name} maxLength={60} onChange={(event) => set("name", event.target.value)} /></label>
      <label>Role<input value={draft.role} maxLength={80} onChange={(event) => set("role", event.target.value)} /></label>
      <label>What it is for<input value={draft.description} maxLength={400} onChange={(event) => set("description", event.target.value)} /></label>
      <label>Instructions<textarea rows={7} value={draft.instructions} maxLength={8000} onChange={(event) => set("instructions", event.target.value)} /></label>
      <fieldset>
        <legend>What it may do (it can only read, never change anything)</legend>
        {AGENT_TOOLS.map((tool) => (
          <label key={tool} className="check"><input type="checkbox" checked={draft.tools.includes(tool)} onChange={() => toggleTool(tool)} /> {TOOL_TEXT[tool]}</label>
        ))}
      </fieldset>
      <label>Model
        <select value={draft.model} onChange={(event) => set("model", event.target.value as AgentDraft["model"])}>
          {!draft.model && <option value="">Choose…</option>}
          <option value="standard">Standard (Haiku): faster and cheaper</option>
          <option value="advanced">Advanced (Sonnet): deeper analysis, uses more of the AI allowance</option>
        </select>
      </label>
      <label>Effort
        <select value={draft.effort} onChange={(event) => set("effort", event.target.value as AgentDraft["effort"])}>
          {!draft.effort && <option value="">Choose…</option>}
          <option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option>
        </select>
      </label>
      <label className="check"><input type="checkbox" checked={everyone} onChange={(event) => set("audience", event.target.checked ? ["everyone"] : [])} /> Available to everyone in the firm</label>
      {!everyone && (
        <label>Who may use it (sign-in IDs or groups, comma separated, e.g. user:alice, group:finance)
          <input value={(draft.audience ?? []).join(", ")} onChange={(event) => set("audience", event.target.value.split(",").map((x) => x.trim()).filter(Boolean))} />
        </label>
      )}
      <label>Monthly budget in dollars (blank for no limit)
        <input type="number" min={0} value={draft.monthly_budget_dollars ?? ""} onChange={(event) => set("monthly_budget_dollars", event.target.value === "" ? null : Number(event.target.value))} />
      </label>
      <label>Tasks at once
        <input type="number" min={1} max={20} value={draft.max_concurrent ?? ""} placeholder="3 if left empty" onChange={(event) => set("max_concurrent", event.target.value === "" ? undefined : Number(event.target.value))} />
      </label>
      {agent && (
        <label>Status
          <select value={draft.status} onChange={(event) => set("status", event.target.value as Agent["status"])}>
            <option value="available">Available</option><option value="paused">Paused</option><option value="archived">Archived</option>
          </select>
        </label>
      )}
      {agent && <label>What changed (optional)<input value={draft.note ?? ""} maxLength={200} onChange={(event) => set("note", event.target.value)} /></label>}
      {!agent && !ready && <p className="knowledge-note">To create the helper, fill in a name and instructions, tick at least one thing it may do, and choose a model and effort.</p>}
      <div className="knowledge-actions">
        <button className="primary" disabled={busy || !ready} onClick={save}>{agent ? "Save" : "Create helper"}</button>
        <button className="secondary" onClick={onClose}>Cancel</button>
      </div>
      {agent && versions.length > 0 && (
        <>
          <h3>Versions</h3>
          <ul className="agent-versions">
            {versions.map((version) => (
              <li key={version.version}>
                <span><strong>Version {version.version}</strong>{version.current ? " (current)" : ""} · {when(version.created_epoch)} · {version.created_by}{version.note ? ` · ${version.note}` : ""}</span>
                {!version.current && (
                  <button className="secondary" disabled={busy} onClick={() => { setBusy(true); void cloudAgentRollback(agent.id, version.version).then(() => { setBusy(false); onSaved(); }, (caught) => { setBusy(false); setProblem(message(caught)); }); }}>
                    Go back to this
                  </button>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

const COLORS = ["#4285f4", "#0f9d58", "#e8710a", "#9b5de5", "#d93a6f"];
const EXAMPLES = ["Summarize everything we have about ", "What is our policy on ", "Make an expense report for "];

/** A round badge with the helper's first letter, in a colour of its own. */
function Avatar({ name, index, size = 52 }: { name: string; index: number; size?: number }) {
  return (
    <span className="helper-avatar" style={{ background: COLORS[index % COLORS.length], width: size, height: size, fontSize: size * 0.46 }} aria-hidden="true">
      {name.trim().slice(0, 1).toUpperCase()}
    </span>
  );
}

/** A box that sits over the page, so what to do next is the only thing in view. */
function Modal({ label, onClose, children }: { label: string; onClose: () => void; children: ReactNode }) {
  return (
    <div className="agent-modal" role="dialog" aria-modal="true" aria-label={label} onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="agent-modal-body">{children}</div>
    </div>
  );
}

/** The first step of adding a helper: start from a ready-made one, or from nothing. */
function TemplatePicker({ onPick, onClose }: { onPick: (template: AgentTemplate | null) => void; onClose: () => void }) {
  const [wish, setWish] = useState("");
  const [drafting, setDrafting] = useState(false);
  const [trouble, setTrouble] = useState<string | null>(null);
  // The description becomes a draft helper, opened in the editor to read and change before it exists.
  const draft = () => {
    setDrafting(true);
    setTrouble(null);
    draftHelper(wish).then(
      (made) => { setDrafting(false); onPick({ id: "described", title: "your description", blurb: "", draft: made }); },
      (caught) => { setDrafting(false); setTrouble(message(caught)); },
    );
  };
  return (
    <section className="agent-panel template-picker" aria-label="Choose a helper to start from">
      <header>
        <h2>What kind of helper do you want?</h2>
        <button className="icon-button" onClick={onClose} aria-label="Close"><X size={16} /></button>
      </header>
      <form className="describe-helper" onSubmit={(event) => { event.preventDefault(); if (!drafting) draft(); }}>
        <label>
          <span>Describe the helper you want</span>
          <textarea rows={3} maxLength={1000} value={wish} placeholder="For example: checks every engagement letter for a missing signature or payment terms and writes me a list each Monday" onChange={(event) => setWish(event.target.value)} />
        </label>
        <div className="knowledge-actions">
          <button type="submit" className="primary" disabled={drafting || wish.trim().length < 10}>{drafting ? <Shimmer>Drafting…</Shimmer> : <><Sparkles size={14} /> Draft this helper</>}</button>
          <small>Only this sentence is sent to Claude, not your documents. You review the draft before anything is created.</small>
        </div>
        {trouble && <p className="knowledge-error" role="alert">{trouble}</p>}
      </form>
      <p className="knowledge-note">Or pick one to start from. You can change everything before it is created.</p>
      <div className="template-grid">
        <button className="template-card blank" onClick={() => onPick(null)}>
          <span className="helper-avatar plus" aria-hidden="true"><Plus size={22} /></span>
          <strong>Start from scratch</strong>
          <span>A blank helper. You write what it does.</span>
        </button>
        {TEMPLATES.map((template, index) => (
          <button key={template.id} className="template-card" aria-label={`Use the template: ${template.title}`} style={{ ["--tone" as string]: COLORS[index % COLORS.length] }} onClick={() => onPick(template)}>
            <Avatar name={template.draft.name} index={index} size={40} />
            <strong>{template.title}</strong>
            <span>{template.blurb}</span>
            {template.note && <em>{template.note}</em>}
          </button>
        ))}
      </div>
    </section>
  );
}

type AgentsTab = "helpers" | "reports" | "repeat";

export function AgentsView({ cloud, onOpenLink, openTaskId = null }: { cloud: CloudStatus; onOpenLink: (url: string) => void; openTaskId?: string | null }) {
  const [list, setList] = useState<AgentList | null>(null);
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [inbox, setInbox] = useState<AgentTask[]>([]);
  const [prepared, setPrepared] = useState<PreparedItem[]>([]);
  const [mailbox, setMailbox] = useState<Connection | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [tab, setTab] = useState<AgentsTab>(openTaskId ? "reports" : "helpers");
  const [giving, setGiving] = useState<Agent | null>(null);
  const [instruction, setInstruction] = useState("");
  const [editing, setEditing] = useState<Agent | "new" | null>(null);
  const [picking, setPicking] = useState(false);
  const [template, setTemplate] = useState<AgentTemplate | null>(null);
  const [openTask, setOpenTask] = useState<string | null>(openTaskId);
  const [busy, setBusy] = useState(false);
  const usable = cloud.configured && cloud.signed_in;

  const reload = useCallback(
    () =>
      Promise.all([cloudAgents(), cloudTasks(), cloudSchedules(), cloudInbox(), cloudActions(), cloudConnectors().then((view) => view.connections.find((c) => c.kind === "outlook_mail") ?? null, () => null)]).then(
        ([agents, mine, scheduled, waiting, preparedItems, connected]) => { setList(agents); setTasks(mine); setSchedules(scheduled); setInbox(waiting); setPrepared(preparedItems); setMailbox(connected); setProblem(null); },
        (caught) => setProblem(message(caught)),
      ),
    [],
  );

  useEffect(() => {
    if (!usable) return;
    void reload();
    const timer = window.setInterval(() => void reload(), 5000);
    return () => window.clearInterval(timer);
  }, [usable, reload]);

  if (!usable) {
    return <div className="knowledge-view"><p className="knowledge-note">Sign in to the firm's cloud workspace to use your AI helpers.</p></div>;
  }

  const start = () => {
    if (!giving) return;
    setBusy(true);
    void cloudTaskStart(giving.id, instruction.trim()).then(
      (id) => { setBusy(false); setGiving(null); setInstruction(""); setOpenTask(id); void reload(); },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  const canManage = list?.can_manage ?? false;
  const agents = list?.agents ?? [];
  const working = tasks.filter(active).length;
  const sender: Sender = {
    enabled: list?.email_sending_enabled ?? false,
    mailbox: mailbox?.status === "active" ? (mailbox.label ?? "your Outlook") : null,
    needsReconnect: mailbox?.status === "needs_reauth",
  };

  return (
    <div className="agents-page">
      <section className="agents-hero">
        <h1>Your AI helpers</h1>
        <p>Pick a helper, tell it what you need, and it reads your documents and writes you a report.</p>
        <ol className="how-steps" aria-label="How it works">
          <li><b style={{ background: COLORS[0] }}>1</b> Pick a helper</li>
          <li><b style={{ background: COLORS[1] }}>2</b> Tell it what to find out</li>
          <li><b style={{ background: COLORS[2] }}>3</b> Read your report</li>
        </ol>
      </section>

      {problem && <p className="knowledge-error" role="alert">{problem}</p>}

      {canManage && (
        <label className="retention-setting sending-setting">
          <input type="checkbox" checked={list?.email_sending_enabled ?? false} onChange={(event) => void cloudEmailSending(event.target.checked).then(() => reload(), (caught) => setProblem(message(caught)))} />
          Let people send the emails their helpers prepare, from the app
          <small>Each person connects their own Outlook and presses Send after reading the draft. Off until you turn it on.</small>
        </label>
      )}
      {canManage && (
        <label className="retention-setting">Keep reports for
          <select value={list?.retention_days ?? 90} onChange={(event) => void cloudRetentionSet(Number(event.target.value)).then(() => reload(), (caught) => setProblem(message(caught)))}>
            <option value={30}>30 days</option><option value={90}>90 days</option><option value={180}>180 days</option><option value={365}>1 year</option><option value={0}>Until their owners delete them</option>
          </select>
          <small>Each person is asked a week before one of their reports is deleted, and can keep it.</small>
        </label>
      )}

      {inbox.length + prepared.length > 0 && tab !== "reports" && (
        <button className="inbox-banner" onClick={() => setTab("reports")}>
          <Inbox size={20} />
          <span>
            <strong>
              {[
                inbox.length > 0 && (inbox.length === 1 ? "1 report" : `${inbox.length} reports`),
                prepared.length > 0 && (prepared.length === 1 ? "1 email or meeting" : `${prepared.length} emails and meetings`),
              ].filter(Boolean).join(" and ")}{" "}
              {inbox.length + prepared.length === 1 ? "is" : "are"} waiting for you
            </strong>{" "}
            Tap to look {inbox.length + prepared.length === 1 ? "at it" : "at them"}.
          </span>
        </button>
      )}

      <div className="agent-tabs" role="tablist" aria-label="Agents sections">
        <button role="tab" aria-selected={tab === "helpers"} className={tab === "helpers" ? "active" : ""} onClick={() => setTab("helpers")}>Helpers</button>
        <button role="tab" aria-selected={tab === "reports"} className={tab === "reports" ? "active" : ""} onClick={() => setTab("reports")}>
          My reports{inbox.length + prepared.length > 0 && <em className="tab-badge">{inbox.length + prepared.length}</em>}{inbox.length + prepared.length === 0 && working > 0 && <LoaderCircle size={13} className="spin" />}
        </button>
        <button role="tab" aria-selected={tab === "repeat"} className={tab === "repeat" ? "active" : ""} onClick={() => setTab("repeat")}>
          Repeat jobs{schedules.length > 0 && <em className="tab-badge quiet">{schedules.length}</em>}
        </button>
      </div>

      {tab === "helpers" && (
        <>
          {agents.length === 0 ? (
            <section className="helpers-empty">
              <Sparkles size={28} />
              {canManage ? (
                <>
                  <h2>Add your first helper</h2>
                  <p>Start from a ready-made helper, like one that finds answers and writes reports, or one that checks budgets. You can change anything.</p>
                  <button className="primary big" onClick={() => setPicking(true)}><Plus size={16} /> Add a helper</button>
                </>
              ) : (
                <>
                  <h2>No helpers yet</h2>
                  <p>Your firm has not added a helper. Ask an administrator to set one up.</p>
                </>
              )}
            </section>
          ) : (
            <div className="helper-grid">
              {agents.map((agent, index) => (
                <article key={agent.id} className={`helper-card ${agent.status !== "available" ? "resting" : ""}`} style={{ ["--tone" as string]: COLORS[index % COLORS.length] }}>
                  <Avatar name={agent.name} index={index} />
                  <h2>{agent.name}</h2>
                  <span className="helper-role">{agent.role}</span>
                  <p>{agent.description}</p>
                  {agent.status !== "available" && <em className="knowledge-badge warn">{agent.status === "paused" ? "Resting" : "Retired"}</em>}
                  {canManage && <small>{agent.running_now} working now · {dollars(agent.spent_this_month_micros)} this month{agent.monthly_budget_micros !== null ? ` of ${dollars(agent.monthly_budget_micros)}` : ""} · {agent.model === "advanced" ? "Advanced model" : "Standard model"}</small>}
                  <div className="helper-actions">
                    <button className="primary big" disabled={agent.status !== "available"} onClick={() => { setGiving(agent); setEditing(null); }}>Ask {agent.name}</button>
                    {canManage && <button className="ghost" onClick={() => { setTemplate(null); setEditing(agent); setGiving(null); }}>Edit</button>}
                  </div>
                </article>
              ))}
              {canManage && (
                <button className="helper-card add" onClick={() => setPicking(true)}>
                  <span className="helper-avatar plus" aria-hidden="true"><Plus size={24} /></span>
                  <h2>Add a helper</h2>
                  <p>Make a new helper with its own job.</p>
                </button>
              )}
            </div>
          )}
          <p className="coming-soon">More helpers are on the way: {PLANNED.map((planned) => `${planned.name} (${planned.role})`).join(", ")}.</p>
        </>
      )}

      {tab === "reports" && (
        <>
          <ActionsSection items={prepared} sender={sender} onChanged={() => void reload()} />
          <InboxSection items={inbox} onOpenTask={setOpenTask} />
          <section aria-label="All reports">
            <h3>All my reports</h3>
            {tasks.length === 0 ? (
              <div className="reports-empty">
                <p>No reports yet. Ask a helper a question and the report will show up here.</p>
                <button className="primary" onClick={() => setTab("helpers")}>Go to the helpers</button>
              </div>
            ) : (
              <ul className="agent-tasks">
                {tasks.map((task) => (
                  <li key={task.id}>
                    <button onClick={() => setOpenTask(task.id)}>
                      <strong>{task.title}</strong>
                      <span>{task.agent_name} · {when(task.created_epoch)}</span>
                      <em className={`status-pill ${task.status}`}>{active(task) && <i className="pulse" aria-hidden="true" />}{STATUS_TEXT[task.status]}</em>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}

      {tab === "repeat" && <SchedulesSection schedules={schedules} agents={agents} onChanged={() => void reload()} onOpenTask={setOpenTask} onProblem={setProblem} />}

      {giving && (
        <Modal label={`Ask ${giving.name}`} onClose={() => setGiving(null)}>
          <section className="agent-panel" aria-label={`Ask ${giving.name}`}>
            <header>
              <div className="ask-head"><Avatar name={giving.name} index={Math.max(0, agents.findIndex((a) => a.id === giving.id))} size={44} /><h2>What should {giving.name} find out?</h2></div>
              <button className="icon-button" onClick={() => setGiving(null)} aria-label="Close"><X size={16} /></button>
            </header>
            <textarea aria-label="Your question" rows={4} maxLength={4000} autoFocus value={instruction} placeholder="Type your question here, like a message to a coworker." onChange={(event) => setInstruction(event.target.value)} />
            <div className="example-chips" aria-label="Ideas">
              <span>Need an idea?</span>
              {EXAMPLES.map((example) => <button key={example} className="chip" onClick={() => setInstruction(example)}>{example.trim()}…</button>)}
            </div>
            <p className="knowledge-note">{giving.model === "advanced" ? `${giving.name} uses the advanced model, which uses more of your monthly AI allowance. ` : ""}This counts toward your monthly AI allowance.</p>
            <div className="knowledge-actions"><button className="primary big" disabled={busy || instruction.trim().length === 0} onClick={start}>{busy ? "Sending…" : `Ask ${giving.name}`}</button></div>
          </section>
        </Modal>
      )}

      {picking && (
        <Modal label="Add a helper" onClose={() => setPicking(false)}>
          <TemplatePicker onClose={() => setPicking(false)} onPick={(chosen) => { setPicking(false); setTemplate(chosen); setEditing("new"); }} />
        </Modal>
      )}

      {editing && (
        <Modal label={editing === "new" ? "New helper" : `Edit ${editing.name}`} onClose={() => setEditing(null)}>
          <Editor key={editing === "new" ? `new-${template?.id ?? "blank"}` : editing.id} agent={editing === "new" ? null : editing} template={editing === "new" ? template : null} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void reload(); }} />
        </Modal>
      )}

      {openTask && (
        <Modal label="Report" onClose={() => setOpenTask(null)}>
          <TaskDetail key={openTask} id={openTask} onClose={() => setOpenTask(null)} onOpenLink={onOpenLink} onChanged={() => void reload()} />
        </Modal>
      )}
    </div>
  );
}
