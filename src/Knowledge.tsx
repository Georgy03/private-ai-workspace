import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Check, ExternalLink, LoaderCircle, Pencil, X } from "lucide-react";
import {
  cloudKnowledge,
  cloudKnowledgeEnable,
  cloudKnowledgeEntities,
  cloudKnowledgeFacts,
  cloudKnowledgeProfileReview,
  cloudKnowledgeReview,
} from "./api";
import type { CloudStatus, KnowledgeEntity, KnowledgeEvidence, KnowledgeFact, KnowledgeOverview } from "./types";

type Tab = "verify" | "confirmed" | "entities";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const label = (text: string) => text.replace(/_/g, " ").replace(/^./, (letter) => letter.toUpperCase());

function Evidence({ items, onOpenLink }: { items: KnowledgeEvidence[]; onOpenLink: (url: string) => void }) {
  if (items.length === 0) return null;
  return (
    <ul className="knowledge-evidence" aria-label="Where this came from">
      {items.map((item) => (
        <li key={`${item.document_id}-${item.quote.slice(0, 20)}`}>
          <div className="evidence-head">
            <strong>{item.document_name}</strong>
            {item.source_changed && <em className="knowledge-badge warn">File changed since</em>}
            {item.link && (
              <button className="open-link" onClick={() => onOpenLink(item.link!)} aria-label={`Open ${item.document_name}`}>
                <ExternalLink size={12} /> Open
              </button>
            )}
          </div>
          <blockquote>“{item.quote}”</blockquote>
        </li>
      ))}
    </ul>
  );
}

