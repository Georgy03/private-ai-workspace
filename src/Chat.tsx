import { useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { wantsEmail, wantsMeeting } from "./intent";
import { AlertTriangle, ChevronRight, ExternalLink, LoaderCircle, PanelLeft, Plus, ArrowUp, Search, PenLine, ReceiptText, Sparkles, Paperclip, Globe, Trash2 } from "lucide-react";
import { answerQuestion, askWeb, attachFile, draftEmail, cloudAgents, cloudTask, cloudTaskStart, removeAttachment } from "./api";
import { ComposerChips, ComposerPlus } from "./ChatTools";
import { Markdown } from "./Markdown";
import { Logo } from "./Logo";
import { Shimmer } from "./Shimmer";
import { ExportButtons } from "./ExportButtons";
import { ReportCard } from "./ReportCard";
import { Canvas } from "./Canvas";
import { ReceiptDialog } from "./ReceiptDialog";
import { EmailPreview } from "./EmailPreview";
import { ShowWork } from "./ShowWork";
import { TypedClaims } from "./Typed";
import { SourcesPanel } from "./SourcesPanel";
import type { Agent, AgentTask, AnswerResponse, AttachedFile, Citation, EmailDraft, DocumentResource, SearchLimits, TaggedFile } from "./types";
import { NO_LIMITS } from "./limits";

export interface Turn {
  id: number;
  question: string;
  answer?: AnswerResponse;
  error?: string;
  advanced: boolean;
  /** The files that went with this question, shown under it. */
  attached?: string[];
  /** The answer has just arrived and is still being written out on screen. */
  fresh?: boolean;
  /** An email written from this request, shown as a preview to send. */
  email?: EmailDraft;
  /** Asked in the web tab (regular Claude) or about the person's documents. */
  mode?: "documents" | "web";
  /** Sent to a helper as a task instead of answered here. */
  task?: { id: string; agent: string; data?: AgentTask };
}

/** When the chat belongs to a workspace: questions look only at its files, and its chats are kept. */
export interface ChatSpace {
  id: string;
  name: string;
  /** Opens the window to add files from this computer to the workspace. */
  onAddFiles: () => void;
  adding: boolean;
  onDeleteThread: (id: number) => void;
}

/** Things to ask about a client or matter. */
const SPACE_PROMPTS = [
  { title: "Where does it stand", text: "Summarize where things stand on this matter, with the key facts and open issues." },
  { title: "Key dates and deadlines", text: "List every key date and deadline in these files." },
  { title: "Who is involved", text: "Who are the people and organizations involved, and what is each one's role?" },
  { title: "What is still missing", text: "What documents or information look like they are missing or need follow-up?" },
  { title: "Draft a status update", text: "Draft a short status update for the client about this matter." },
  { title: "Find a clause or figure", text: "Find what these files say about " },
];

export interface ChatThread {
  id: number;
  title: string;
  startedAt: number;
  turns: Turn[];
}

const PROMPTS = [
  { title: "Find a policy", text: "What is our policy on " },
  { title: "Summarize everything on a client or project", text: "Summarize everything we have on " },
  { title: "Draft a status update", text: "Draft a short status update about " },
  { title: "Pricing and payment terms", text: "What are the pricing and payment terms for " },
  { title: "Key dates and deadlines", text: "List the key dates and deadlines in " },
  { title: "Who is responsible", text: "Who is responsible for " },
];

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const DAY = 86_400_000;

/** "Today", "Past 7 days" or "Older", as in the chat list. */
function groupOf(startedAt: number, now: number): string {
  const startOfToday = new Date(now).setHours(0, 0, 0, 0);
  if (startedAt >= startOfToday) return "Today";
  return startedAt >= now - 7 * DAY ? "Past 7 days" : "Older";
}

/** What a chat remembers of earlier answers, so a follow-up makes sense. */
function earlierOf(thread: ChatThread, onlyWeb = false) {
  return thread.turns
    .filter((turn) => turn.answer && !turn.answer.abstained && (!onlyWeb || turn.mode === "web"))
    .map((turn) => ({ question: turn.question, answer: turn.answer!.claims.map((claim) => claim.text).join("\n"), web: turn.mode === "web" }));
}

/** "Draft an email to ...", "write an email asking ...", "email Sam about ...": a request for an email, not a question. */

/** Things to try in the web tab, for an accounting or law firm. */
const WEB_PROMPTS = [
  { title: "Current tax rules and deadlines", text: "What are the current filing rules and deadlines for " },
  { title: "Research a legal question", text: "Research the law on " },
  { title: "Recent changes", text: "What has changed recently in regulations or tax law for " },
  { title: "Compare software or tools", text: "Compare QuickBooks Online and Xero for " },
  { title: "Draft a client email", text: "Draft a professional email to a client about " },
  { title: "Explain it in plain words", text: "Explain in plain words, for a client, how " },
];

/** An answer is offered in the canvas once it is about this long, or when a file was asked for. */
const CANVAS_WORDS = 90;
const answerText = (answer: AnswerResponse) => answer.claims.map((claim) => claim.text).join("\n\n");
const wordCount = (text: string) => (text.trim() ? text.trim().split(/\s+/).length : 0);

/** A name for a saved file: the first heading of what was written, or the start of the request. */
function fileTitle(text: string, question: string): string {
  const heading = text.match(/^#{1,6}\s+(.+)$/m)?.[1]?.replace(/[*`]/g, "").trim();
  return heading || question.replace(/\s+/g, " ").trim().slice(0, 60) || "Report";
}

/** A helper's work on a task given from the chat: progress while it works, then its report. */
function HelperReply({ turn, onOpenAgents, onOpenLink }: { turn: Turn; onOpenAgents: (taskId?: string) => void; onOpenLink: (url: string) => void }) {
  const task = turn.task!;
  const data = task.data;
  const working = !data || data.status === "queued" || data.status === "running";
  if (working) {
    return <p className="chat-thinking" role="status"><Logo size={24} mark thinking /> <Shimmer>{task.agent} is working on it… you can keep chatting.</Shimmer></p>;
  }
  if (data.status !== "done") {
    return <p className="chat-error" role="alert"><AlertTriangle size={14} /> {data.status === "cancelled" ? `${task.agent}'s task was stopped.` : (data.error ?? `${task.agent} could not finish this.`)}</p>;
  }
  return (
    <div className="assistant-row">
      <div className="assistant-avatar"><Logo size={28} mark /></div>
      <section className="answer-area" aria-label={`${task.agent}'s report`}>
        <p className="helper-label">{task.agent}'s report</p>
        <article className="answer-copy"><Markdown text={data.edited_result ?? data.result ?? ""} /></article>
        <ExportButtons title={data.title ?? `${task.agent}'s report`} text={data.edited_result ?? data.result ?? ""} />
        <ReportCard compact task={data} onOpenLink={onOpenLink} />
        <p className="chat-helper-note">If {task.agent} prepared an email or a meeting, it is waiting for you in Agents. <button className="link-button" onClick={() => onOpenAgents(task.id)}>Open the full report</button></p>
      </section>
    </div>
  );
}

export function ChatView({
  onCitation,
  onOpenLink,
  onAnswered,
  onOpenAgents,
  hasDocuments,
  documents,
  cloudReady,
  threads,
  setThreads,
  activeId,
  setActiveId,
  pending,
  onPendingUsed,
  space,
}: {
  space?: ChatSpace;
  onCitation: (citation: Citation) => void;
  onOpenLink: (url: string) => void;
  onAnswered: (answer: AnswerResponse) => void;
  /** Go to the Agents page, with a task open when one is named. */
  onOpenAgents: (taskId?: string) => void;
  hasDocuments: boolean;
  /** Files on this device, for tagging one. */
  documents: DocumentResource[];
  cloudReady: boolean;
  /** The chats, kept by the app so they survive going to another page and back. */
  threads: ChatThread[];
  setThreads: Dispatch<SetStateAction<ChatThread[]>>;
  activeId: number | null;
  setActiveId: (id: number | null) => void;
  /** A question typed on the Home page, to be asked as soon as the chat opens. */
  pending: { id: number; text: string; attachments: AttachedFile[]; limits: SearchLimits } | null;
  onPendingUsed: () => void;
}) {
  const [listOpen, setListOpen] = useState(true);
  const [input, setInput] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [asking, setAsking] = useState(false);
  // The helpers this person can give a task to, and where the next message goes: "chat" or a helper.
  const [helpers, setHelpers] = useState<Agent[]>([]);
  // What the next questions include or are limited to: files attached from this computer, one tagged
  // file, and a limit on where to look. They stay until taken off.
  const [attachments, setAttachments] = useState<AttachedFile[]>([]);
  const [tagged, setTagged] = useState<TaggedFile | null>(null);
  const [limits, setLimits] = useState<SearchLimits>(NO_LIMITS);
  const [toolProblem, setToolProblem] = useState<string | null>(null);
  // Documents: answers drawn from the person's files. Web: regular Claude, for research or any question.
  const [mode, setMode] = useState<"documents" | "web">("documents");
  const [attaching, setAttaching] = useState(false);
  // The answer whose sources are open beside the chat.
  const [sourcesOf, setSourcesOf] = useState<number | null>(null);
  const [sourcesWide, setSourcesWide] = useState(false);
  const [receiptOf, setReceiptOf] = useState<AnswerResponse | null>(null);
  // The answer open in the canvas, and the person's own edits of each answer.
  const [canvasOf, setCanvasOf] = useState<number | null>(null);
  const [edits, setEdits] = useState<Record<number, string>>({});
  const endRef = useRef<HTMLDivElement | null>(null);
  const box = useRef<HTMLTextAreaElement | null>(null);
  const active = threads.find((thread) => thread.id === activeId) ?? null;
  const openSources = active?.turns.find((turn) => turn.id === sourcesOf)?.answer?.citations ?? null;
  const canvasTurn = active?.turns.find((turn) => turn.id === canvasOf && turn.answer) ?? null;
  const canvasOriginal = canvasTurn?.answer ? answerText(canvasTurn.answer) : "";

  useEffect(() => {
    let alive = true;
    void cloudAgents().then(
      (list) => { if (alive) setHelpers(list.agents.filter((agent) => agent.status === "available")); },
      () => undefined,
    );
    return () => { alive = false; };
  }, []);

  // Tasks given to helpers are checked on while they work, and the report appears in the chat.
  const unfinished = threads.flatMap((thread) => thread.turns.filter((turn) => turn.task && (!turn.task.data || ["queued", "running"].includes(turn.task.data.status))).map((turn) => turn.task!.id));
  const unfinishedKey = unfinished.join(",");
  useEffect(() => {
    if (!unfinishedKey) return;
    let alive = true;
    const check = () =>
      unfinishedKey.split(",").forEach((id) =>
        void cloudTask(id).then(
          (data) => {
            if (!alive) return;
            setThreads((all) => all.map((thread) => ({ ...thread, turns: thread.turns.map((turn) => (turn.task?.id === id ? { ...turn, task: { ...turn.task, data } } : turn)) })));
          },
          () => undefined,
        ),
      );
    const timer = window.setInterval(check, 2500);
    return () => { alive = false; window.clearInterval(timer); };
  }, [unfinishedKey, setThreads]);

  useEffect(() => {
    endRef.current?.scrollIntoView?.({ behavior: "smooth", block: "end" });
  }, [active?.turns.length, asking]);

  /** "@John write to the client" goes to John. */
  const route = (typed: string): { helper: Agent | null; instruction: string } => {
    const mentioned = [...helpers]
      .sort((a, b) => b.name.length - a.name.length)
      .find((agent) => typed.toLowerCase().startsWith(`@${agent.name.toLowerCase()} `) || typed.toLowerCase() === `@${agent.name.toLowerCase()}`);
    if (mentioned) return { helper: mentioned, instruction: typed.slice(mentioned.name.length + 1).trim() };
    return { helper: null, instruction: typed.trim() };
  };

  const send = async (text: string, useAdvanced = advanced, extra?: { attachments: AttachedFile[]; limits: SearchLimits; mode?: "documents" | "web" }) => {
    const asking_mode = extra?.mode ?? mode;
    const { helper, instruction } = asking_mode === "web" ? { helper: null, instruction: text.trim() } : route(text.trim());
    const question = helper ? instruction : text.trim();
    if (question.length < 3 || asking) return;
    const now = Date.now();
    const sentFiles = (extra ? extra.attachments : attachments).map((file) => file.name);
    const turn: Turn = { id: now, question: text.trim(), advanced: useAdvanced, mode: asking_mode, attached: asking_mode === "web" ? sentFiles : [...sentFiles, ...(tagged ? [tagged.name] : [])] };
    let thread = active;
    const earlier = thread ? earlierOf(thread, asking_mode === "web") : [];
    if (!thread) {
      thread = { id: now, title: question.slice(0, 60), startedAt: now, turns: [] };
      setThreads((all) => [thread!, ...all]);
      setActiveId(thread.id);
    }
    const threadId = thread.id;
    const update = (change: (turns: Turn[]) => Turn[]) =>
      setThreads((all) => all.map((item) => (item.id === threadId ? { ...item, turns: change(item.turns) } : item)));
    update((turns) => [...turns, turn]);
    setInput("");
    if (helper) {
      // A task for a helper: it works in the background and its report appears here.
      try {
        const id = await cloudTaskStart(helper.id, question);
        // Look straight away, so a task that is already done does not wait for the next check.
        const data = await cloudTask(id).catch(() => undefined);
        update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, task: { id, agent: helper.name, data } } : t)));
      } catch (caught) {
        update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, error: message(caught) } : t)));
      }
      return;
    }
    if (wantsEmail(question) || wantsMeeting(question)) {
      // An email is written for the person to read and send, not answered from documents.
      setAsking(true);
      try {
        const email = await draftEmail(question);
        update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, email } : t)));
      } catch (caught) {
        update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, error: message(caught) } : t)));
      } finally {
        setAsking(false);
      }
      return;
    }
    setAsking(true);
    try {
      const answer = asking_mode === "web" ? await askWeb(question, useAdvanced, earlier, (extra ? extra.attachments : attachments).map((file) => file.id)) : await answerQuestion(question, useAdvanced, earlier, undefined, space ? { workspace: space.id } : { attachments: (extra ? extra.attachments : attachments).map((file) => file.id), tagged, ...((extra ? extra.limits : limits)) });
      update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, answer, fresh: !answer.abstained && answer.claims.length > 0 } : t)));
      onAnswered(answer);
    } catch (caught) {
      update((turns) => turns.map((t) => (t.id === turn.id ? { ...t, error: message(caught) } : t)));
    } finally {
      setAsking(false);
    }
  };

  // A question typed on the Home page is asked once, as soon as the chat is showing.
  const sendNow = useRef(send);
  useEffect(() => {
    sendNow.current = send;
  });
  const pendingId = pending?.id;
  const pendingText = pending?.text;
  const pendingFiles = pending?.attachments;
  const pendingLimits = pending?.limits ?? NO_LIMITS;
  const handled = useRef<number | null>(null);
  useEffect(() => {
    if (pendingId === undefined || pendingText === undefined) return;
    // Once only, even when the page runs its effects twice while developing.
    if (handled.current === pendingId) return;
    handled.current = pendingId;
    onPendingUsed();
    const files = pendingFiles ?? [];
    void sendNow.current(pendingText, undefined, { attachments: files, limits: pendingLimits, mode: "documents" });
    // What was attached or limited on Home stays on the chat's message box for the next question.
    void Promise.resolve().then(() => {
      setAttachments(files);
      setLimits(pendingLimits);
      setMode("documents");
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingId]);

  /** The answer has been written out in full: the rest of what belongs under it can appear. */
  const finishTyping = (turnId: number) =>
    setThreads((all) => all.map((thread) => ({ ...thread, turns: thread.turns.map((turn) => (turn.id === turnId && turn.fresh ? { ...turn, fresh: false } : turn)) })));
  const keepEndInView = () => endRef.current?.scrollIntoView?.({ block: "end" });

  const attach = () => {
    setToolProblem(null);
    setAttaching(true);
    attachFile().then(
      (file) => { setAttaching(false); if (file) setAttachments((all) => [...all, file]); },
      (caught) => { setAttaching(false); setToolProblem(message(caught)); },
    );
  };
  const detach = (id: string) => {
    setAttachments((all) => all.filter((file) => file.id !== id));
    void removeAttachment(id).catch(() => undefined);
  };

  const newChat = () => {
    setActiveId(null);
    setInput("");
    box.current?.focus();
  };

  const now = Date.now();
  const groups = ["Today", "Past 7 days", "Older"].map((label) => ({ label, items: threads.filter((thread) => groupOf(thread.startedAt, now) === label) })).filter((group) => group.items.length > 0);

  const composer = (
    <form className="chat-composer" onSubmit={(event) => { event.preventDefault(); void send(input); }}>
      <ComposerChips attachments={attachments} attaching={attaching} tagged={mode === "documents" ? tagged : null} limits={mode === "documents" ? limits : NO_LIMITS} onRemoveAttachment={detach} onRemoveTag={() => setTagged(null)} onRemoveScope={() => setLimits(NO_LIMITS)} />
      <textarea
        ref={box}
        aria-label="Message"
        rows={2}
        value={input}
        placeholder={mode === "web" ? "Ask anything, or research the web…" : space ? `Ask about ${space.name}…` : hasDocuments ? "Ask about your documents…" : "Ask about your connected sources…"}
        onChange={(event) => setInput(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void send(input);
          }
        }}
      />
      {toolProblem && <p className="chat-error composer-problem" role="alert">{toolProblem}</p>}
      <div className="chat-composer-row">
        {mode === "documents" && space && (
          <button type="button" className="plus-button" aria-label="Add files to this workspace" title={`Add files from this computer to ${space.name}`} disabled={space.adding} onClick={space.onAddFiles}>
            {space.adding ? <LoaderCircle size={17} className="spin" /> : <Paperclip size={18} />}
          </button>
        )}
        {mode === "documents" && !space && <ComposerPlus documents={documents} cloudReady={cloudReady} attachCount={attachments.length} maxAttachments={3} limits={limits} onAttach={attach} onTag={setTagged} onLimits={setLimits} />}
        {mode === "web" && (
          <button type="button" className="plus-button" aria-label="Attach file" title={attachments.length >= 3 ? "Up to 3 files" : "Attach a file from this computer"} disabled={attachments.length >= 3} onClick={attach}><Paperclip size={18} /></button>
        )}
        <button type="button" className={`web-toggle ${mode === "web" ? "on" : ""}`} aria-pressed={mode === "web"} title={mode === "web" ? "Web is on. Click to go back to your documents." : "Turn on the web, for research or any question"} onClick={() => setMode(mode === "web" ? "documents" : "web")}>
          <Globe size={16} /> Web
        </button>
        {route(input).helper === null && (
          <label className="chat-model">
            <span className="visually-hidden">Analysis</span>
            <select aria-label="Analysis" value={advanced ? "advanced" : "standard"} onChange={(event) => setAdvanced(event.target.value === "advanced")}>
              <option value="standard">Standard</option>
              <option value="advanced">Advanced analysis</option>
            </select>
          </label>
        )}
        {route(input).helper !== null && <small>{route(input).helper!.name} will work on this in the background and write a report here. You can keep chatting.</small>}
        {route(input).helper === null && advanced && <small>Advanced analysis uses about 20× more of your monthly allowance.</small>}
        <button className="chat-send" type="submit" aria-label="Send" disabled={asking || input.trim().length < 3}>
          {asking ? <LoaderCircle size={17} className="spin" /> : <ArrowUp size={16} />}
        </button>
      </div>
    </form>
  );

  return (
    <div className={`chat-view ${listOpen ? "" : "list-collapsed"} ${openSources || canvasTurn ? "with-sources" : ""} ${(openSources || canvasTurn) && sourcesWide ? "sources-wide" : ""}`}>
      {listOpen && <aside className="chat-list" aria-label="Chats">
        <div className="chat-list-head">
          <h2>Chats</h2>
          <button className="icon-button" onClick={() => setListOpen(false)} aria-label="Hide the list of chats" title="Hide the list of chats"><PanelLeft size={16} /></button>
        </div>
        <button className="chat-new" onClick={newChat}><Plus size={15} /> New Chat</button>
        {groups.map((group) => (
          <div key={group.label}>
            <h3>{group.label}</h3>
            {group.items.map((thread) => (
              <div key={thread.id} className={`chat-item-row ${thread.id === activeId ? "active" : ""}`}>
                <button className={`chat-item ${thread.id === activeId ? "active" : ""}`} onClick={() => setActiveId(thread.id)} title={thread.title}>
                  {thread.title}
                </button>
                {space && <button className="chat-item-delete" aria-label={`Delete the chat ${thread.title}`} title="Delete this chat" onClick={() => space.onDeleteThread(thread.id)}><Trash2 size={13} /></button>}
              </div>
            ))}
          </div>
        ))}
        <p className="chat-note">{space ? "Chats in this workspace are saved, encrypted, on this computer." : "Chats are kept only until you close the app."}</p>
      </aside>}

      <section className="chat-main">
        {!listOpen && <button className="icon-button chat-list-open" onClick={() => setListOpen(true)} aria-label="Show the list of chats" title="Show the list of chats"><PanelLeft size={16} /></button>}
        {!active ? (
          <div className="chat-empty">
            <h1>{mode === "web" ? "Ask anything" : space ? `Ask about ${space.name}` : "Ask about your documents"}</h1>
            <p>{mode === "web" ? "Research the web or ask any question. Your documents are not used here, but you can turn the web off afterwards and ask about them: this conversation will be remembered." : space ? "Answers come only from the files in this workspace, and cite them. Add more files any time with the paperclip." : "Try a prompt below to get started. Answers cite the files they came from, and you only see files you may open."}</p>
            <div className="chat-tabs" role="tablist"><button role="tab" aria-selected="true" className="active">Recommended</button></div>
            <div className="prompt-grid">
              {(mode === "web" ? WEB_PROMPTS : space ? SPACE_PROMPTS : PROMPTS).map((prompt) => (
                <button key={prompt.title} className="prompt-card" onClick={() => { setInput(prompt.text); box.current?.focus(); }}>
                  <strong>{prompt.title}</strong>
                  <span>{prompt.text.trim()}…</span>
                </button>
              ))}
            </div>
            {composer}
          </div>
        ) : (
          <>
            <div className="chat-thread" aria-label="Conversation">
              {active.turns.map((turn) => (
                <div key={turn.id}>
                  <div className="user-row"><div className="user-bubble">
                    {turn.question}
                    {turn.attached && turn.attached.length > 0 && (
                      <ul className="bubble-files" aria-label="Sent with this question">
                        {turn.attached.map((name, index) => <li key={index}><Paperclip size={12} /> {name}</li>)}
                      </ul>
                    )}
                  </div></div>
                  {turn.task && <HelperReply turn={turn} onOpenAgents={onOpenAgents} onOpenLink={onOpenLink} />}
                  {!turn.task && !turn.answer && !turn.error && !turn.email && <p className="chat-thinking" role="status"><Logo size={24} mark thinking /> <Shimmer>Searching your sources…</Shimmer></p>}
                  {turn.email && (
                    <div className="assistant-row">
                      <div className="assistant-avatar"><Logo size={28} mark /></div>
                      <section className="answer-area"><EmailPreview draft={turn.email} /></section>
                    </div>
                  )}
                  {turn.error && <p className="chat-error" role="alert"><AlertTriangle size={14} /> {turn.error}</p>}
                  {turn.answer && (
                    <div className="assistant-row">
                      <div className="assistant-avatar"><Logo size={28} mark /></div>
                      <section className="answer-area">
                        <div className="answer-tools">
                          <ShowWork steps={turn.answer.work ?? []} />
                          <button className="receipt-link" onClick={() => setReceiptOf(turn.answer!)}><ReceiptText size={13} /> Privacy receipt</button>
                        </div>
                        {turn.answer.notice && <p className="cloud-note"><AlertTriangle size={14} /> {turn.answer.notice}</p>}
                        {turn.answer.abstained ? (
                          <>
                            <p className="abstention">I couldn’t find enough matching information to answer safely. Try a specific file name, client, clause, or identifier.</p>
                            {!turn.advanced && (
                              <button className="retry-advanced" disabled={asking} onClick={() => void send(turn.question, true)}>
                                <Sparkles size={14} /> Try again with advanced analysis <small>uses about 20× more of your allowance</small>
                              </button>
                            )}
                          </>
                        ) : (
                          <TypedClaims texts={turn.answer.claims.map((claim) => claim.text)} animate={turn.fresh === true} onDone={() => finishTyping(turn.id)} onGrow={keepEndInView} />
                        )}
                        {!turn.fresh && !turn.answer.abstained && (turn.answer.file || wordCount(answerText(turn.answer)) >= CANVAS_WORDS) && (
                          <button className="canvas-open" aria-pressed={canvasOf === turn.id} onClick={() => { setSourcesOf(null); setCanvasOf(canvasOf === turn.id ? null : turn.id); }}>
                            <PenLine size={13} /> Open in canvas
                          </button>
                        )}
                        {!turn.fresh && turn.answer.file && !turn.answer.abstained && (
                          <ExportButtons title={fileTitle(turn.answer.claims.map((claim) => claim.text).join("\n\n"), turn.question)} text={turn.answer.claims.map((claim) => claim.text).join("\n\n")} first={turn.answer.file} />
                        )}
                        {!turn.fresh && turn.answer.citations.length > 0 && (
                          <div className="chat-sources" aria-label="Documents used for this answer">
                            <button className="sources-toggle" aria-pressed={sourcesOf === turn.id} onClick={() => { setCanvasOf(null); setSourcesOf(sourcesOf === turn.id ? null : turn.id); }}>
                              <Search size={13} /> Sources <b>{turn.answer.citations.length}</b>
                            </button>
                            {turn.answer.citations.map((citation, index) => (
                              <span className="chat-source" key={citation.id}>
                                <button onClick={() => onCitation(citation)}>
                                  <b>{index + 1}</b> {citation.document_name} <ChevronRight size={13} />
                                </button>
                                {citation.link && (
                                  <button className="open-link" aria-label={`Open ${citation.document_name}`} onClick={() => onOpenLink(citation.link!)}><ExternalLink size={12} /></button>
                                )}
                              </span>
                            ))}
                          </div>
                        )}
                      </section>
                    </div>
                  )}
                </div>
              ))}
              <div ref={endRef} />
            </div>
            <div className="chat-dock">
              {composer}
              <p className="dock-note">Answers can be wrong. Check the cited sources before relying on them.</p>
            </div>
          </>
        )}
      </section>
      {receiptOf && <ReceiptDialog answer={receiptOf} onClose={() => setReceiptOf(null)} />}
      {canvasTurn && canvasTurn.answer && (
        <Canvas
          title={fileTitle(canvasOriginal, canvasTurn.question)}
          original={canvasOriginal}
          text={edits[canvasTurn.id] ?? canvasOriginal}
          expanded={sourcesWide}
          first={canvasTurn.answer.file}
          onChange={(text) => setEdits((all) => ({ ...all, [canvasTurn.id]: text }))}
          onExpand={() => setSourcesWide((wide) => !wide)}
          onClose={() => setCanvasOf(null)}
          onOpenLink={onOpenLink}
        />
      )}
      {openSources && !canvasTurn && <SourcesPanel citations={openSources} expanded={sourcesWide} onExpand={() => setSourcesWide((wide) => !wide)} onCitation={onCitation} onOpenLink={onOpenLink} onClose={() => setSourcesOf(null)} />}
    </div>
  );
}
