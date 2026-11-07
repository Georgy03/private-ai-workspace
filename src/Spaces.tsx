import { FolderOpen, LoaderCircle, MessageSquareText, Plus, X } from "lucide-react";
import { useEffect, useState } from "react";
import { spaceCreate, spaces } from "./api";
import { ago, formatBytes } from "./spaceText";
import type { SpaceSummary } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

/** Name (and a line about) a new workspace. */
function NewSpace({ onCreated, onClose }: { onCreated: (space: SpaceSummary) => void; onClose: () => void }) {
  const [name, setName] = useState("");
  const [about, setAbout] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const create = () => {
    setBusy(true);
    setProblem(null);
    spaceCreate(name, about).then(onCreated, (caught) => { setBusy(false); setProblem(message(caught)); });
  };
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <form className="space-dialog" role="dialog" aria-label="New workspace" onSubmit={(event) => { event.preventDefault(); if (name.trim()) create(); }}>
        <header><h2>New workspace</h2><button type="button" className="icon-button" aria-label="Close" onClick={onClose}><X size={16} /></button></header>
        <p>One place for everything about a client or a matter: its files, and every question you ask about it.</p>
        <label>Name<input autoFocus aria-label="Workspace name" value={name} maxLength={80} placeholder="Steve Miller" onChange={(event) => setName(event.target.value)} /></label>
        <label>What it is for <small>(optional)</small><textarea aria-label="What it is for" rows={3} value={about} maxLength={500} placeholder="Custody case. Hearing set for March." onChange={(event) => setAbout(event.target.value)} /></label>
        {problem && <p className="chat-error" role="alert">{problem}</p>}
        <div className="space-dialog-actions">
          <button type="button" className="secondary" onClick={onClose}>Cancel</button>
          <button type="submit" className="primary" disabled={busy || !name.trim()}>{busy ? <><LoaderCircle size={14} className="spin" /> Creating…</> : "Create workspace"}</button>
        </div>
      </form>
    </div>
  );
}

/** Every workspace, and a way to start one. */
export function SpacesView({ onOpen }: { onOpen: (id: string) => void }) {
  const [list, setList] = useState<SpaceSummary[] | null>(null);
  const [creating, setCreating] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    void spaces().then(
      (all) => { if (alive) setList(all); },
      (caught) => { if (alive) { setList([]); setProblem(message(caught)); } },
    );
    return () => { alive = false; };
  }, []);

  return (
    <div className="spaces-view">
      <div className="page-heading">
        <div>
          <span className="eyebrow">Clients and matters</span>
          <h1>Workspaces</h1>
          <p>Everything about one client in one place: the files, and every question you have asked about them.</p>
        </div>
        <button className="primary" onClick={() => setCreating(true)}><Plus size={16} /> New workspace</button>
      </div>
      {problem && <p className="chat-error" role="alert">{problem}</p>}
      {list === null && <p className="connectors-none"><LoaderCircle size={14} className="spin" /> Loading…</p>}
      {list !== null && list.length === 0 && (
        <section className="spaces-empty" aria-label="No workspaces yet">
          <FolderOpen size={30} />
          <h2>Make a workspace for a client or a case</h2>
          <p>Call it after the client, add their files, and ask questions about them. Everything stays together, and your chat history is kept here for the next time.</p>
          <button className="primary" onClick={() => setCreating(true)}><Plus size={16} /> New workspace</button>
        </section>
      )}
      {list !== null && list.length > 0 && (
        <ul className="space-grid" aria-label="Workspaces">
          {list.map((space) => (
            <li key={space.id}>
              <button className="space-card" aria-label={`Open ${space.name}`} onClick={() => onOpen(space.id)}>
                <span className="space-card-icon"><FolderOpen size={18} /></span>
                <strong>{space.name}</strong>
                <span className="space-card-about">{space.description || "No description"}</span>
                <span className="space-card-meta">
                  <span>{space.files + space.cloud_files} file{space.files + space.cloud_files === 1 ? "" : "s"}</span>
                  <span><MessageSquareText size={12} /> {space.chats}</span>
                  <span>{formatBytes(space.stored_bytes)}</span>
                </span>
                <small>Updated {ago(space.updated_at)}</small>
              </button>
            </li>
          ))}
        </ul>
      )}
      {creating && <NewSpace onClose={() => setCreating(false)} onCreated={(space) => { setCreating(false); onOpen(space.id); }} />}
    </div>
  );
}