/** One thing the AI found, with where it found it and what a person can do about it. */
function ReviewCard({
  title,
  badges,
  evidence,
  editable,
  busy,
  onAct,
  onOpenLink,
  meta,
}: {
  title: string;
  badges: { text: string; tone?: "ok" | "warn" }[];
  evidence: KnowledgeEvidence[];
  editable: boolean;
  busy: boolean;
  onAct: (action: "confirm" | "reject" | "correct", text?: string) => void;
  onOpenLink: (url: string) => void;
  meta?: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(title);
  return (
    <article className="knowledge-card">
      {editing ? (
        <div className="knowledge-edit">
          <textarea aria-label="Corrected text" value={draft} rows={2} onChange={(event) => setDraft(event.target.value)} />
          <div className="knowledge-actions">
            <button className="primary" disabled={busy || draft.trim().length < 3} onClick={() => { onAct("correct", draft.trim()); setEditing(false); }}>Save and confirm</button>
            <button className="secondary" onClick={() => setEditing(false)}>Cancel</button>
          </div>
        </div>
      ) : (
        <>
          <p className="knowledge-statement">{title}</p>
          <div className="knowledge-badges">
            {badges.map((badge) => <em key={badge.text} className={`knowledge-badge ${badge.tone ?? ""}`}>{badge.text}</em>)}
            {meta && <small>{meta}</small>}
          </div>
        </>
      )}
      <Evidence items={evidence} onOpenLink={onOpenLink} />
      {editable && !editing && (
        <div className="knowledge-actions">
          <button className="secondary" disabled={busy} onClick={() => onAct("confirm")}><Check size={14} /> Confirm</button>
          <button className="secondary" disabled={busy} onClick={() => { setDraft(title); setEditing(true); }}><Pencil size={14} /> Edit</button>
          <button className="secondary danger" disabled={busy} onClick={() => onAct("reject")}><X size={14} /> Not true</button>
        </div>
      )}
    </article>
  );
}

function factBadges(fact: KnowledgeFact): { text: string; tone?: "ok" | "warn" }[] {
  const badges: { text: string; tone?: "ok" | "warn" }[] = [{ text: label(fact.kind) }];
  if (fact.status === "verified") badges.push({ text: "Confirmed", tone: "ok" });
  if (fact.status === "suggested") badges.push({ text: "Needs verification", tone: "warn" });
  if (fact.status === "outdated") badges.push({ text: "Outdated: its source changed or was removed", tone: "warn" });
  if (fact.needs_review && fact.status === "verified") badges.push({ text: "Check again: its source changed", tone: "warn" });
  if (fact.stale) badges.push({ text: "Not confirmed for a long time", tone: "warn" });
  return badges;
}

export function KnowledgeView({ cloud, onOpenLink }: { cloud: CloudStatus; onOpenLink: (url: string) => void }) {
  const [overview, setOverview] = useState<KnowledgeOverview | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("verify");
  const [facts, setFacts] = useState<KnowledgeFact[]>([]);
  const [entities, setEntities] = useState<KnowledgeEntity[]>([]);
  const [busy, setBusy] = useState(false);
  const [understood, setUnderstood] = useState(false);
  const [editingProfile, setEditingProfile] = useState(false);
  const [summaryDraft, setSummaryDraft] = useState("");
  const [servicesDraft, setServicesDraft] = useState("");

  const usable = cloud.configured && cloud.signed_in;

  const loadOverview = useCallback(
    () =>
      cloudKnowledge().then(
        (next) => {
          setOverview(next);
          setProblem(null);
          return next;
        },
        (caught) => {
          setProblem(message(caught));
          return null;
        },
      ),
    [],
  );

  const loadList = useCallback(
    (which: Tab) =>
      (which === "entities"
        ? cloudKnowledgeEntities().then(setEntities)
        : cloudKnowledgeFacts(which === "verify" ? { reviewOnly: true } : { status: "verified" }).then(setFacts)
      ).catch((caught) => setProblem(message(caught))),
    [],
  );

  useEffect(() => {
    if (usable) void loadOverview();
  }, [usable, loadOverview]);

  const enabled = overview?.enabled === true;
  useEffect(() => {
    if (usable && enabled) void loadList(tab);
  }, [usable, enabled, tab, loadList]);

  // While documents are still being read, look again every so often.
  const reading = enabled && overview !== null && overview.overview.documents_analysed < overview.overview.documents_total;
  useEffect(() => {
    if (!usable || !reading) return;
    const timer = setInterval(() => void loadOverview(), 15000);
    return () => clearInterval(timer);
  }, [usable, reading, loadOverview]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
      await Promise.all([loadOverview(), loadList(tab)]);
    } catch (caught) {
      setProblem(message(caught));
    } finally {
      setBusy(false);
    }
  };

  if (!cloud.configured || !cloud.signed_in) {
    return (
      <div className="knowledge-view">
        <div className="page-heading"><div><span className="eyebrow">Firm cloud index</span><h1>Company knowledge</h1></div></div>
        <div className="connectors-empty"><strong>Sign in to see company knowledge.</strong><p>It is built from the documents your firm has connected, so it needs the firm cloud index.</p></div>
      </div>
    );
  }
  if (problem && overview === null) {
    return (
      <div className="knowledge-view">
        <div className="page-heading"><div><span className="eyebrow">Firm cloud index</span><h1>Company knowledge</h1></div></div>
        <p className="connector-problem" role="alert"><AlertTriangle size={14} /> {problem}</p>
      </div>
    );
  }
  if (overview === null) {
    return (
      <div className="knowledge-view">
        <div className="page-heading"><div><span className="eyebrow">Firm cloud index</span><h1>Company knowledge</h1></div></div>
        <p className="connectors-loading"><LoaderCircle size={15} className="spin" /> Loading…</p>
      </div>
    );
  }

  const o = overview.overview;
  const profile = overview.profile;

  if (!overview.enabled) {
    return (
      <div className="knowledge-view">
        <div className="page-heading"><div><span className="eyebrow">Firm cloud index</span><h1>Company knowledge</h1><p>What your documents say about your business, with the source of every fact.</p></div></div>
        <section className="knowledge-intro">
          <h2>Let the AI learn your business from your documents</h2>
          <p>It reads your connected documents and builds a profile of what your business does, the customers, vendors, people and projects it mentions, and the facts, procedures and policies written down. Nothing is trusted until a person confirms it, and every item links back to the passage it came from.</p>
          <ul>
            <li>Turning this on lets your firm's server send the <strong>text of your connected documents</strong> to Claude, one document at a time, to read them.</li>
            <li>It costs roughly <strong>$1 to $2 for every 1,000 documents</strong>, once, plus a little for documents that change later.</li>
            <li>Staff only ever see confirmed facts, and only from documents they can already open.</li>
            <li>It uses files in the firm cloud index only. Files on people's own computers are never included.</li>
          </ul>
          {!overview.analysis_available && <p className="connector-problem"><AlertTriangle size={14} /> This server has no Claude key set, so it cannot read documents yet.</p>}
          <label className="toggle">
            <input type="checkbox" checked={understood} onChange={(event) => setUnderstood(event.target.checked)} />
            I understand my documents' text will be sent to Claude to build this.
          </label>
          <button className="primary" disabled={!understood || busy || !overview.analysis_available} onClick={() => void run(() => cloudKnowledgeEnable(true))}>
            {busy ? "Turning on…" : "Turn on company knowledge"}
          </button>
          {problem && <p className="connector-problem" role="alert"><AlertTriangle size={14} /> {problem}</p>}
        </section>
      </div>
    );
  }

  const progress = o.documents_total > 0 ? Math.round((o.documents_analysed / o.documents_total) * 100) : 0;
  const recheck = o.facts_needing_review + o.facts_stale;
  const startEditing = () => {
    setSummaryDraft(profile?.summary ?? "");
    setServicesDraft((profile?.services ?? []).join(", "));
    setEditingProfile(true);
  };

  return (
    <div className="knowledge-view">
      <div className="page-heading">
        <div><span className="eyebrow">Firm cloud index</span><h1>Company knowledge</h1><p>What the AI understands about your business, where it learned it, and what still needs a person to check.</p></div>
        <button className="secondary" disabled={busy} onClick={() => void run(() => cloudKnowledgeEnable(false))}>Turn off</button>
      </div>
      {problem && <p className="connector-problem" role="alert"><AlertTriangle size={14} /> {problem}</p>}
      <p className="knowledge-note">This is drawn from every document connected to the firm, including ones some staff cannot open, so only administrators see this page. Staff see only confirmed facts from documents they can open.</p>

      <section className="knowledge-profile" aria-label="Business profile">
        <header>
          <h2>What the AI understands about your business</h2>
          {profile && <em className={`knowledge-badge ${profile.status === "verified" ? "ok" : "warn"}`}>{profile.status === "verified" ? "Confirmed" : "Needs verification"}</em>}
        </header>
        {!profile ? (
          <p className="home-empty">Not enough has been read yet. A profile appears once about ten documents have been read.</p>
        ) : editingProfile ? (
          <div className="knowledge-edit">
            <label>What the business does<textarea aria-label="What the business does" rows={3} value={summaryDraft} onChange={(event) => setSummaryDraft(event.target.value)} /></label>
            <label>Services or products, separated by commas<input aria-label="Services or products" value={servicesDraft} onChange={(event) => setServicesDraft(event.target.value)} /></label>
            <div className="knowledge-actions">
              <button className="primary" disabled={busy || summaryDraft.trim() === ""} onClick={() => void run(async () => { await cloudKnowledgeProfileReview(summaryDraft.trim(), servicesDraft.split(",").map((s) => s.trim()).filter(Boolean)); setEditingProfile(false); })}>Save and confirm</button>
              <button className="secondary" onClick={() => setEditingProfile(false)}>Cancel</button>
            </div>
          </div>
        ) : (
          <>
            <p className="knowledge-statement">{profile.summary}</p>
            {profile.services.length > 0 && <div className="chips" aria-label="Services">{profile.services.map((service) => <span key={service}>{service}</span>)}</div>}
            {profile.terminology.length > 0 && (
              <dl className="terms">{profile.terminology.map((entry) => <div key={entry.term}><dt>{entry.term}</dt><dd>{entry.meaning}</dd></div>)}</dl>
            )}
            <small>Drawn from {profile.based_on.toLocaleString()} documents.</small>
            <div className="knowledge-actions">
              {profile.status !== "verified" && <button className="secondary" disabled={busy} onClick={() => void run(() => cloudKnowledgeProfileReview())}><Check size={14} /> This is right</button>}
              <button className="secondary" disabled={busy} onClick={startEditing}><Pencil size={14} /> Edit</button>
            </div>
          </>
        )}
      </section>

      <section className="knowledge-progress" aria-label="Reading progress">
        <div className="usage-head"><strong>Read {o.documents_analysed.toLocaleString()} of {o.documents_total.toLocaleString()} documents</strong><span>{progress}%</span></div>
        <div className="usage-track"><div style={{ width: `${progress}%` }} /></div>
        {o.documents_failed > 0 && <small>{o.documents_failed} could not be read and will be tried again.</small>}
        {o.document_types.length > 0 && <div className="chips" aria-label="Kinds of document">{o.document_types.slice(0, 8).map(([kind, count]) => <span key={kind}>{label(kind)} · {count}</span>)}</div>}
      </section>

      <div className="knowledge-stats">
        <div><strong>{o.facts_unverified.toLocaleString()}</strong><span>facts to verify</span></div>
        <div><strong>{recheck.toLocaleString()}</strong><span>to check again</span></div>
        <div><strong>{o.facts_verified.toLocaleString()}</strong><span>confirmed facts</span></div>
        <div><strong>{o.entities_by_kind.reduce((sum, [, count]) => sum + count, 0).toLocaleString()}</strong><span>people, companies and projects</span></div>
      </div>

      <div className="source-tabs" role="tablist" aria-label="Company knowledge">
        <button role="tab" aria-selected={tab === "verify"} className={tab === "verify" ? "active" : ""} onClick={() => setTab("verify")}>Needs verification</button>
        <button role="tab" aria-selected={tab === "confirmed"} className={tab === "confirmed" ? "active" : ""} onClick={() => setTab("confirmed")}>Confirmed facts</button>
        <button role="tab" aria-selected={tab === "entities"} className={tab === "entities" ? "active" : ""} onClick={() => setTab("entities")}>People and companies</button>
      </div>

      <div className="knowledge-list">
        {tab !== "entities" && facts.length === 0 && <p className="connectors-none">{tab === "verify" ? "Nothing needs checking right now." : "No facts have been confirmed yet."}</p>}
        {tab !== "entities" && facts.map((fact) => (
          <ReviewCard
            key={fact.id}
            title={fact.statement}
            badges={factBadges(fact)}
            meta={fact.subject ? `About ${fact.subject}` : undefined}
            evidence={fact.evidence}
            editable
            busy={busy}
            onOpenLink={onOpenLink}
            onAct={(action, text) => void run(() => cloudKnowledgeReview("fact", fact.id, action, text))}
          />
        ))}
        {tab === "entities" && entities.length === 0 && <p className="connectors-none">Nothing found yet.</p>}
        {tab === "entities" && entities.map((entity) => (
          <ReviewCard
            key={entity.id}
            title={entity.name}
            badges={[{ text: label(entity.kind) }, ...entity.roles.map((role) => ({ text: label(role) })), ...(entity.status === "verified" ? [{ text: "Confirmed", tone: "ok" as const }] : [{ text: "Needs verification", tone: "warn" as const }])]}
            meta={`In ${entity.mentions} document${entity.mentions === 1 ? "" : "s"}`}
            evidence={entity.evidence}
            editable
            busy={busy}
            onOpenLink={onOpenLink}
            onAct={(action, text) => void run(() => cloudKnowledgeReview("entity", entity.id, action, text))}
          />
        ))}
      </div>
    </div>
  );
}
