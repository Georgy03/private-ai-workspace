import { ArrowLeft, LoaderCircle, MessageSquareText, Pencil, Plus, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { spaceAddCloud, spaceAddFiles, spaceAddFolder, spaceChat, spaceChats, spaceDelete, spaceDeleteChat, spaceFiles, spaceRemoveFile, spaceSaveChat, spaces, spaceUpdate } from "./api";
import { ChatView, type ChatThread } from "./Chat";
import { SpaceFiles } from "./SpaceFiles";
import { formatBytes, parseChat, serializeChat, savedTurns } from "./spaceText";
import type { AnswerResponse, BrowseRow, Citation, CloudStatus, SpaceAddResult, SpaceFile, SpaceSummary } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

/** Change the name or the line about a workspace. */
function EditSpace({ space, onSaved, onClose }: { space: SpaceSummary; onSaved: (space: SpaceSummary) => void; onClose: () => void }) {
  const [name, setName] = useState(space.name);
  const [about, setAbout] = useState(space.description);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <form className="space-dialog" role="dialog" aria-label="Edit workspace" onSubmit={(event) => {
        event.preventDefault();
        setBusy(true);
        setProblem(null);
        spaceUpdate(space.id, name, about).then(onSaved, (caught) => { setBusy(false); setProblem(message(caught)); });
      }}>
        <header><h2>Edit workspace</h2><button type="button" className="icon-button" aria-label="Close" onClick={onClose}><X size={16} /></button></header>
        <label>Name<input autoFocus aria-label="Workspace name" value={name} maxLength={80} onChange={(event) => setName(event.target.value)} /></label>
        <label>What it is for<textarea aria-label="What it is for" rows={3} value={about} maxLength={500} onChange={(event) => setAbout(event.target.value)} /></label>
        {problem && <p className="chat-error" role="alert">{problem}</p>}
        <div className="space-dialog-actions">
          <button type="button" className="secondary" onClick={onClose}>Cancel</button>
          <button type="submit" className="primary" disabled={busy || !name.trim()}>Save</button>
        </div>
      </form>
    </div>
  );
}

