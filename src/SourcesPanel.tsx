import { ExternalLink, Maximize2, Minimize2, X } from "lucide-react";
import { ConnectorMark } from "./ConnectorLogo";
import { FileIcon } from "./FileIcon";
import { appFromLink } from "./fileApp";
import type { Citation } from "./types";

/** The files an answer came from, in the order the answer cites them, beside the chat. */
export function SourcesPanel({ citations, expanded, onExpand, onCitation, onOpenLink, onClose }: {
  expanded: boolean;
  onExpand: () => void;
  citations: Citation[];
  onCitation: (citation: Citation) => void;
  onOpenLink: (url: string) => void;
  onClose: () => void;
}) {
  return (
    <aside className={`sources-panel ${expanded ? "expanded" : ""}`} aria-label="Search results">
      <header>
        <div>
          <h2>Search results</h2>
          <small>{citations.length} source{citations.length === 1 ? "" : "s"} used for this answer</small>
        </div>
        <div className="sources-actions">
          <button className="icon-button" aria-label={expanded ? "Make the results narrower" : "Make the results wider"} aria-pressed={expanded} onClick={onExpand}>{expanded ? <Minimize2 size={15} /> : <Maximize2 size={15} />}</button>
          <button className="icon-button" aria-label="Close search results" onClick={onClose}><X size={16} /></button>
        </div>
      </header>
      <ol>
        {citations.map((citation, index) => {
          // A file whose kind is not known shows the service it lives in, as Search does.
          const { app, source } = citation.document_id.startsWith("web:") ? { app: "other" as const, source: null } : appFromLink(citation.link, citation.document_name);
          return (
          <li className="source-result" key={citation.id}>
            <b>{index + 1}</b>
            <span className="result-icon">{app === "other" && source ? <ConnectorMark kind={source} size={40} /> : <FileIcon app={app} size={40} />}</span>
            <div>
              <h3>
                <button className="result-title" onClick={() => onCitation(citation)}>{citation.document_name}</button>
                {citation.link && <button className="open-link" aria-label={`Open ${citation.document_name} in its app`} onClick={() => onOpenLink(citation.link!)}><ExternalLink size={12} /></button>}
              </h3>
              <small>{citation.document_id.startsWith("web:") ? "Web" : `${citation.origin === "cloud" ? "Firm cloud" : "This device"}${citation.location_label ? ` · ${citation.location_label}` : ""}`}</small>
              <p>{citation.excerpt}</p>
            </div>
          </li>
          );
        })}
      </ol>
    </aside>
  );
}
