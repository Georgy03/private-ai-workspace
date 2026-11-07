import { useEffect, useState } from "react";
import { Check, Search } from "lucide-react";
import { cloudConnectors } from "./api";
import { ConnectorMark } from "./ConnectorLogo";
import { KINDS, SOURCE_NAMES } from "./limits";
import type { ChatScope, SearchLimits } from "./types";

const SCOPES: { value: ChatScope; label: string; note: string }[] = [
  { value: null, label: "All sources", note: "This device and the firm cloud" },
  { value: "local", label: "This device only", note: "Nothing is asked of the firm cloud index" },
  { value: "cloud", label: "Firm cloud only", note: "Connected sources, not files on this device" },
];

const toggle = (list: string[], value: string) => (list.includes(value) ? list.filter((item) => item !== value) : [...list, value]);

/**
 * Where a question looks. Choosing "Firm cloud only" opens a search box with the connected sources
 * (Google Drive and so on) and the kinds of file (Docs, Sheets, Slides and so on) to limit it to.
 */
export function ScopePanel({ limits, cloudReady, everything = "All sources", label = "Where to look", onChange, onClose }: {
  limits: SearchLimits;
  cloudReady: boolean;
  /** What the first choice is called. */
  everything?: string;
  label?: string;
  onChange: (limits: SearchLimits) => void;
  onClose: () => void;
}) {
  const [connected, setConnected] = useState<string[]>([]);
  const [search, setSearch] = useState("");

  // The sources this person's firm has connected, for the list to choose from.
  useEffect(() => {
    let alive = true;
    if (!cloudReady) return;
    void cloudConnectors().then(
      (view) => {
        const kinds = [...new Set(view.connections.map((connection) => connection.kind))].filter((kind) => kind !== "outlook_mail");
        if (alive) setConnected(kinds);
      },
      () => undefined,
    );
    return () => { alive = false; };
  }, [cloudReady]);

  const needle = search.trim().toLowerCase();
  const sources = connected.filter((kind) => !needle || `${SOURCE_NAMES[kind] ?? kind} ${kind}`.toLowerCase().includes(needle));
  const kinds = KINDS.filter((kind) => !needle || `${kind.label} ${kind.hint} ${kind.words}`.toLowerCase().includes(needle));
  const choose = (scope: ChatScope) => {
    onChange({ scope, sources: scope === "cloud" ? limits.sources : [], types: scope === "cloud" ? limits.types : [] });
    if (scope !== "cloud") onClose();
  };

  return (
    <div className="scope-panel" role="dialog" aria-label={label}>
      <div className="scope-choices" role="group" aria-label="Where to look">
        {SCOPES.filter((item) => item.value !== "cloud" || cloudReady).map((item) => (
          <button type="button" key={item.label} role="menuitemradio" aria-checked={limits.scope === item.value} onClick={() => choose(item.value)}>
            {limits.scope === item.value ? <Check size={16} /> : <span className="scope-gap" />}
            <span><strong>{item.value === null ? everything : item.label}</strong><small>{item.note}</small></span>
          </button>
        ))}
      </div>

      {limits.scope === "cloud" && (
        <div className="scope-filters">
          <label className="scope-search">
            <Search size={14} />
            <input aria-label="Search sources and file types" placeholder="Search sources and file types…" value={search} onChange={(event) => setSearch(event.target.value)} />
          </label>
          <div className="scope-list">
            {sources.length > 0 && <h4>Sources</h4>}
            {sources.map((kind) => (
              <label key={kind} className="scope-check">
                <input type="checkbox" checked={limits.sources.includes(kind)} onChange={() => onChange({ ...limits, sources: toggle(limits.sources, kind) })} />
                <span className="scope-logo"><ConnectorMark kind={kind} size={18} /></span>
                <span>{SOURCE_NAMES[kind] ?? kind}</span>
              </label>
            ))}
            {connected.length === 0 && !needle && <p className="scope-none">No sources are connected yet.</p>}
            {kinds.length > 0 && <h4>Kinds of file</h4>}
            {kinds.map((kind) => (
              <label key={kind.value} className="scope-check">
                <input type="checkbox" checked={limits.types.includes(kind.value)} onChange={() => onChange({ ...limits, types: toggle(limits.types, kind.value) })} />
                <span>{kind.label}<small>{kind.hint}</small></span>
              </label>
            ))}
            {sources.length === 0 && kinds.length === 0 && <p className="scope-none">Nothing matches “{search.trim()}”.</p>}
          </div>
          <div className="scope-foot">
            <button type="button" className="link-button" disabled={limits.sources.length + limits.types.length === 0} onClick={() => onChange({ scope: "cloud", sources: [], types: [] })}>Clear</button>
            <button type="button" className="secondary" onClick={onClose}>Done</button>
          </div>
        </div>
      )}
    </div>
  );
}