/** One workspace: its chat, with history kept, and its files, in one place. */
export function SpaceView({ id, cloud, onBack, onGone, onCitation, onOpenLink, onOpenAgents, onAnswered }: {
  id: string;
  cloud: CloudStatus;
  onBack: () => void;
  /** The workspace was deleted. */
  onGone: () => void;
  onCitation: (citation: Citation) => void;
  onOpenLink: (url: string) => void;
  onOpenAgents: (taskId?: string) => void;
  onAnswered: (answer: AnswerResponse) => void;
}) {
  const [summary, setSummary] = useState<SpaceSummary | null>(null);
  const [files, setFiles] = useState<SpaceFile[]>([]);
  const [tab, setTab] = useState<"chat" | "files">("chat");
  const [threads, setThreads] = useState<ChatThread[]>([]);
  const [activeId, setActiveIdState] = useState<number | null>(null);
  const [ready, setReady] = useState(false);
  const [adding, setAdding] = useState(false);
  const [notice, setNotice] = useState<{ text: string; problems: { name: string; reason: string }[] } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // What was last written for each chat, so only chats that changed are saved again.
  const savedBodies = useRef(new Map<string, string>());
  const opened = useRef(new Set<number>());
  const latest = useRef<ChatThread[]>([]);

  const reload = useCallback(
    () => Promise.all([spaces(), spaceFiles(id)]).then(([all, list]) => { setSummary(all.find((item) => item.id === id) ?? null); setFiles(list); }),
    [id],
  );

  useEffect(() => {
    let alive = true;
    void Promise.all([reload(), spaceChats(id)]).then(
      ([, chats]) => {
        if (!alive) return;
        setThreads(chats.map((chat) => ({ id: Number(chat.id), title: chat.title, startedAt: chat.updated_at * 1000, turns: [] })));
        setReady(true);
      },
      (caught) => { if (alive) { setProblem(message(caught)); setReady(true); } },
    );
    return () => { alive = false; };
  }, [id, reload]);

  // A saved chat is read when it is opened, not all at once.
  const setActiveId = (threadId: number | null) => {
    setActiveIdState(threadId);
    if (threadId === null || opened.current.has(threadId)) return;
    opened.current.add(threadId);
    spaceChat(id, String(threadId)).then((body) => {
      if (!body) return;
      const turns = parseChat(body);
      savedBodies.current.set(String(threadId), serializeChat(turns));
      setThreads((all) => all.map((thread) => (thread.id === threadId && thread.turns.length === 0 ? { ...thread, turns } : thread)));
    }, (caught) => setProblem(message(caught)));
  };

  // Chats are saved a moment after they change, and once more when the page is left.
  const saveAll = useCallback(() => {
    for (const thread of latest.current) {
      const kept = savedTurns(thread.turns);
      if (kept.length === 0) continue;
      const body = serializeChat(thread.turns);
      if (savedBodies.current.get(String(thread.id)) === body) continue;
      savedBodies.current.set(String(thread.id), body);
      spaceSaveChat(id, String(thread.id), thread.title, body).then(
        () => void reload(),
        (caught) => { savedBodies.current.delete(String(thread.id)); setProblem(message(caught)); },
      );
    }
  }, [id, reload]);
  useEffect(() => {
    latest.current = threads;
    if (!ready) return;
    const timer = window.setTimeout(saveAll, 500);
    return () => window.clearTimeout(timer);
  }, [threads, ready, saveAll]);
  useEffect(() => () => saveAll(), [saveAll]);

  const deleteChat = (threadId: number) => {
    spaceDeleteChat(id, String(threadId)).then(
      () => {
        savedBodies.current.delete(String(threadId));
        setThreads((all) => all.filter((thread) => thread.id !== threadId));
        setActiveIdState((current) => (current === threadId ? null : current));
        void reload();
      },
      (caught) => setProblem(message(caught)),
    );
  };

  const finished = (result: SpaceAddResult | null) => {
    setAdding(false);
    if (!result) return;
    setNotice({ text: result.added > 0 ? `Added ${result.added} file${result.added === 1 ? "" : "s"} to ${summary?.name ?? "the workspace"}.` : "Nothing was added.", problems: result.failed });
    void reload();
  };
  const add = (pick: typeof spaceAddFiles) => {
    setAdding(true);
    setProblem(null);
    setNotice(null);
    pick(id).then(finished, (caught) => { setAdding(false); setProblem(message(caught)); });
  };
  const pin = (row: BrowseRow) =>
    spaceAddCloud(id, row.document_id, row.name, row.source).then(() => reload());
  const remove = (file: SpaceFile) => {
    setNotice(null);
    spaceRemoveFile(id, file.kind, file.id).then(
      (forgotten) => { setNotice({ text: file.kind === "local" ? `Removed ${file.name}. ${forgotten ? "What was read from it was cleared from this computer." : "The file itself was not touched."}` : `Unpinned ${file.name}.`, problems: [] }); void reload(); },
      (caught) => setProblem(message(caught)),
    );
  };
  const destroy = () => {
    spaceDelete(id).then(onGone, (caught) => { setDeleting(false); setProblem(message(caught)); });
  };

  if (!summary) {
    return <div className="space-view"><button className="space-back" onClick={onBack}><ArrowLeft size={15} /> Workspaces</button>{problem ? <p className="chat-error" role="alert">{problem}</p> : <p className="connectors-none"><LoaderCircle size={14} className="spin" /> Opening…</p>}</div>;
  }
  const total = summary.files + summary.cloud_files;
  return (
    <div className="space-view">
      <header className="space-head">
        <button className="space-back" onClick={onBack}><ArrowLeft size={15} /> Workspaces</button>
        <div className="space-title">
          <h1>{summary.name}</h1>
          {summary.description && <p>{summary.description}</p>}
          <small>{total} file{total === 1 ? "" : "s"}{summary.cloud_files > 0 ? ` (${summary.cloud_files} pinned from the firm cloud)` : ""} · {summary.chats} chat{summary.chats === 1 ? "" : "s"} · about {formatBytes(summary.stored_bytes)} on this computer</small>
        </div>
        <div className="space-head-actions">
          <button className="secondary" aria-label="Edit this workspace" onClick={() => setEditing(true)}><Pencil size={13} /> Edit</button>
          <button className="secondary danger" aria-label="Delete this workspace" onClick={() => setDeleting(true)}><Trash2 size={13} /> Delete</button>
        </div>
      </header>
      <div className="source-tabs" role="tablist" aria-label="Workspace">
        <button role="tab" aria-selected={tab === "chat"} className={tab === "chat" ? "active" : ""} onClick={() => setTab("chat")}><MessageSquareText size={14} /> Chat</button>
        <button role="tab" aria-selected={tab === "files"} className={tab === "files" ? "active" : ""} onClick={() => setTab("files")}>Files <span className="nav-count">{total}</span></button>
      </div>
      {problem && <p className="chat-error" role="alert">{problem}</p>}
      {notice && (
        <div className="space-notice" role="status">
          <span>{notice.text}</span>
          {notice.problems.length > 0 && <ul>{notice.problems.map((failed, index) => <li key={index}><strong>{failed.name}</strong>: {failed.reason}</li>)}</ul>}
          <button className="icon-button" aria-label="Dismiss" onClick={() => setNotice(null)}><X size={14} /></button>
        </div>
      )}
      {tab === "files" ? (
        <SpaceFiles files={files} adding={adding} cloud={cloud} onAddFiles={() => add(spaceAddFiles)} onAddFolder={() => add(spaceAddFolder)} onPin={pin} onRemove={remove} />
      ) : (
        <>
          {total === 0 && (
            <div className="space-empty-hint" role="note">
              <span>No files in this workspace yet, so there is nothing to answer from.</span>
              <button className="secondary" onClick={() => setTab("files")}><Plus size={13} /> Add files</button>
            </div>
          )}
          <ChatView
            space={{ id, name: summary.name, onAddFiles: () => add(spaceAddFiles), adding, onDeleteThread: deleteChat }}
            hasDocuments
            documents={[]}
            cloudReady={cloud.state === "ready"}
            threads={threads}
            setThreads={setThreads as Dispatch<SetStateAction<ChatThread[]>>}
            activeId={activeId}
            setActiveId={setActiveId}
            pending={null}
            onPendingUsed={() => undefined}
            onCitation={onCitation}
            onOpenLink={onOpenLink}
            onOpenAgents={onOpenAgents}
            onAnswered={onAnswered}
          />
        </>
      )}
      {editing && <EditSpace space={summary} onClose={() => setEditing(false)} onSaved={(next) => { setSummary(next); setEditing(false); }} />}
      {deleting && (
        <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setDeleting(false); }}>
          <div className="space-dialog" role="dialog" aria-label="Delete workspace">
            <header><h2>Delete “{summary.name}”?</h2></header>
            <p>Its {summary.chats} saved chat{summary.chats === 1 ? "" : "s"} are deleted, and what was read from its files is cleared from this computer. The files themselves are not touched, and nothing in the firm cloud changes. This cannot be undone.</p>
            <div className="space-dialog-actions">
              <button className="secondary" onClick={() => setDeleting(false)}>Keep it</button>
              <button className="primary danger" onClick={destroy}>Delete workspace</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
