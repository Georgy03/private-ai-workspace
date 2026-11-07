import { useState } from "react";
import { ArrowUp, ChartColumn, ExternalLink, FileSearch, MessageCircle } from "lucide-react";
import { answerQuestion } from "./api";
import { Logo } from "./Logo";
import { Markdown } from "./Markdown";
import { countUnder, FINDINGS, RECOMMENDATIONS, summaryOf } from "./reportSummary";
import { Shimmer } from "./Shimmer";
import type { AgentTask, AnswerResponse, ReportChecks } from "./types";

/** How the report held up against the documents it read, with the details of what was not found. */
export function Checks({ checks }: { checks: ReportChecks }) {
  const missing = checks.figures_missing.length + checks.quotes_missing.length;
  return (
    <section className={`agent-checks ${missing > 0 ? "warn" : "ok"}`} aria-label="Report check">
      <strong>{missing === 0 ? "Checked against the documents it read" : "Check these before relying on this report"}</strong>
      <p>
        {checks.figures_total > 0 && <>{checks.figures_found} of {checks.figures_total} figures appear in the documents it read or in its own calculations. </>}
        {checks.quotes_total > 0 && <>{checks.quotes_found} of {checks.quotes_total} quotations match word for word.</>}
      </p>
      {checks.figures_missing.length > 0 && (
        <p>Not found in the documents or calculations: {checks.figures_missing.join(", ")}</p>
      )}
      {checks.quotes_missing.map((quote) => <p key={quote}>Quotation not found: “{quote}”</p>)}
    </section>
  );
}

const SUGGESTIONS = [
  "What is the main point of this report?",
  "Which figures should I double-check?",
  "Where does the biggest number come from?",
];

type Turn = { id: number; question: string; answer?: AnswerResponse; error?: string };

/**
 * A finished report as something to interrogate: what it says, whether it held up, and two ways in.
 * "Ask about report" answers from the report and the files it was drawn from, never from anything
 * else; "View evidence" shows the check and the files themselves. With `compact`, only the counts,
 * the buttons and what they open are shown, for places where the report itself is already on screen.
 */
export function ReportCard({ task, onOpenLink, compact = false }: { task: AgentTask; onOpenLink: (url: string) => void; compact?: boolean }) {
  const [panel, setPanel] = useState<"ask" | "evidence" | null>(null);
  const text = task.edited_result ?? task.result ?? "";
  if (!text) return null;
  const sources = task.sources ?? [];
  const findings = countUnder(text, FINDINGS);
  const recommendations = countUnder(text, RECOMMENDATIONS);
  const checks = task.checks && (task.checks.figures_total > 0 || task.checks.quotes_total > 0) ? task.checks : null;
  const unchecked = checks ? checks.figures_missing.length + checks.quotes_missing.length : 0;
  const toggle = (next: "ask" | "evidence") => setPanel((current) => (current === next ? null : next));
  return (
    <section className={`report-card ${compact ? "compact" : ""}`} aria-label="Report">
      {!compact && (
        <>
          <header className="report-card-head">
            <ChartColumn size={22} aria-hidden="true" />
            <div>
              <h3>{task.title}</h3>
              <p>Created by {task.agent_name}</p>
            </div>
            {checks && <span className={`check-badge ${unchecked === 0 ? "ok" : "warn"}`}>{unchecked === 0 ? "Checks passed" : `${unchecked} to check`}</span>}
          </header>
          <div className="report-card-summary">
            <h4>Summary</h4>
            <p>{summaryOf(text)}</p>
          </div>
        </>
      )}
      <ul className="report-chips" aria-label="What the report is built on">
        <li>{sources.length} source{sources.length === 1 ? "" : "s"}</li>
        {findings > 0 && <li>{findings} finding{findings === 1 ? "" : "s"}</li>}
        {recommendations > 0 && <li>{recommendations} recommendation{recommendations === 1 ? "" : "s"}</li>}
        {compact && checks && <li className={unchecked === 0 ? "ok" : "warn"}>{unchecked === 0 ? "Checks passed" : `${unchecked} to check`}</li>}
      </ul>
      <div className="report-actions">
        <button className="report-action primary" aria-expanded={panel === "ask"} onClick={() => toggle("ask")}><MessageCircle size={16} /> Ask about report</button>
        <button className="report-action" aria-expanded={panel === "evidence"} onClick={() => toggle("evidence")}><FileSearch size={16} /> View evidence</button>
      </div>
      {panel === "ask" && <AskPanel task={task} onOpenLink={onOpenLink} />}
      {panel === "evidence" && (
        <section className="report-evidence" aria-label="Evidence">
          {checks ? <Checks checks={checks} /> : <p className="knowledge-note">No figures or quotations were checked for this report.</p>}
          {sources.length > 0 ? (
            <ul className="agent-sources" aria-label="Documents used">
              {sources.map((source) => (
                <li key={source.document_id}>
                  {source.name}
                  {source.link && (
                    <button className="open-link" onClick={() => onOpenLink(source.link!)} aria-label={`Open ${source.name}`}><ExternalLink size={12} /> Open</button>
                  )}
                </li>
              ))}
            </ul>
          ) : (
            <p className="knowledge-note">This report was not drawn from any documents.</p>
          )}
        </section>
      )}
    </section>
  );
}

