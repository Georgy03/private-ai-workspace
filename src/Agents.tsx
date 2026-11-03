import { useCallback, useEffect, useState } from "react";
import { Bot, ExternalLink, LoaderCircle, Plus, Square, X } from "lucide-react";
import {
  AGENT_TOOLS,
  cloudAgentCreate,
  cloudAgentRollback,
  cloudAgentSave,
  cloudAgentVersions,
  cloudAgents,
  cloudTask,
  cloudTaskCancel,
  cloudTaskStart,
  cloudTasks,
} from "./api";
import type { Agent, AgentDraft, AgentList, AgentTask, AgentVersion, CloudStatus } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const dollars = (micros: number) => `$${(micros / 1_000_000).toFixed(micros < 100_000 ? 3 : 2)}`;
const when = (epoch: number) => new Date(epoch * 1000).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
const active = (task: AgentTask) => task.status === "queued" || task.status === "running";
const STATUS_TEXT: Record<AgentTask["status"], string> = { queued: "Waiting", running: "Working", done: "Done", failed: "Failed", cancelled: "Stopped" };
const TOOL_TEXT: Record<string, string> = {
  search_documents: "Search documents",
  read_document: "Read whole documents",
  lookup_knowledge: "Look up confirmed company knowledge",
};

const JOHN: AgentDraft = {
  name: "John",
  role: "Research & Reporting",
  description: "Finds what your documents say about a question and writes a short, cited report.",
  instructions:
    "You are John, a research and reporting assistant for this firm. When given a question, search the firm's documents, read the most relevant ones, and write a clear report in Markdown: a short summary first, then the details, then a list of the documents you used. Quote figures and dates exactly as written. If the documents do not answer the question, say so and say what you did find.",
  tools: ["search_documents", "read_document", "lookup_knowledge"],
  model: "standard",
  effort: "medium",
};

const PLANNED = [
  { name: "Alex", role: "Operations" },
  { name: "Taylor", role: "Finance" },
  { name: "Casey", role: "Communications" },
];

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
            {task.agent_name} · {STATUS_TEXT[task.status]} · {dollars(task.cost_micros)} · {task.turns} step{task.turns === 1 ? "" : "s"}
            {active(task) && <LoaderCircle size={13} className="spin" />}
          </p>
          {task.error && <p className="knowledge-error" role="alert">{task.error}</p>}
          {task.instruction && <blockquote className="agent-ask">{task.instruction}</blockquote>}
          {task.result ? <div className="agent-report">{task.result}</div> : !task.instruction && <p className="knowledge-note">The request and the report are private to the person who asked.</p>}
          {task.sources && task.sources.length > 0 && (
            <ul className="agent-sources" aria-label="Documents used">
              {task.sources.map((source) => (
                <li key={source.document_id}>
                  {source.name}
                  {source.link && (
                    <button className="open-link" onClick={() => onOpenLink(source.link!)} aria-label={`Open ${source.name}`}><ExternalLink size={12} /> Open</button>
                  )}
                </li>
              ))}
            </ul>
          )}
          <h3>Activity</h3>
          <ol className="agent-timeline">
            {(task.events ?? []).map((event, index) => (
              <li key={index}><time>{when(event.epoch)}</time> {event.summary}</li>
            ))}
          </ol>
          {active(task) && (
            <button className="secondary" onClick={() => void cloudTaskCancel(id).then(onChanged, (caught) => setProblem(message(caught)))}>
              <Square size={13} /> Stop this task
            </button>
          )}
        </>
      )}
    </section>
  );
}

