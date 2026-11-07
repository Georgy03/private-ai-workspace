import { useState } from "react";
import { Clock } from "lucide-react";
import { cloudTaskKeep, cloudTaskLetExpire } from "./api";
import type { AgentTask } from "./types";

const day = (epoch: number) => new Date(epoch * 1000).toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" });
const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

/**
 * Asks whether to keep old reports before they are deleted. Each answer is final for that report.
 * "Ask me later" closes the box without deciding, so it comes back the next time the app opens.
 */
export function RetentionPrompt({ items, onDone }: { items: AgentTask[]; onDone: () => void }) {
  const [left, setLeft] = useState(items);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const decide = (ids: string[], keep: boolean) => {
    setBusy(true);
    Promise.all(ids.map((id) => (keep ? cloudTaskKeep(id) : cloudTaskLetExpire(id)))).then(
      () => {
        setBusy(false);
        const remaining = left.filter((task) => !ids.includes(task.id));
        setLeft(remaining);
        if (remaining.length === 0) onDone();
      },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  if (left.length === 0) return null;
  return (
    <div className="agent-modal" role="dialog" aria-modal="true" aria-label="Keep your old reports?">
      <div className="agent-modal-body">
        <section className="agent-panel retention-prompt">
          <header><div className="ask-head"><Clock size={26} /><h2>Keep your old reports?</h2></div></header>
          <p>{left.length === 1 ? "This report and the conversation behind it" : "These reports and the conversations behind them"} will be deleted soon, so your firm does not keep old work forever. Would you like to keep {left.length === 1 ? "it" : "them"}?</p>
          {problem && <p className="knowledge-error" role="alert">{problem}</p>}
          <ul className="retention-list">
            {left.map((task) => (
              <li key={task.id}>
                <div>
                  <strong>{task.title}</strong>
                  <span>{task.agent_name}{task.deletes_epoch ? ` · deleted on ${day(task.deletes_epoch)}` : ""}</span>
                </div>
                <button className="primary" disabled={busy} onClick={() => decide([task.id], true)}>Keep it</button>
                <button className="secondary" disabled={busy} onClick={() => decide([task.id], false)}>Let it be deleted</button>
              </li>
            ))}
          </ul>
          <div className="knowledge-actions">
            {left.length > 1 && <button className="secondary" disabled={busy} onClick={() => decide(left.map((t) => t.id), true)}>Keep all</button>}
            <button className="ghost" onClick={onDone}>Ask me later</button>
          </div>
          <p className="knowledge-note">Kept reports stay until you delete them yourself. Only you can see them.</p>
        </section>
      </div>
    </div>
  );
}
