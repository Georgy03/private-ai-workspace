import { useState } from "react";
import { Check, ExternalLink, LoaderCircle, Mail, Send, Video } from "lucide-react";
import { createZoomMeeting, openPreparedLink, sendChatEmail } from "./api";
import { withZoomLink } from "./zoomText";
import { prepareEmail } from "./prepared";
import type { EmailDraft } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const valid = (address: string) => /^[^\s@,;<>"]+@[^\s@,;<>"]+\.[^\s@,;<>"]+$/.test(address);
const split = (text: string) => text.split(/[,;\s]+/).map((part) => part.trim()).filter(Boolean);

/**
 * An email written from the person's request, shown as a preview they can change before sending. It
 * goes from their own Outlook, and only when they press Send.
 */
export function EmailPreview({ draft }: { draft: EmailDraft }) {
  const [to, setTo] = useState(draft.to.join(", "));
  const [subject, setSubject] = useState(draft.subject);
  const [body, setBody] = useState(draft.body);
  const [state, setState] = useState<"editing" | "sending" | "sent">("editing");
  const [from, setFrom] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [needsConfirm, setNeedsConfirm] = useState(false);
  // A meeting the email is about: a Zoom link can be made for it, in the person's own Zoom.
  const meeting = draft.meeting ?? null;
  const [minutes, setMinutes] = useState(meeting?.duration_minutes ?? 30);
  const [linking, setLinking] = useState(false);
  const [linked, setLinked] = useState(false);

  const people = split(to);
  const ready = people.length > 0 && people.every(valid) && subject.trim().length > 0 && body.trim().length > 0;

  const send = (confirmOutside: boolean) => {
    setState("sending");
    setProblem(null);
    sendChatEmail({ to: people, cc: [], subject: subject.trim(), body }, confirmOutside).then(
      (result) => { setFrom(result.from ?? null); setState("sent"); setNeedsConfirm(false); },
      (caught) => {
        const text = message(caught);
        setState("editing");
        // Someone outside the firm: ask once more, naming who, before it goes.
        if (/outside your organi[sz]ation/i.test(text)) setNeedsConfirm(true);
        else setProblem(text);
      },
    );
  };

  // For when Outlook is not connected, or the person would rather send it from their own mail.
  const openInMail = () => {
    const prepared = prepareEmail("outlook", { kind: "email", to: people, cc: [], subject, body });
    openPreparedLink(prepared.url).catch((caught) => setProblem(message(caught)));
  };

  const addZoomLink = () => {
    if (!meeting) return;
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    setLinking(true);
    setProblem(null);
    createZoomMeeting({ topic: meeting.topic, start: meeting.start, duration_minutes: minutes, timezone, agenda: "" }).then(
      (zoom) => { setBody((text) => withZoomLink(text, zoom, timezone)); setLinked(true); setLinking(false); },
      (caught) => { setLinking(false); setProblem(message(caught)); },
    );
  };

  const when = meeting ? new Date(meeting.start).toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" }) : "";
  const locked = state !== "editing";
  return (
    <section className={`email-card ${state === "sent" ? "is-sent" : ""}`} aria-label="Email preview">
      <header>
        <span className="email-icon"><Mail size={17} /></span>
        <span className="email-heading"><strong>Email draft</strong><small>{state === "sent" ? "Delivered from your Outlook" : "Check it, change anything, then send from your Outlook"}</small></span>
        {state === "sent" && <span className="email-sent" role="status"><Check size={14} /> Sent{from ? ` from ${from}` : ""}</span>}
      </header>
      <div className="email-fields">
        <div className="email-field"><span aria-hidden="true">To</span><input aria-label="To" value={to} disabled={locked} placeholder="name@example.com" onChange={(event) => { setTo(event.target.value); setNeedsConfirm(false); }} /></div>
        <div className="email-field"><span aria-hidden="true">Subject</span><input aria-label="Subject" value={subject} disabled={locked} maxLength={200} onChange={(event) => setSubject(event.target.value)} /></div>
        <textarea className="email-body" aria-label="Message body" rows={10} value={body} disabled={locked} onChange={(event) => setBody(event.target.value)} />
      </div>
      {meeting && state !== "sent" && (
        <div className={`email-zoom ${linked ? "added" : ""}`} aria-label="Zoom meeting">
          <span className="email-zoom-icon"><Video size={16} /></span>
          <span className="email-zoom-text"><strong>{linked ? "Zoom link added" : "Zoom meeting"}</strong><small>{when}{linked ? ` · ${minutes} min` : ""}</small></span>
          {!linked && (
            <>
              <label className="visually-hidden" htmlFor="zoom-length">Length</label>
              <select id="zoom-length" aria-label="Length" value={minutes} disabled={linking} onChange={(event) => setMinutes(Number(event.target.value))}>
                {[15, 30, 45, 60, 90].map((length) => <option key={length} value={length}>{length} min</option>)}
              </select>
              <button type="button" className="secondary" disabled={linking || locked} onClick={addZoomLink}>{linking ? <><LoaderCircle size={14} className="spin" /> Creating…</> : "Add a Zoom link"}</button>
            </>
          )}
          {linked && <Check size={16} className="email-zoom-check" />}
        </div>
      )}
      {people.length > 0 && !people.every(valid) && <p className="email-problem">One of the addresses does not look right. Separate several with commas.</p>}
      {problem && <p className="email-problem" role="alert">{problem}</p>}
      {needsConfirm && (
        <p className="email-confirm" role="alert">
          This goes to someone outside your organisation ({people.join(", ")}). Check the address before sending.
          <button type="button" className="primary" disabled={state === "sending"} onClick={() => send(true)}>Send anyway</button>
        </p>
      )}
      {state !== "sent" && (
        <div className="email-actions">
          <button type="button" className="primary" disabled={!ready || state === "sending"} onClick={() => send(false)}>
            {state === "sending" ? <><LoaderCircle size={14} className="spin" /> Sending…</> : <><Send size={14} /> Send</>}
          </button>
          <button type="button" className="secondary" disabled={!ready} onClick={openInMail} title="Open this message in Outlook on the web, to send it from there"><ExternalLink size={14} /> Open in Outlook</button>
        </div>
      )}
    </section>
  );
}
