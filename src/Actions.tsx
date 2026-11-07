import { useState } from "react";
import { CalendarPlus, Mail, TriangleAlert } from "lucide-react";
import { cloudActionApprove, cloudActionDismiss, cloudActionSend, cloudConnect, openCalendarFile, openPreparedLink } from "./api";
import { calendarFile, draftKey, prepareEmail, prepareMeeting, type CalendarService, type MailService } from "./prepared";
import type { PreparedAction, PreparedItem } from "./types";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");
const list = (text: string) => text.split(/[,;\n]/).map((part) => part.trim()).filter(Boolean);
const when = (start: string, end: string) => {
  const from = new Date(start);
  const until = new Date(end);
  const day = from.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" });
  const time = (d: Date) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return `${day}, ${time(from)} to ${time(until)}`;
};

/** Remembered on this device only: which mail and calendar the person uses. */
const remembered = <T extends string>(key: string, fallback: T): T => (localStorage.getItem(key) as T | null) ?? fallback;

/** Whether an email can be sent from the app: the firm allows it, and this person has connected Outlook. */
export interface Sender {
  enabled: boolean;
  /** The address it sends from, when Outlook is connected and working. */
  mailbox: string | null;
  needsReconnect: boolean;
}

function Card({ item, sender, others, onDone, onProblem, onNote }: { item: PreparedItem; sender: Sender; others: number; onDone: (note: string) => void; onProblem: (text: string) => void; onNote: (text: string) => void }) {
  const [editing, setEditing] = useState(false);
  const [checked, setChecked] = useState(false);
  const [draft, setDraft] = useState<PreparedAction>(item as PreparedAction);
  const [busy, setBusy] = useState(false);
  const isEmail = draft.kind === "email";

  const approveAndOpen = (open: (payload: PreparedAction) => Promise<string>) => {
    setBusy(true);
    cloudActionApprove(item.id, editing ? draft : undefined)
      .then((approved) => open(approved))
      .then(
        (note) => { setBusy(false); onDone(note); },
        (caught) => { setBusy(false); onProblem(message(caught)); },
      );
  };

  const withBodyCopied = async (copyBody: string | undefined, where: string) => {
    if (!copyBody) return `Opened in ${where}. Check it and press Send yourself.`;
    await navigator.clipboard?.writeText(copyBody).catch(() => undefined);
    return `Opened in ${where}. The message was too long to fit in a link, so it is copied: paste it into the message, then press Send yourself.`;
  };

  const openMail = (service: MailService, where: string) => {
    localStorage.setItem("prepared.mail", service);
    approveAndOpen(async (payload) => {
      if (payload.kind !== "email") return "";
      const prepared = prepareEmail(service, payload);
      await openPreparedLink(prepared.url);
      return withBodyCopied(prepared.copyBody, where);
    });
  };

  const openCalendar = (service: CalendarService, where: string) => {
    localStorage.setItem("prepared.calendar", service);
    approveAndOpen(async (payload) => {
      if (payload.kind !== "meeting") return "";
      if (service === "file") {
        await openCalendarFile(calendarFile(payload));
        return "Opened as a calendar file. Your calendar will ask you to add it.";
      }
      const prepared = prepareMeeting(service, payload);
      await openPreparedLink(prepared.url);
      return withBodyCopied(prepared.copyBody, where);
    });
  };

  const canSend = item.kind === "email" && sender.enabled && sender.mailbox !== null;
  const sendFromApp = () => {
    setBusy(true);
    cloudActionSend(item.id, editing ? draft : undefined, checked).then(
      (result) => { setBusy(false); onDone(`Sent from ${result.from ?? sender.mailbox ?? "your Outlook"}. It is in your Sent folder.${others > 0 ? " Another draft like it is still waiting below: dismiss it if you do not need it." : ""}`); },
      (caught) => { setBusy(false); onProblem(message(caught)); },
    );
  };
  const connectOutlook = () =>
    cloudConnect("outlook_mail", [], undefined, "private").then(
      () => onNote("Finish signing in to Outlook in your browser, then come back here."),
      (caught) => onProblem(message(caught)),
    );

  const dismiss = () => {
    setBusy(true);
    cloudActionDismiss(item.id).then(
      () => { setBusy(false); onDone("Dismissed."); },
      (caught) => { setBusy(false); onProblem(message(caught)); },
    );
  };

  const set = <K extends string>(key: K, value: unknown) => setDraft((old) => ({ ...old, [key]: value }) as PreparedAction);
  const preferredMail = remembered<MailService>("prepared.mail", "gmail");
  const preferredCalendar = remembered<CalendarService>("prepared.calendar", "google");
  const ordered = <T extends string>(options: { id: T; label: string; where: string }[], first: T) => [...options].sort((a, b) => (a.id === first ? -1 : b.id === first ? 1 : 0));

  return (
    <article className="action-card" aria-label={isEmail ? `Email: ${draft.subject}` : `Meeting: ${draft.title}`}>
      <header>
        <span className="action-icon">{isEmail ? <Mail size={18} /> : <CalendarPlus size={18} />}</span>
        <div>
          <strong>{item.agent_name} prepared {isEmail ? "an email" : "a meeting"} for you</strong>
          <small>From your task “{item.task_title}” · prepared {new Date(item.created_epoch * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</small>
        </div>
      </header>
      <p className="action-reassure">Nothing has been {isEmail ? "sent" : "booked"}. Check it, then open it in your own {isEmail ? "email" : "calendar"} and {isEmail ? "press Send" : "save it"} yourself.</p>
      {others > 0 && (
        <p className="action-warning" role="note">
          <TriangleAlert size={14} /> You have {others === 1 ? "another draft" : `${others} other drafts`} to the same {isEmail ? "people with the same subject" : "meeting"}. Check you are using the one you mean, and dismiss the rest.
        </p>
      )}
      {item.outside.length > 0 && (
        <p className="action-warning" role="alert"><TriangleAlert size={14} /> This goes to {item.outside.length === 1 ? "someone" : "people"} outside your organisation: {item.outside.join(", ")}</p>
      )}

      {editing ? (
        <div className="action-edit">
          {draft.kind === "email" ? (
            <>
              <label>To<input value={draft.to.join(", ")} onChange={(event) => set("to", list(event.target.value))} /></label>
              <label>Cc<input value={draft.cc.join(", ")} onChange={(event) => set("cc", list(event.target.value))} /></label>
              <label>Subject<input value={draft.subject} onChange={(event) => set("subject", event.target.value)} /></label>
              <label>Message<textarea rows={8} value={draft.body} onChange={(event) => set("body", event.target.value)} /></label>
            </>
          ) : (
            <>
              <label>Title<input value={draft.title} onChange={(event) => set("title", event.target.value)} /></label>
              <label>Starts<input type="datetime-local" value={draft.start} onChange={(event) => set("start", event.target.value)} /></label>
              <label>Ends<input type="datetime-local" value={draft.end} onChange={(event) => set("end", event.target.value)} /></label>
              <label>Who is invited<input value={draft.attendees.join(", ")} onChange={(event) => set("attendees", list(event.target.value))} /></label>
              <label>Where<input value={draft.location} onChange={(event) => set("location", event.target.value)} /></label>
              <label>Notes<textarea rows={4} value={draft.notes} onChange={(event) => set("notes", event.target.value)} /></label>
            </>
          )}
        </div>
      ) : draft.kind === "email" ? (
        <dl className="action-preview">
          <dt>To</dt><dd>{draft.to.join(", ")}</dd>
          {draft.cc.length > 0 && <><dt>Cc</dt><dd>{draft.cc.join(", ")}</dd></>}
          <dt>Subject</dt><dd>{draft.subject}</dd>
          <dt>Message</dt><dd className="action-body">{draft.body}</dd>
        </dl>
      ) : (
        <dl className="action-preview">
          <dt>Meeting</dt><dd>{draft.title}</dd>
          <dt>When</dt><dd>{when(draft.start, draft.end)}</dd>
          {draft.attendees.length > 0 && <><dt>Invited</dt><dd>{draft.attendees.join(", ")}</dd></>}
          {draft.location && <><dt>Where</dt><dd>{draft.location}</dd></>}
          {draft.notes && <><dt>Notes</dt><dd className="action-body">{draft.notes}</dd></>}
        </dl>
      )}

      {canSend && (
        <div className="action-send" role="group" aria-label="Send this email">
          <p>Sends from <strong>{sender.mailbox}</strong>. It sends exactly what you see above.</p>
          {item.outside.length > 0 && (
            <label className="check"><input type="checkbox" checked={checked} onChange={(event) => setChecked(event.target.checked)} /> I have checked the people outside my organisation</label>
          )}
          <button className="primary big" disabled={busy || (item.outside.length > 0 && !checked)} onClick={sendFromApp}>Send</button>
        </div>
      )}
      {item.kind === "email" && !canSend && (
        <p className="action-hint">
          {!sender.enabled
            ? "Sending from the app is switched off for your firm. You can still open it in your own email below."
            : sender.needsReconnect
              ? <>Outlook needs you to sign in again to send from here. <button className="link-button" onClick={() => void connectOutlook()}>Reconnect Outlook</button></>
              : <>Want to send it from here? <button className="link-button" onClick={() => void connectOutlook()}>Connect your Outlook</button> once, and you can read the draft and press Send.</>}
        </p>
      )}
      {canSend && <p className="action-or">Or open it in your own email instead:</p>}
      <div className="action-buttons">
        {isEmail
          ? ordered<MailService>([{ id: "gmail", label: "Open in Gmail", where: "Gmail" }, { id: "outlook", label: "Open in Outlook", where: "Outlook" }, { id: "app", label: "Open in my mail app", where: "your mail app" }], preferredMail).map((option, index) => (
              <button key={option.id} className={index === 0 && !canSend ? "primary" : "secondary"} disabled={busy} onClick={() => openMail(option.id, option.where)}>{option.label}</button>
            ))
          : ordered<CalendarService>([{ id: "google", label: "Open in Google Calendar", where: "Google Calendar" }, { id: "outlook", label: "Open in Outlook Calendar", where: "Outlook Calendar" }, { id: "file", label: "Download a calendar file", where: "your calendar" }], preferredCalendar).map((option, index) => (
              <button key={option.id} className={index === 0 ? "primary" : "secondary"} disabled={busy} onClick={() => openCalendar(option.id, option.where)}>{option.label}</button>
            ))}
        <button className="ghost" disabled={busy} onClick={() => setEditing((on) => !on)}>{editing ? "Stop editing" : "Edit first"}</button>
        <button className="ghost" disabled={busy} onClick={dismiss}>Dismiss</button>
      </div>
    </article>
  );
}

/** Emails and meetings agents prepared, waiting for the person to check and open in their own apps. */
export function ActionsSection({ items, sender, onChanged }: { items: PreparedItem[]; sender: Sender; onChanged: () => void }) {
  const [note, setNote] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  if (items.length === 0 && !note) return null;
  return (
    <section aria-label="Prepared for you" className="agent-actions">
      <h3>Prepared for you {items.length > 0 && <span className="inbox-count">{items.length}</span>}</h3>
      {note && <p className="action-note" role="status">{note}</p>}
      {problem && <p className="knowledge-error" role="alert">{problem}</p>}
      <div className="action-list">
        {items.map((item) => (
          <Card key={item.id} item={item} sender={sender} others={items.filter((other) => other.id !== item.id && draftKey(other) === draftKey(item)).length} onProblem={setProblem} onNote={setNote} onDone={(text) => { setProblem(null); setNote(text); onChanged(); }} />
        ))}
      </div>
    </section>
  );
}
