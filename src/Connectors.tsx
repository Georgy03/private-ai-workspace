import { useCallback, useEffect, useRef, useState } from "react";
import { Shimmer } from "./Shimmer";
import { ZoomMeetings } from "./ZoomMeetings";
import { ConnectorLogo } from "./ConnectorLogo";
import { AlertTriangle, Check, LoaderCircle, Lock, Plus, RefreshCw, Search, Trash2, Users, X } from "lucide-react";
import { cloudConnect, cloudConnectionFiles, cloudConnectors, cloudDisconnect, cloudSetScope, cloudSignIn, cloudSyncNow } from "./api";
import type { CloudStatus, Connection, ConnectorInfo, ConnectorsView, NameOnlyFile } from "./types";

/** Connectors on the roadmap. Shown so people can see what is coming; they cannot be connected yet. */
const PLANNED = [
  { name: "Box", category: "Files" },
  { name: "Clio", category: "Practice management" },
  { name: "NetDocuments", category: "Files" },
];
/** What each connector is for, so the list can be narrowed. */
const CATEGORY: Record<string, string> = { google_drive: "Files", microsoft365: "Files", dropbox: "Files", docusign: "Signing", quickbooks: "Accounting", outlook_mail: "Email", zoom: "Meetings" };
/** The connectors the app knows about. They are shown even when the server cannot be reached, so
 *  the list never looks empty; they are only usable when the server says they are set up. */
const KNOWN: ConnectorInfo[] = [
  { kind: "google_drive", name: "Google Drive", description: "Docs, Sheets, Slides, PDFs and Office files in My Drive, shared drives and files shared with you. You can also save a report into your own Drive, after you have read and approved it.", available: false },
  { kind: "microsoft365", name: "Microsoft 365", description: "OneDrive and SharePoint document libraries: Word, Excel, PowerPoint and PDF files.", available: false },
  { kind: "dropbox", name: "Dropbox", description: "Client files and shared documents in Dropbox: Word, Excel, PowerPoint, PDF and text files, with the sharing Dropbox records.", available: false },
  { kind: "docusign", name: "DocuSign", description: "Contracts and agreements sent for signature: the signed documents and who has signed or is still outstanding.", available: false },
  { kind: "quickbooks", name: "QuickBooks Online", description: "A company's customers, vendors, invoices, bills, payments, expenses and key reports, kept up to date. Read only, and only you can search what you connect.", available: false },
  { kind: "zoom", name: "Zoom", description: "Create Zoom meetings for you, after you have read and approved them, and put the join link in your emails. It cannot read your meetings, recordings or contacts.", available: false },
  { kind: "outlook_mail", name: "Outlook email", description: "Send the emails your AI helpers prepare, from your own Outlook, after you have read them. It can only send what you approve, and cannot read your mail.", available: false },
];
/** Connected for one person to do something for them, with no files to read or sync. */
const SOLO = ["outlook_mail", "zoom"];
/** Connected for one person only, so there is no choice of who the files are for. */
const PRIVATE_ONLY = [...SOLO, "quickbooks"];
const POLL_MS = 3000;
const APPROVAL_WAIT_MS = 5 * 60 * 1000;

interface Waiting {
  kind: string;
  /** How many connections existed when the sign-in was opened. */
  before: number;
  until: number;
  reconnect?: string;
}

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

function agoLabel(epochSeconds: number | null, now = Date.now()): string {
  if (epochSeconds === null) return "Not synced yet";
  const seconds = Math.max(0, Math.round(now / 1000 - epochSeconds));
  if (seconds < 60) return "Synced just now";
  if (seconds < 3600) return `Synced ${Math.floor(seconds / 60)} min ago`;
  if (seconds < 86400) return `Synced ${Math.floor(seconds / 3600)} h ago`;
  return `Synced ${Math.floor(seconds / 86400)} d ago`;
}

/** Plain words for why a file is stored by name only. */
const REASONS: Record<string, string> = {
  no_text: "No text found, even after reading scans (blank pages, photos with no writing, empty files)",
  unsupported_type: "File types the reader does not support (such as images)",
  too_large: "Too large to read safely (over about half a gigabyte)",
  unreadable: "Damaged, password-protected, or built in a way that crashed the reader",
  retry: "A temporary problem; tried again on the next sync",
};

