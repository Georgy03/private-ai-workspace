import { useEffect, useRef, useState } from "react";
import { AtSign, Check, Filter, LoaderCircle, Paperclip, Plus, X } from "lucide-react";
import { cloudSearchBrowse } from "./api";
import { limitsLabel } from "./limits";
import { ScopePanel } from "./ScopePanel";
import type { AttachedFile, DocumentResource, SearchLimits, TaggedFile } from "./types";

type Panel = "menu" | "tag" | "filter" | null;

/**
 * The "+" in the message box: attach a file from this computer, tag one file so the answer comes from
 * all of it, or limit where the question looks. What was chosen is shown as chips (see ComposerChips).
 */
export function ComposerPlus({ documents, cloudReady, attachCount, maxAttachments, limits, onAttach, onTag, onLimits }: {
  documents: DocumentResource[];
  cloudReady: boolean;
  attachCount: number;
  maxAttachments: number;
  limits: SearchLimits;
  onAttach: () => void;
  onTag: (file: TaggedFile) => void;
  onLimits: (limits: SearchLimits) => void;
}) {
  const [panel, setPanel] = useState<Panel>(null);
  const [query, setQuery] = useState("");
  const [cloudFiles, setCloudFiles] = useState<TaggedFile[]>([]);
  const holder = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!panel) return;
    const away = (event: MouseEvent) => {
      if (holder.current && !holder.current.contains(event.target as Node)) setPanel(null);
    };
    const escape = (event: KeyboardEvent) => { if (event.key === "Escape") setPanel(null); };
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", escape);
    return () => { document.removeEventListener("mousedown", away); document.removeEventListener("keydown", escape); };
  }, [panel]);

  const search = (text: string) => {
    setQuery(text);
    if (!cloudReady || text.trim().length < 2) { setCloudFiles([]); return; }
    cloudSearchBrowse(text.trim(), {}).then(
      (found) => setCloudFiles(found.results.slice(0, 6).map((row) => ({ id: row.document_id, name: row.name, origin: "cloud" as const }))),
      () => setCloudFiles([]),
    );
  };

  const needle = query.trim().toLowerCase();
  const local: TaggedFile[] = [...documents]
    .sort((a, b) => b.modified_at - a.modified_at)
    .filter((doc) => !needle || doc.name.toLowerCase().includes(needle))
    .slice(0, 8)
    .map((doc) => ({ id: doc.id, name: doc.name, origin: "local" as const }));

  const open = (next: Panel) => { setPanel(next); setQuery(""); setCloudFiles([]); };
  const full = attachCount >= maxAttachments;

  return (
    <div className="composer-plus" ref={holder}>
      <button type="button" className="plus-button" aria-label="Add to your message" aria-haspopup="menu" aria-expanded={panel !== null} onClick={() => open(panel ? null : "menu")}>
        <Plus size={18} />
      </button>
      {panel === "menu" && (
        <div className="plus-menu" role="menu" aria-label="Add to your message">
          <button type="button" role="menuitem" onClick={() => open("tag")}>
            <AtSign size={18} />
            <span><strong>Tag a file</strong><small>Answer from all of one file</small></span>
          </button>
          <button type="button" role="menuitem" onClick={() => open("filter")}>
            <Filter size={18} />
            <span><strong>Filter data</strong><small>{limits.scope ? limitsLabel(limits, "Everything") : "Choose where it looks"}</small></span>
          </button>
          <button type="button" role="menuitem" disabled={full} onClick={() => { setPanel(null); onAttach(); }}>
            <Paperclip size={18} />
            <span><strong>Attach file</strong><small>{full ? `Up to ${maxAttachments} files` : "Upload a local document"}</small></span>
          </button>
        </div>
      )}
      {panel === "tag" && (
        <div className="plus-menu plus-tag" role="dialog" aria-label="Tag a file">
          <input autoFocus aria-label="Find a file" placeholder="Find a file by name…" value={query} onChange={(event) => search(event.target.value)} />
          <ul>
            {local.map((file) => <li key={`l${file.id}`}><button type="button" onClick={() => { setPanel(null); onTag(file); }}><AtSign size={15} /> {file.name}<small>This device</small></button></li>)}
            {cloudFiles.map((file) => <li key={`c${file.id}`}><button type="button" onClick={() => { setPanel(null); onTag(file); }}><AtSign size={15} /> {file.name}<small>Firm cloud</small></button></li>)}
            {local.length + cloudFiles.length === 0 && <li className="plus-none">No files match.</li>}
          </ul>
        </div>
      )}
      {panel === "filter" && (
        <div className="plus-menu plus-scope">
          <ScopePanel limits={limits} cloudReady={cloudReady} everything="Everything" label="Filter data" onChange={onLimits} onClose={() => setPanel(null)} />
        </div>
      )}
    </div>
  );
}

/** What the next question will include or be limited to, each with a way to take it off again. */
export function ComposerChips({ attachments, attaching, tagged, limits, onRemoveAttachment, onRemoveTag, onRemoveScope }: {
  attachments: AttachedFile[];
  /** A file is being chosen and read right now. */
  attaching: boolean;
  tagged: TaggedFile | null;
  limits: SearchLimits;
  onRemoveAttachment: (id: string) => void;
  onRemoveTag: () => void;
  onRemoveScope: () => void;
}) {
  if (attachments.length === 0 && !attaching && !tagged && !limits.scope) return null;
  return (
    <ul className="composer-chips" aria-label="Included with your message">
      {attachments.map((file) => (
        <li key={file.id} className="attached"><Check size={13} /> <em>Attached</em> <span>{file.name}</span><button type="button" aria-label={`Remove ${file.name}`} onClick={() => onRemoveAttachment(file.id)}><X size={13} /></button></li>
      ))}
      {attaching && <li className="attaching" role="status"><LoaderCircle size={13} className="spin" /> <span>Attaching your file…</span></li>}
      {tagged && <li><AtSign size={13} /> <span>{tagged.name}</span><button type="button" aria-label={`Remove ${tagged.name}`} onClick={onRemoveTag}><X size={13} /></button></li>}
      {limits.scope && <li><Filter size={13} /> <span>{limitsLabel(limits, "Everything")}</span><button type="button" aria-label="Remove the filter" onClick={onRemoveScope}><X size={13} /></button></li>}
    </ul>
  );
}
