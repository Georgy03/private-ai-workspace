import { useState } from "react";
import { CalendarClock, Pause, Play, Trash2, X } from "lucide-react";
import { cloudScheduleCreate, cloudScheduleRemove, cloudScheduleRun, cloudScheduleUpdate, localOffsetMinutes } from "./api";
import { describeCadence } from "./scheduleText";
import type { Agent, AgentTask, Cadence, Schedule } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const dollars = (micros: number) => `$${(micros / 1_000_000).toFixed(micros < 100_000 ? 3 : 2)}`;
const when = (epoch: number) => new Date(epoch * 1000).toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

const toTime = (minutes: number) => `${String(Math.floor(minutes / 60)).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`;
const fromTime = (value: string) => {
  const [h, m] = value.split(":").map(Number);
  return (h || 0) * 60 + (m || 0);
};

function NewSchedule({ agents, onDone, onClose }: { agents: Agent[]; onDone: () => void; onClose: () => void }) {
  const usable = agents.filter((agent) => agent.status === "available");
  const [agentId, setAgentId] = useState(usable[0]?.id ?? "");
  const [name, setName] = useState("");
  const [instruction, setInstruction] = useState("");
  const [cadence, setCadence] = useState<Cadence>({ kind: "weekly", weekday: 4, day: 1, minutes: 9 * 60, offset: localOffsetMinutes() });
  const [budget, setBudget] = useState<number | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const save = () => {
    setBusy(true);
    cloudScheduleCreate({ agent_id: agentId, name: name.trim(), instruction: instruction.trim(), cadence, monthly_budget_dollars: budget }).then(
      () => { setBusy(false); onDone(); },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  return (
    <section className="agent-panel" aria-label="New repeat job">
      <header><h2>New repeat job</h2><button className="icon-button" onClick={onClose} aria-label="Close"><X size={16} /></button></header>
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}
      <label>Which helper
        <select value={agentId} onChange={(event) => setAgentId(event.target.value)}>
          {usable.map((agent) => <option key={agent.id} value={agent.id}>{agent.name} · {agent.role}</option>)}
        </select>
      </label>
      <label>Name<input value={name} maxLength={80} placeholder="Weekly performance report" onChange={(event) => setName(event.target.value)} /></label>
      <label>What should the helper do each time?
        <textarea rows={4} maxLength={4000} value={instruction} placeholder="Create the weekly performance report and compare it with last week's." onChange={(event) => setInstruction(event.target.value)} />
      </label>
      <label>How often
        <select value={cadence.kind} onChange={(event) => setCadence({ ...cadence, kind: event.target.value as Cadence["kind"] })}>
          <option value="weekly">Every week</option><option value="daily">Every day</option><option value="monthly">Every month</option>
        </select>
      </label>
      {cadence.kind === "weekly" && (
        <label>On
          <select value={cadence.weekday} onChange={(event) => setCadence({ ...cadence, weekday: Number(event.target.value) })}>
            {WEEKDAYS.map((day, index) => <option key={day} value={index}>{day}</option>)}
          </select>
        </label>
      )}
      {cadence.kind === "monthly" && (
        <label>Day of the month (1 to 28)
          <input type="number" min={1} max={28} value={cadence.day} onChange={(event) => setCadence({ ...cadence, day: Math.min(28, Math.max(1, Number(event.target.value) || 1)) })} />
        </label>
      )}
      <label>At<input type="time" value={toTime(cadence.minutes)} onChange={(event) => setCadence({ ...cadence, minutes: fromTime(event.target.value) })} /></label>
      <label>Monthly budget in dollars (blank for no limit)
        <input type="number" min={0} value={budget ?? ""} onChange={(event) => setBudget(event.target.value === "" ? null : Number(event.target.value))} />
      </label>
      <p className="knowledge-note">It runs as you, with the documents you can open. Each report waits in your Inbox for you to approve, edit or reject. If nothing the last report used has changed, that week is skipped. Open the app at least every two weeks, or it pauses.</p>
      <div className="knowledge-actions">
        <button className="primary" disabled={busy || !agentId || !name.trim() || !instruction.trim()} onClick={save}>Start repeating</button>
        <button className="secondary" onClick={onClose}>Cancel</button>
      </div>
    </section>
  );
}

export function SchedulesSection({ schedules, agents, onChanged, onOpenTask, onProblem }: {
  schedules: Schedule[];
  agents: Agent[];
  onChanged: () => void;
  onOpenTask: (id: string) => void;
  onProblem: (text: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const act = (work: Promise<unknown>) => work.then(onChanged, (caught) => onProblem(message(caught)));

  return (
    <section aria-label="Repeat jobs">
      <h3>Repeat jobs</h3>
      {schedules.length === 0 && !creating && <p className="knowledge-note">Nothing repeats yet. Choose a helper, say what to do, and pick a day: for example, a report every Friday that waits here for you to approve.</p>}
      <ul className="schedule-list">
        {schedules.map((schedule) => (
          <li key={schedule.id} className={schedule.status === "paused" ? "paused" : ""}>
            <CalendarClock size={18} />
            <div className="schedule-main">
              <strong>{schedule.name}</strong>
              <span>{schedule.agent_name} · {describeCadence(schedule.cadence)}</span>
              <small>
                {schedule.status === "paused" ? `Paused${schedule.pause_reason ? `: ${schedule.pause_reason}` : ""}` : schedule.next_run_epoch ? `Next run ${when(schedule.next_run_epoch)}` : ""}
                {schedule.last_run_epoch ? ` · Last run ${when(schedule.last_run_epoch)}` : ""}
                {` · ${dollars(schedule.spent_this_month_micros)} this month${schedule.monthly_budget_micros !== null ? ` of ${dollars(schedule.monthly_budget_micros)}` : ""}`}
              </small>
              {schedule.last_note && <small className="schedule-note">{schedule.last_note}</small>}
              {schedule.waiting_for_review > 0 && schedule.last_task_id && (
                <button className="link-button" onClick={() => onOpenTask(schedule.last_task_id!)}>{schedule.waiting_for_review} waiting for your review</button>
              )}
            </div>
            <div className="schedule-actions">
              <button className="secondary" onClick={() => void act(cloudScheduleRun(schedule.id).then(onOpenTask))}>Run now</button>
              <button className="secondary" aria-label={`${schedule.status === "active" ? "Pause" : "Resume"} ${schedule.name}`} onClick={() => void act(cloudScheduleUpdate(schedule.id, { status: schedule.status === "active" ? "paused" : "active" }))}>
                {schedule.status === "active" ? <><Pause size={13} /> Pause</> : <><Play size={13} /> Resume</>}
              </button>
              {removing === schedule.id ? (
                <>
                  <button className="danger" onClick={() => { setRemoving(null); void act(cloudScheduleRemove(schedule.id)); }}>Delete</button>
                  <button className="secondary" onClick={() => setRemoving(null)}>Keep</button>
                </>
              ) : (
                <button className="secondary" aria-label={`Delete ${schedule.name}`} onClick={() => setRemoving(schedule.id)}><Trash2 size={13} /></button>
              )}
            </div>
          </li>
        ))}
      </ul>
      {creating ? (
        <NewSchedule agents={agents} onClose={() => setCreating(false)} onDone={() => { setCreating(false); onChanged(); }} />
      ) : (
        <div className="knowledge-actions"><button className="secondary" disabled={agents.every((a) => a.status !== "available")} onClick={() => setCreating(true)}>New repeat job</button></div>
      )}
    </section>
  );
}

/** Reports that a schedule made and that wait for their owner's decision. */
export function InboxSection({ items, onOpenTask }: { items: AgentTask[]; onOpenTask: (id: string) => void }) {
  if (items.length === 0) return null;
  return (
    <section aria-label="Waiting for you" className="agent-inbox">
      <h3>Waiting for you <span className="inbox-count">{items.length}</span></h3>
      <ul className="agent-tasks">
        {items.map((task) => (
          <li key={task.id}>
            <button onClick={() => onOpenTask(task.id)}>
              <strong>{task.title}</strong>
              <span>{task.agent_name} · ready for you to read · {dollars(task.cost_micros)}</span>
              <em className="knowledge-badge warn">Review</em>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
