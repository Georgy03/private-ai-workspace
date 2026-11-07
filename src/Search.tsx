import { useEffect, useState } from "react";
import { ExternalLink, Search as SearchIcon, X } from "lucide-react";
import { cloudSearchBrowse } from "./api";
import { ConnectorLogo, ConnectorMark } from "./ConnectorLogo";
import { FileIcon } from "./FileIcon";
import type { BrowseFilters, BrowseResult, CloudStatus } from "./types";
import { Shimmer } from "./Shimmer";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const SOURCE_NAMES: Record<string, string> = { google_drive: "Google Drive", microsoft365: "Microsoft 365", dropbox: "Dropbox", docusign: "DocuSign" };
const TYPE_NAMES: Record<string, string> = { document: "Documents", spreadsheet: "Spreadsheets", presentation: "Presentations", pdf: "PDFs", image: "Images", other: "Other" };
const DAY = 86400;
const UPDATED: { label: string; seconds: number | null }[] = [
  { label: "Any time", seconds: null },
  { label: "Past day", seconds: DAY },
  { label: "Past week", seconds: 7 * DAY },
  { label: "Past month", seconds: 30 * DAY },
  { label: "Past year", seconds: 365 * DAY },
];

const date = (epoch: number) => (epoch > 0 ? new Date(epoch * 1000).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" }) : "");

/** The words of the query, in bold where they appear in a passage. */
function Highlighted({ text, query }: { text: string; query: string }) {
  const words = query.split(/\s+/).filter((word) => word.length > 2).map((word) => word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (words.length === 0) return <>{text}</>;
  const parts = text.split(new RegExp(`(${words.join("|")})`, "gi"));
  return <>{parts.map((part, index) => (index % 2 === 1 ? <strong key={index}>{part}</strong> : part))}</>;
}

function toggle(list: string[] | undefined, value: string): string[] {
  const current = list ?? [];
  return current.includes(value) ? current.filter((item) => item !== value) : [...current, value];
}

export function SearchView({ cloud, onOpenLink }: { cloud: CloudStatus; onOpenLink: (url: string) => void }) {
  const [text, setText] = useState("");
  const [asked, setAsked] = useState("");
  const [updated, setUpdated] = useState<number | null>(null);
  const [sources, setSources] = useState<string[]>([]);
  const [types, setTypes] = useState<string[]>([]);
  const [result, setResult] = useState<BrowseResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const usable = cloud.configured && cloud.signed_in;

  // Runs again whenever the question or a filter changes.
  useEffect(() => {
    if (!asked) return;
    let alive = true;
    const filters: BrowseFilters = { sources, types };
    if (updated) filters.since_epoch = Math.floor(Date.now() / 1000) - updated;
    void Promise.resolve().then(() => {
      if (alive) setBusy(true);
      return cloudSearchBrowse(asked, filters);
    }).then(
      (next) => { if (alive) { setResult(next); setProblem(null); setBusy(false); } },
      (caught) => { if (alive) { setProblem(message(caught)); setBusy(false); } },
    );
    return () => { alive = false; };
  }, [asked, updated, sources, types]);

  if (!usable) {
    return <div className="knowledge-view"><p className="knowledge-note">Sign in to the firm's cloud workspace to search its connected sources.</p></div>;
  }

  const submit = () => {
    if (text.trim()) setAsked(text.trim());
  };
  const active = sources.length + types.length + (updated ? 1 : 0);

  return (
    <div className="search-view">
      <form className="search-box" role="search" onSubmit={(event) => { event.preventDefault(); submit(); }}>
        <SearchIcon size={18} />
        <input aria-label="Search the firm's sources" value={text} onChange={(event) => setText(event.target.value)} placeholder="Search Google Drive and Microsoft 365" />
        {text && <button type="button" className="icon-button" aria-label="Clear search" onClick={() => { setText(""); setAsked(""); setResult(null); }}><X size={16} /></button>}
      </form>

      {asked && (
        <div className="search-filters" aria-label="Filters">
          <label>Updated
            <select value={updated ?? ""} onChange={(event) => setUpdated(event.target.value ? Number(event.target.value) : null)}>
              {UPDATED.map((option) => <option key={option.label} value={option.seconds ?? ""}>{option.label}</option>)}
            </select>
          </label>
          <div className="filter-group" role="group" aria-label="Type">
            {(result?.facets.types ?? []).map((facet) => (
              <button key={facet.value} aria-pressed={types.includes(facet.value)} className="chip" onClick={() => setTypes(toggle(types, facet.value))}>
                {TYPE_NAMES[facet.value] ?? facet.value} <small>{facet.count}</small>
              </button>
            ))}
          </div>
          {active > 0 && <button className="link-button" onClick={() => { setUpdated(null); setSources([]); setTypes([]); }}>Clear filters</button>}
        </div>
      )}
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}

      {asked && (
        <div className="search-body">
          <section className="search-results" aria-label="Search results" aria-busy={busy}>
            <p className="knowledge-note">{busy && !result ? <Shimmer>Searching…</Shimmer> : `Found ${result?.total ?? 0} result${result?.total === 1 ? "" : "s"}`}</p>
            {result?.results.map((row) => (
              <article className="search-result" key={row.document_id}>
                <span className="result-icon">
                  {/* A file whose kind is not known shows the service it lives in. */}
                  {row.app === "other" && (row.source in SOURCE_NAMES) ? <ConnectorMark kind={row.source} size={40} /> : <FileIcon app={row.app} size={40} />}
                </span>
                <div>
                  <h3>
                    {row.link ? <button className="result-title" onClick={() => onOpenLink(row.link!)} aria-label={`Open ${row.name}`}>{row.name} <ExternalLink size={12} /></button> : row.name}
                  </h3>
                  <small>{SOURCE_NAMES[row.source] ?? row.source}{row.file_type !== "other" ? ` · ${TYPE_NAMES[row.file_type] ?? row.file_type}` : ""}{row.modified_epoch > 0 ? ` · ${date(row.modified_epoch)}` : ""}{row.location_label ? ` · ${row.location_label}` : ""}</small>
                  <p><Highlighted text={row.excerpt} query={asked} /></p>
                </div>
              </article>
            ))}
            {result && result.results.length === 0 && !busy && <p className="knowledge-note">Nothing matches. Try different words or clear the filters.</p>}
          </section>
          <aside className="search-facets" aria-label="Sources">
            <h4>Sources</h4>
            <ul>
              {(result?.facets.sources ?? []).map((facet) => (
                <li key={facet.value}>
                  <button aria-pressed={sources.includes(facet.value)} onClick={() => setSources(toggle(sources, facet.value))}>
                    <ConnectorLogo kind={facet.value} name={SOURCE_NAMES[facet.value] ?? facet.value} small />
                    <span>{SOURCE_NAMES[facet.value] ?? facet.value}</span><em>{facet.count}</em>
                  </button>
                </li>
              ))}
            </ul>
          </aside>
        </div>
      )}
      {!asked && <p className="knowledge-note">Search across every source your firm has connected. You only see files you are allowed to open. Files on this computer are searched from Home.</p>}
    </div>
  );
}