function durationLabel(seconds: number): string {
  if (seconds < 90) return "under a minute";
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`;
  const hours = seconds / 3600;
  return `${hours < 10 ? hours.toFixed(1) : Math.round(hours)} h`;
}

/** One line on how a running sync is going. Totals are not known in advance, so no percentage. */
function progressLabel(connection: Connection, now = Date.now()): string {
  const read = connection.sync_files_read ?? 0;
  const started = connection.sync_started_epoch ?? null;
  const elapsed = started === null ? 0 : Math.max(0, now / 1000 - started);
  const rate = elapsed >= 60 && read > 0 ? Math.round((read / elapsed) * 60) : null;
  const parts = [`${read.toLocaleString()} files read so far`];
  if (rate !== null) parts.push(`about ${rate.toLocaleString()} a minute`);
  if (started !== null) parts.push(`running for ${durationLabel(elapsed)}`);
  return parts.join(" · ");
}

function nextLabel(epochSeconds: number | null | undefined, now = Date.now()): string | null {
  if (!epochSeconds) return null;
  const seconds = epochSeconds - now / 1000;
  return seconds <= 60 ? "Next check within a minute" : `Next check in about ${Math.round(seconds / 60)} min`;
}

function parseDomains(text: string): string[] {
  return text.split(/[,\s]+/).map((domain) => domain.trim().toLowerCase()).filter(Boolean);
}

export function ConnectorsPanel({ cloud, onCloud }: { cloud: CloudStatus; onCloud: (status: CloudStatus) => void }) {
  const [fetched, setView] = useState<ConnectorsView | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [waiting, setWaitingState] = useState<Waiting | null>(null);
  const waitingRef = useRef<Waiting | null>(null);
  const [busy, setBusy] = useState(false);
  const [domains, setDomains] = useState("");
  // Narrowing the list: words to look for, a kind of connector, and only the ones already connected.
  const [search, setSearch] = useState("");
  const [category, setCategory] = useState("All");
  const [onlyConnected, setOnlyConnected] = useState(false);
  // The source being connected, and who its files are for, chosen before signing in to the provider.
  const [choosing, setChoosing] = useState<string | null>(null);
  const [scope, setScope] = useState<"private" | "workspace">("private");
  const [openDetails, setOpenDetails] = useState<string | null>(null);
  const [detailFiles, setDetailFiles] = useState<NameOnlyFile[] | null>(null);

  const usable = cloud.configured && cloud.signed_in;
  const view = usable ? fetched : null;
  const loading = usable && fetched === null && problem === null;

  const setWaiting = (next: Waiting | null) => {
    waitingRef.current = next;
    setWaitingState(next);
  };

  // Reads the lists, and notices when a connection being waited for has arrived or time has run out.
  const load = useCallback(
    () =>
      cloudConnectors().then(
        (next) => {
          setView(next);
          setProblem(null);
          const pending = waitingRef.current;
          if (!pending) return;
          const arrived = pending.reconnect
            ? next.connections.find((item) => item.id === pending.reconnect)?.status === "active"
            : next.connections.length > pending.before;
          if (arrived) setNote("Connected. Your files are being added to the firm cloud index.");
          if (arrived || Date.now() > pending.until) {
            waitingRef.current = null;
            setWaitingState(null);
          }
        },
        (caught) => setProblem(message(caught)),
      ),
    [],
  );

  useEffect(() => {
    if (usable) void load();
  }, [usable, load]);

  // Look again every few seconds while waiting for an approval, or while a new source has not yet
  // finished its first sync.
  const firstSyncPending = view?.connections.some((item) => !SOLO.includes(item.kind) && item.status === "active" && (item.last_sync_epoch === null || item.syncing)) ?? false;
  useEffect(() => {
    if (!usable || (!waiting && !firstSyncPending)) return;
    const timer = setInterval(() => void load(), POLL_MS);
    return () => clearInterval(timer);
  }, [usable, waiting, firstSyncPending, load]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setProblem(null);
    setNote(null);
    try {
      await action();
    } catch (caught) {
      setProblem(message(caught));
    } finally {
      setBusy(false);
    }
  };

  const connect = (kind: string, reconnect?: string, chosen?: "private" | "workspace") =>
    run(async () => {
      await cloudConnect(kind, kind === "microsoft365" ? parseDomains(domains) : [], reconnect, chosen);
      setChoosing(null);
      setWaiting({ kind, before: view?.connections.length ?? 0, until: Date.now() + APPROVAL_WAIT_MS, reconnect });
    });

  const changeScope = (connection: Connection, name: string, next: "private" | "workspace") => {
    const question =
      next === "private"
        ? `Make ${name} private? Only you will be able to search these files. Everyone else loses access right away.`
        : `Share ${name} with the entire workspace? Everyone at your firm will be able to search these files, each person finding only what they are allowed to open in ${name}.`;
    if (!window.confirm(question)) return;
    return run(async () => {
      await cloudSetScope(connection.id, next);
      await load();
      setNote(next === "private" ? `${name} is now private to you.` : `${name} is being shared with the workspace. Each file's own sharing is restored in a few minutes.`);
    });
  };

  const disconnect = (connection: Connection, name: string) => {
    if (!window.confirm(`Remove ${name}? Its files are deleted from the firm cloud index. Your files in ${name} are not touched.`)) return;
    return run(async () => {
      await cloudDisconnect(connection.id);
      await load();
      setNote(`${name} was removed.`);
    });
  };

  const toggleDetails = (connection: Connection) => {
    if (openDetails === connection.id) {
      setOpenDetails(null);
      return;
    }
    setOpenDetails(connection.id);
    setDetailFiles(null);
    cloudConnectionFiles(connection.id).then(setDetailFiles, () => setDetailFiles([]));
  };

  const syncNow = (connection: Connection) =>
    run(async () => {
      await cloudSyncNow(connection.id);
      await load();
      setNote("Sync requested. New and changed files will appear in a few minutes.");
    });

  if (!cloud.configured) {
    return (
      <div className="connectors-empty">
        <strong>The firm cloud index is not set up on this device.</strong>
        <p>Your firm's administrator provides the server address. Once it is set, you can connect Google Drive and Microsoft 365 here.</p>
      </div>
    );
  }
  if (!cloud.signed_in) {
    return (
      <div className="connectors-empty">
        <strong>Sign in to manage connectors.</strong>
        <p>Connectors copy files from your firm's accounts into the firm cloud index so they can be searched with your local files.</p>
        <button className="primary" disabled={busy} onClick={() => void run(async () => onCloud(await cloudSignIn()))}>{busy ? "Waiting for sign-in…" : "Sign in"}</button>
        {problem && <p className="connector-problem"><AlertTriangle size={14} /> {problem}</p>}
      </div>
    );
  }

  const connectors = KNOWN.map((known) => view?.connectors.find((item) => item.kind === known.kind) ?? known);
  const nameOf = (kind: string) => connectors.find((item) => item.kind === kind)?.name ?? kind;
  const canShare = view?.can_share_with_workspace ?? false;

  const words = search.trim().toLowerCase();
  const categories = ["All", ...Array.from(new Set([...connectors.map((item) => CATEGORY[item.kind]), ...PLANNED.map((item) => item.category)].filter(Boolean)))];
  const fits = (name: string, description: string, kindCategory: string | undefined) =>
    (category === "All" || kindCategory === category) && (!words || `${name} ${description} ${kindCategory ?? ""}`.toLowerCase().includes(words));
  const shownConnectors = connectors.filter((item) => fits(item.name, item.description, CATEGORY[item.kind]) && (!onlyConnected || (view?.connections.some((c) => c.kind === item.kind) ?? false)));
  const shownPlanned = onlyConnected ? [] : PLANNED.filter((item) => fits(item.name, "coming soon", item.category));
  const shownConnections = view?.connections.filter((item) => fits(`${nameOf(item.kind)} ${item.label ?? ""}`, "", CATEGORY[item.kind])) ?? [];

  return (
    <div className="connectors">
      <p className="connectors-intro">Connect a source to add its files to your firm's cloud index. Sources are only read, except that Google Drive can also save files you approve and Outlook can send emails you approve. You choose whether the files are only for you or for the entire workspace, where each person sees only the files they may open in the source. Removing a connector deletes its copy.</p>
      {problem && <p className="connector-problem" role="alert"><AlertTriangle size={14} /> {problem}</p>}
      {note && <p className="connector-note" role="status"><Check size={14} /> {note}</p>}

      {choosing && (
        <section className="scope-chooser" aria-label="Who can search these files?">
          <h2>Who should be able to search these files from {nameOf(choosing)}?</h2>
          <div className="scope-options" role="radiogroup" aria-label="Who can search these files?">
            <label className={`scope-option ${scope === "private" ? "chosen" : ""}`}>
              <input type="radio" name="scope" checked={scope === "private"} onChange={() => setScope("private")} />
              <Lock size={18} />
              <span><strong>Only me</strong><small>Only you can search these files. Nobody else at your firm can find them, even if they are shared with others in {nameOf(choosing)}.</small></span>
            </label>
            <label className={`scope-option ${scope === "workspace" ? "chosen" : ""} ${canShare ? "" : "disabled"}`}>
              <input type="radio" name="scope" checked={scope === "workspace"} disabled={!canShare} onChange={() => setScope("workspace")} />
              <Users size={18} />
              <span>
                <strong>Entire workspace</strong>
                <small>Everyone at your firm can search these files, but each person only finds the ones they are allowed to open in {nameOf(choosing)}.</small>
                {!canShare && <small className="scope-hint">Only an administrator can connect a source for the whole workspace.</small>}
              </span>
            </label>
          </div>
          <div className="scope-actions">
            <button className="primary" disabled={busy} onClick={() => void connect(choosing, undefined, scope)}>Continue to {nameOf(choosing)}</button>
            <button className="secondary" onClick={() => setChoosing(null)}>Cancel</button>
          </div>
        </section>
      )}
      <div className="connector-tools">
        <label className="connector-search">
          <Search size={15} />
          <input aria-label="Search connectors" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search connectors" />
          {search && <button type="button" className="icon-button" aria-label="Clear search" onClick={() => setSearch("")}><X size={14} /></button>}
        </label>
        <div className="connector-filters" role="group" aria-label="Filter connectors">
          {categories.map((name) => <button key={name} className="chip" aria-pressed={category === name} onClick={() => setCategory(name)}>{name}</button>)}
          <button className="chip" aria-pressed={onlyConnected} onClick={() => setOnlyConnected(!onlyConnected)}>Connected</button>
        </div>
      </div>
      <h2 className="connectors-heading">Add a connector</h2>
      {loading && !view && <p className="connectors-loading"><LoaderCircle size={15} className="spin" /> <Shimmer>Loading…</Shimmer></p>}
      <div className="connector-grid">
        {shownConnectors.map((connector) => {
          const connected = view?.connections.filter((item) => item.kind === connector.kind).length ?? 0;
          const isWaiting = waiting?.kind === connector.kind && !waiting.reconnect;
          return (
            <article className="connector-card" key={connector.kind}>
              <ConnectorLogo kind={connector.kind} name={connector.name} />
              <div className="connector-body">
                <strong>{connector.name}</strong>
                <p>{connector.description}</p>
                {connected > 0 && <small className="connector-connected"><Check size={12} /> Connected{connected > 1 ? ` (${connected})` : ""}</small>}
                {!connector.available && <small className="connector-unavailable">{view ? "Not set up on this server" : "Unavailable until the server can be reached"}</small>}
                {connector.kind === "microsoft365" && connector.available && (
                  <input aria-label="Firm email domains (optional)" value={domains} onChange={(event) => setDomains(event.target.value)} placeholder="firm.com (optional)" />
                )}
              </div>
              <button className={connected > 0 ? "secondary" : "primary"} aria-label={connected > 0 ? `Connect another ${connector.name}` : `Connect ${connector.name}`} disabled={!connector.available || busy || waiting !== null} onClick={() => { if (PRIVATE_ONLY.includes(connector.kind)) { void connect(connector.kind, undefined, "private"); return; } setScope("private"); setChoosing(connector.kind); }}>
                {isWaiting ? <><LoaderCircle size={14} className="spin" /> <Shimmer>Waiting for approval…</Shimmer></> : <><Plus size={14} /> {connected > 0 ? "Add another" : "Connect"}</>}
              </button>
            </article>
          );
        })}
        {shownPlanned.map(({ name }) => (
          <article className="connector-card planned" key={name}>
            <div className="connector-logo planned" aria-hidden="true">{name.slice(0, 1)}</div>
            <div className="connector-body"><strong>{name}</strong><small>Coming soon</small></div>
            <button className="secondary" disabled aria-label={`${name} is coming soon`}>Coming soon</button>
          </article>
        ))}
      </div>
      {shownConnectors.length === 0 && shownPlanned.length === 0 && <p className="connectors-none">No connectors match. Try other words, or choose All.</p>}

      <h2 className="connectors-heading">Connected sources</h2>
      {view && view.connections.length > 0 && shownConnections.length === 0 && <p className="connectors-none">None of your connected sources match.</p>}
      {view && view.connections.length === 0 && <p className="connectors-none">Nothing connected yet.</p>}
      <section className="connection-list" aria-label="Connected sources">
        {shownConnections.map((connection) => {
          const name = nameOf(connection.kind);
          const needsReconnect = connection.status === "needs_reauth";
          const isMail = SOLO.includes(connection.kind);
          const syncing = !isMail && connection.status === "active" && (connection.last_sync_epoch === null || connection.syncing === true);
          const reasons = Object.entries(connection.name_only_reasons ?? {});
          const nameOnly = reasons.reduce((total, [, count]) => total + count, 0);
          const failed = connection.sync_files_failed ?? 0;
          const next = nextLabel(connection.next_sync_epoch);
          return (
            <div className="connection-row" key={connection.id}>
              <ConnectorLogo kind={connection.kind} name={name} small />
              <div className="connection-main">
                <strong>{name}{connection.kind === "quickbooks" && connection.label ? `: ${connection.label}` : ""}</strong>
                {connection.syncing ? (
                  <span>{progressLabel(connection)}</span>
                ) : (
                  isMail
                    ? connection.kind === "zoom"
                      ? <span>{connection.label ? `Creates meetings in ${connection.label}` : "Creates meetings in your Zoom"} when you approve them. It cannot read your meetings or recordings.</span>
                      : <span>{connection.label ? `Sends email from ${connection.label}` : "Sends email from your Outlook"}. It cannot read your mail.</span>
                    : <span>{connection.documents.toLocaleString()} files · {agoLabel(connection.last_sync_epoch)}{next && !needsReconnect ? ` · ${next}` : ""}</span>
                )}
                {connection.readable !== undefined && connection.documents > 0 && (
                  <small className="connection-split">
                    {connection.readable.toLocaleString()} searchable by content
                    {nameOnly > 0 && <> · {nameOnly.toLocaleString()} by name only <button className="link-button" onClick={() => toggleDetails(connection)} aria-expanded={openDetails === connection.id}>{openDetails === connection.id ? "Hide" : "Why?"}</button></>}
                  </small>
                )}
                {connection.kind === "zoom" && connection.yours && !needsReconnect && <ZoomMeetings />}
                {failed > 0 && <small className="connection-warn">{failed.toLocaleString()} file{failed === 1 ? "" : "s"} could not be read this time and will be tried again</small>}
                {connection.last_error && <small className="connection-error">{connection.last_error}</small>}
                {openDetails === connection.id && (
                  <div className="connection-details">
                    <ul>
                      {reasons.map(([reason, count]) => (
                        <li key={reason}><strong>{count.toLocaleString()}</strong> {REASONS[reason] ?? reason}</li>
                      ))}
                    </ul>
                    {detailFiles === null ? <p><LoaderCircle size={13} className="spin" /> <Shimmer>Loading file names…</Shimmer></p> : detailFiles.length > 0 && (
                      <details>
                        <summary>Show file names ({detailFiles.length}{detailFiles.length >= 300 ? "+" : ""})</summary>
                        <ul className="detail-files">
                          {detailFiles.map((file, index) => <li key={`${file.name}-${index}`}><span>{file.name}</span><small>{REASONS[file.reason] ?? file.reason}</small></li>)}
                        </ul>
                      </details>
                    )}
                  </div>
                )}
              </div>
              <span className={`scope-badge ${connection.scope === "private" ? "private" : ""}`} title={connection.scope === "private" ? "Only the person who connected it can search these files" : "Everyone at the firm can search these files, each finding what they may open"}>
                {connection.scope === "private" ? <><Lock size={12} /> Only {connection.yours ? "you" : "one person"}</> : <><Users size={12} /> Entire workspace</>}
              </span>
              <span className={`status-badge ${needsReconnect ? "warning" : "ready"}`}>
                {needsReconnect ? <AlertTriangle size={12} /> : syncing ? <LoaderCircle size={12} className="spin" /> : <Check size={12} />}
                {needsReconnect ? "Needs reconnecting" : syncing ? <Shimmer>Syncing…</Shimmer> : "Active"}
              </span>
              {(connection.scope === "private" ? connection.yours : canShare) && (
              <div className="connection-actions">
                {connection.yours && !isMail && (
                  connection.scope === "private"
                    ? canShare && <button className="secondary" disabled={busy} onClick={() => void changeScope(connection, name, "workspace")}><Users size={13} /> Share with workspace</button>
                    : <button className="secondary" disabled={busy} onClick={() => void changeScope(connection, name, "private")}><Lock size={13} /> Make private</button>
                )}
                {needsReconnect ? (
                  <button className="secondary" disabled={busy || waiting !== null} onClick={() => void connect(connection.kind, connection.id)}>Reconnect</button>
                ) : isMail ? null : (
                  <button className="secondary" aria-label={`Sync ${name} now`} disabled={busy} onClick={() => void syncNow(connection)}><RefreshCw size={13} /> Sync now</button>
                )}
                <button className="secondary danger" aria-label={`Disconnect ${name}`} disabled={busy} onClick={() => void disconnect(connection, name)}><Trash2 size={13} /> Disconnect</button>
              </div>
              )}
            </div>
          );
        })}
      </section>
    </div>
  );
}
