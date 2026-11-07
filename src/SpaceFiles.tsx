import { Cloud, File as FileGlyph, FolderPlus, LoaderCircle, Pin, Plus, Search, Trash2, X } from "lucide-react";
import { useState } from "react";
import { cloudSearchBrowse } from "./api";
import { ConnectorMark } from "./ConnectorLogo";
import { FileIcon } from "./FileIcon";
import { fileApp } from "./fileApp";
import { formatBytes } from "./spaceText";
import type { BrowseRow, CloudStatus, SpaceFile } from "./types";

const SOURCE_NAMES: Record<string, string> = { google_drive: "Google Drive", microsoft365: "Microsoft 365", dropbox: "Dropbox", docusign: "DocuSign" };
const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

/** Search the firm cloud index and pin documents to the workspace. Only the name and ID are kept here. */
function PinDialog({ pinned, onPin, onClose }: { pinned: Set<string>; onPin: (row: BrowseRow) => Promise<void>; onClose: () => void }) {
  const [text, setText] = useState("");
  const [rows, setRows] = useState<BrowseRow[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [working, setWorking] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const search = () => {
    if (!text.trim()) return;
    setBusy(true);
    setProblem(null);
    cloudSearchBrowse(text.trim(), {}).then(
      (result) => { setRows(result.results); setBusy(false); },
      (caught) => { setProblem(message(caught)); setBusy(false); },
    );
  };
  const pin = (row: BrowseRow) => {
    setWorking(row.document_id);
    onPin(row).then(() => setWorking(null), (caught) => { setWorking(null); setProblem(message(caught)); });
  };
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div className="space-dialog wide" role="dialog" aria-label="Pin from the firm cloud index">
        <header><h2>Pin from the firm cloud index</h2><button type="button" className="icon-button" aria-label="Close" onClick={onClose}><X size={16} /></button></header>
        <p>Find documents in Google Drive, Microsoft 365 or Dropbox and add them to this workspace. Nothing is copied: only the name is kept here, and you can only pin what you may open.</p>
        <form className="search-box" role="search" onSubmit={(event) => { event.preventDefault(); search(); }}>
          <Search size={16} />
          <input autoFocus aria-label="Search the firm's sources" value={text} placeholder="Search by name or by what it says" onChange={(event) => setText(event.target.value)} />
          <button type="submit" className="secondary" disabled={busy || !text.trim()}>{busy ? <LoaderCircle size={14} className="spin" /> : "Search"}</button>
        </form>
        {problem && <p className="chat-error" role="alert">{problem}</p>}
        {rows !== null && rows.length === 0 && <p className="connectors-none">Nothing found. Try other words.</p>}
        {rows !== null && rows.length > 0 && (
          <ul className="pin-results" aria-label="Documents found">
            {rows.map((row) => (
              <li key={row.document_id}>
                <FileIcon app={row.app} size={26} />
                <span className="pin-name"><strong>{row.name}</strong><small>{SOURCE_NAMES[row.source] ?? row.source}</small></span>
                {pinned.has(row.document_id)
                  ? <span className="pinned-label"><Pin size={13} /> Pinned</span>
                  : <button className="secondary" aria-label={`Pin ${row.name}`} disabled={working !== null} onClick={() => pin(row)}>{working === row.document_id ? <LoaderCircle size={13} className="spin" /> : <><Plus size={13} /> Pin</>}</button>}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

/** Everything the workspace holds, and how to add to it. */
export function SpaceFiles({ files, adding, cloud, onAddFiles, onAddFolder, onPin, onRemove }: {
  files: SpaceFile[];
  adding: boolean;
  cloud: CloudStatus;
  onAddFiles: () => void;
  onAddFolder: () => void;
  onPin: (row: BrowseRow) => Promise<void>;
  onRemove: (file: SpaceFile) => void;
}) {
  const [pinning, setPinning] = useState(false);
  const [removing, setRemoving] = useState<SpaceFile | null>(null);
  const canPin = cloud.configured && cloud.signed_in;
  const pinned = new Set(files.filter((file) => file.kind === "cloud").map((file) => file.id));
  return (
    <div className="space-files">
      <div className="space-files-bar">
        <button className="primary" disabled={adding} onClick={onAddFiles}>{adding ? <><LoaderCircle size={14} className="spin" /> Reading files…</> : <><Plus size={15} /> Add files</>}</button>
        <button className="secondary" disabled={adding} onClick={onAddFolder}><FolderPlus size={15} /> Add a folder</button>
        <button className="secondary" disabled={!canPin} title={canPin ? undefined : "Sign in to the firm cloud index to pin its documents"} onClick={() => setPinning(true)}><Cloud size={15} /> Pin from the firm cloud</button>
      </div>
      <p className="space-files-note">Files stay where they are on your computer. The workspace keeps only a search index of their text, encrypted on this device.</p>
      {files.length === 0 ? (
        <section className="spaces-empty" aria-label="No files yet">
          <FileGlyph size={28} />
          <h2>No files yet</h2>
          <p>Add the contracts, letters and notes for this client. Questions in this workspace are answered only from them.</p>
        </section>
      ) : (
        <ul className="space-file-list" aria-label="Files in this workspace">
          {files.map((file) => (
            <li key={`${file.kind}:${file.id}`}>
              <FileIcon app={fileApp("", file.name)} size={28} />
              <span className="space-file-name">
                <strong>{file.name}</strong>
                <small>
                  {file.kind === "cloud" ? <><ConnectorMark kind={file.source ?? ""} size={11} /> {SOURCE_NAMES[file.source ?? ""] ?? "Firm cloud"} · pinned</> : `On this computer · ${formatBytes(file.size)}`}
                  {file.status === "missing" && " · no longer indexed"}
                  {file.status === "warning" && file.warning ? ` · ${file.warning}` : ""}
                </small>
              </span>
              <button className="secondary" aria-label={`Remove ${file.name}`} title={file.kind === "local" ? "Take it out of the workspace. The file itself is not touched." : "Unpin it"} onClick={() => setRemoving(file)}><Trash2 size={13} /> Remove</button>
            </li>
          ))}
        </ul>
      )}
      {removing && (
        <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setRemoving(null); }}>
          <div className="space-dialog" role="alertdialog" aria-label="Confirm removing a file">
            <header><h2>{removing.kind === "local" ? "Remove this file?" : "Unpin this document?"}</h2></header>
            <p><strong>{removing.name}</strong> will be taken out of this workspace, so answers here will no longer use it.{" "}
              {removing.kind === "local" ? "The file itself is not touched. What was read from it is cleared from this computer unless another workspace uses it." : "Nothing changes in the firm cloud."}</p>
            <div className="space-dialog-actions">
              <button className="secondary" autoFocus onClick={() => setRemoving(null)}>Keep it</button>
              <button className="primary danger" onClick={() => { const file = removing; setRemoving(null); onRemove(file); }}>{removing.kind === "local" ? "Yes, remove it" : "Yes, unpin it"}</button>
            </div>
          </div>
        </div>
      )}
      {pinning && <PinDialog pinned={pinned} onClose={() => setPinning(false)} onPin={onPin} />}
    </div>
  );
}