function AskPanel({ task, onOpenLink }: { task: AgentTask; onOpenLink: (url: string) => void }) {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState("");
  const busy = turns.some((turn) => !turn.answer && !turn.error);

  const ask = (question: string) => {
    const trimmed = question.trim();
    if (trimmed.length < 3 || busy) return;
    const id = turns.length + 1;
    const earlier = turns
      .filter((turn) => turn.answer && !turn.answer.abstained)
      .map((turn) => ({ question: turn.question, answer: turn.answer!.claims.map((claim) => claim.text).join(" ") }));
    setTurns((all) => [...all, { id, question: trimmed }]);
    setInput("");
    answerQuestion(trimmed, false, earlier, task.id).then(
      (answer) => setTurns((all) => all.map((turn) => (turn.id === id ? { ...turn, answer } : turn))),
      (caught) => setTurns((all) => all.map((turn) => (turn.id === id ? { ...turn, error: caught instanceof Error ? caught.message : String(caught) } : turn))),
    );
  };

  return (
    <section className="report-ask" aria-label="Ask about this report">
      {turns.length === 0 && (
        <div className="report-suggestions">
          {SUGGESTIONS.map((suggestion) => <button key={suggestion} className="chip" onClick={() => ask(suggestion)}>{suggestion}</button>)}
        </div>
      )}
      <div className="report-thread">
        {turns.map((turn) => (
          <div key={turn.id}>
            <div className="user-row"><div className="user-bubble">{turn.question}</div></div>
            {!turn.answer && !turn.error && (
              <p className="chat-thinking" role="status"><Logo size={22} mark thinking /> <Shimmer>Reading the report and its evidence…</Shimmer></p>
            )}
            {turn.error && <p className="chat-error" role="alert">{turn.error}</p>}
            {turn.answer && (
              <div className="assistant-row">
                <div className="assistant-avatar"><Logo size={28} mark /></div>
                <section className="answer-area">
                  {turn.answer.abstained ? (
                    <p className="abstention">The report and the files it used do not say. Try asking about something the report covers.</p>
                  ) : (
                    turn.answer.claims.map((claim, index) => <article className="answer-copy" key={index}><Markdown text={claim.text} /></article>)
                  )}
                  {!turn.answer.abstained && turn.answer.citations.length > 0 && (
                    <div className="chat-sources" aria-label="Where this answer comes from">
                      {turn.answer.citations.map((citation) => (
                        <span className="chat-source" key={citation.id}>
                          <span className="chat-source-name">{citation.id === "report" ? "The report" : citation.document_name}</span>
                          {citation.link && <button className="open-link" aria-label={`Open ${citation.document_name}`} onClick={() => onOpenLink(citation.link!)}><ExternalLink size={12} /></button>}
                        </span>
                      ))}
                    </div>
                  )}
                </section>
              </div>
            )}
          </div>
        ))}
      </div>
      <form className="report-ask-form" onSubmit={(event) => { event.preventDefault(); ask(input); }}>
        <textarea aria-label="Question about this report" placeholder="Ask about this report…" rows={2} value={input} onChange={(event) => setInput(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); ask(input); } }} />
        <button className="chat-send" type="submit" aria-label="Ask" disabled={busy || input.trim().length < 3}><ArrowUp size={16} /></button>
      </form>
      <p className="dock-note">Answers come only from this report and the files it used. They are not saved.</p>
    </section>
  );
}