function Editor({ agent, onSaved, onClose }: { agent: Agent | null; onSaved: () => void; onClose: () => void }) {
  const [draft, setDraft] = useState<AgentDraft>(
    agent
      ? { name: agent.name, role: agent.role, description: agent.description, instructions: agent.instructions ?? "", tools: agent.tools, model: agent.model, effort: agent.effort, status: agent.status, audience: agent.audience, max_concurrent: agent.max_concurrent, monthly_budget_dollars: agent.monthly_budget_micros === null ? null : agent.monthly_budget_micros / 1_000_000 }
      : { ...JOHN, status: "available", max_concurrent: 3, monthly_budget_dollars: null },
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

  const set = <K extends keyof AgentDraft>(key: K, value: AgentDraft[K]) => setDraft((old) => ({ ...old, [key]: value }));
  const toggleTool = (tool: string) => set("tools", draft.tools.includes(tool) ? draft.tools.filter((t) => t !== tool) : [...draft.tools, tool]);
  const everyone = (draft.audience ?? ["everyone"]).includes("everyone");

  const save = () => {
    setBusy(true);
    (agentId ? cloudAgentSave(agentId, draft) : cloudAgentCreate(draft).then(() => undefined)).then(
      () => { setBusy(false); onSaved(); },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  return (
    <section className="agent-panel" aria-label={agent ? `Edit ${agent.name}` : "New agent"}>
      <header>
        <h2>{agent ? `Edit ${agent.name}` : "New agent"}</h2>
        <button className="icon-button" onClick={onClose} aria-label="Close editor"><X size={16} /></button>
      </header>
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}
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
          <option value="standard">Standard (Haiku): faster and cheaper</option>
          <option value="advanced">Advanced (Sonnet): deeper analysis, uses more of the AI allowance</option>
        </select>
      </label>
      <label>Effort
        <select value={draft.effort} onChange={(event) => set("effort", event.target.value as AgentDraft["effort"])}>
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
        <input type="number" min={1} max={20} value={draft.max_concurrent ?? 3} onChange={(event) => set("max_concurrent", Number(event.target.value))} />
      </label>
      {agent && (
        <label>Status
          <select value={draft.status} onChange={(event) => set("status", event.target.value as Agent["status"])}>
            <option value="available">Available</option><option value="paused">Paused</option><option value="archived">Archived</option>
          </select>
        </label>
      )}
      {agent && <label>What changed (optional)<input value={draft.note ?? ""} maxLength={200} onChange={(event) => set("note", event.target.value)} /></label>}
      <div className="knowledge-actions">
        <button className="primary" disabled={busy || !draft.name.trim() || !draft.instructions.trim()} onClick={save}>{agent ? "Save" : "Create agent"}</button>
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

export function AgentsView({ cloud, onOpenLink }: { cloud: CloudStatus; onOpenLink: (url: string) => void }) {
  const [list, setList] = useState<AgentList | null>(null);
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const [giving, setGiving] = useState<Agent | null>(null);
  const [instruction, setInstruction] = useState("");
  const [editing, setEditing] = useState<Agent | "new" | null>(null);
  const [openTask, setOpenTask] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const usable = cloud.configured && cloud.signed_in;

  const reload = useCallback(
    () =>
      Promise.all([cloudAgents(), cloudTasks()]).then(
        ([agents, mine]) => { setList(agents); setTasks(mine); setProblem(null); },
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
    return <div className="knowledge-view"><p className="knowledge-note">Sign in to the firm's cloud workspace to use shared agents.</p></div>;
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

  return (
    <div className="knowledge-view agents-view">
      <section className="knowledge-intro">
        <h2>Agents</h2>
        <p>Shared AI workers your firm sets up. Everyone can give an agent a task; each task is private to the person who started it, and the agent can only read documents that person is allowed to open. Agents only read: they never change anything.</p>
      </section>
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}

      <div className="agent-grid">
        {agents.map((agent) => (
          <article key={agent.id} className="knowledge-card agent-card">
            <header>
              <Bot size={18} />
              <div><strong>{agent.name}</strong><span>{agent.role}</span></div>
              <em className={`knowledge-badge ${agent.status === "available" ? "ok" : "warn"}`}>{agent.status === "available" ? "Available" : agent.status === "paused" ? "Paused" : "Archived"}</em>
            </header>
            <p>{agent.description}</p>
            {canManage && <small>{agent.running_now} working now · {dollars(agent.spent_this_month_micros)} this month{agent.monthly_budget_micros !== null ? ` of ${dollars(agent.monthly_budget_micros)}` : ""} · {agent.model === "advanced" ? "Advanced model" : "Standard model"}</small>}
            <div className="knowledge-actions">
              <button className="primary" disabled={agent.status !== "available"} onClick={() => { setGiving(agent); setEditing(null); }}>Give a task</button>
              {canManage && <button className="secondary" onClick={() => { setEditing(agent); setGiving(null); }}>Edit</button>}
            </div>
          </article>
        ))}
        {PLANNED.map((planned) => (
          <article key={planned.name} className="knowledge-card agent-card planned" aria-label={`${planned.name} (planned)`}>
            <header><Bot size={18} /><div><strong>{planned.name}</strong><span>{planned.role}</span></div><em className="knowledge-badge">Planned</em></header>
            <p>Not built yet.</p>
          </article>
        ))}
      </div>

      {canManage && agents.length === 0 && (
        <div className="knowledge-actions">
          <button className="primary" onClick={() => setEditing("new")}><Plus size={14} /> Create John, the research and reporting agent</button>
        </div>
      )}
      {canManage && agents.length > 0 && (
        <div className="knowledge-actions"><button className="secondary" onClick={() => setEditing("new")}><Plus size={14} /> New agent</button></div>
      )}

      {giving && (
        <section className="agent-panel" aria-label={`Give ${giving.name} a task`}>
          <header><h2>Give {giving.name} a task</h2><button className="icon-button" onClick={() => setGiving(null)} aria-label="Close"><X size={16} /></button></header>
          <textarea aria-label="Task" rows={4} maxLength={4000} value={instruction} placeholder="What should it look into?" onChange={(event) => setInstruction(event.target.value)} />
          <p className="knowledge-note">{giving.model === "advanced" ? "This agent uses the advanced model, which uses more of your monthly AI allowance. " : ""}The work counts toward your monthly AI allowance.</p>
          <div className="knowledge-actions"><button className="primary" disabled={busy || instruction.trim().length === 0} onClick={start}>Start task</button></div>
        </section>
      )}

      {editing && <Editor key={editing === "new" ? "new" : editing.id} agent={editing === "new" ? null : editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void reload(); }} />}
      {openTask && <TaskDetail key={openTask} id={openTask} onClose={() => setOpenTask(null)} onOpenLink={onOpenLink} onChanged={() => void reload()} />}

      <section>
        <h3>Your tasks</h3>
        {tasks.length === 0 ? <p className="knowledge-note">No tasks yet.</p> : (
          <ul className="agent-tasks">
            {tasks.map((task) => (
              <li key={task.id}>
                <button onClick={() => setOpenTask(task.id)}>
                  <strong>{task.title}</strong>
                  <span>{task.agent_name} · {when(task.created_epoch)} · {dollars(task.cost_micros)}</span>
                  <em className={`knowledge-badge ${task.status === "done" ? "ok" : task.status === "failed" ? "warn" : ""}`}>{STATUS_TEXT[task.status]}</em>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
