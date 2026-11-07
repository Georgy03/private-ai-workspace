import { CalendarClock, LoaderCircle, Trash2, Video } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { cancelZoomMeeting, rescheduleZoomMeeting, zoomMeetings } from "./api";
import type { ZoomMeeting } from "./types";

const LENGTHS = [15, 30, 45, 60, 90];

function message(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong. Try again.";
}

function when(meeting: ZoomMeeting): string {
  const start = new Date(meeting.start);
  if (Number.isNaN(start.getTime())) return meeting.start;
  return `${start.toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })} · ${meeting.duration_minutes} min`;
}

/**
 * The Zoom meetings made from the app, which can be moved or cancelled here. Only these: the app
 * never lists or changes any other meeting in the person's Zoom. Nothing changes until they choose.
 */
export function ZoomMeetings() {
  const [meetings, setMeetings] = useState<ZoomMeeting[] | null>(null);
  const [moving, setMoving] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState<string | null>(null);
  const [date, setDate] = useState("");
  const [time, setTime] = useState("");
  const [minutes, setMinutes] = useState(30);
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const load = useCallback(() => zoomMeetings().then(setMeetings, (caught) => { setMeetings([]); setProblem(message(caught)); }), []);
  useEffect(() => { void load(); }, [load]);

  const startMoving = (meeting: ZoomMeeting) => {
    setMoving(meeting.id);
    setCancelling(null);
    setProblem(null);
    setDone(null);
    setDate(meeting.start.slice(0, 10));
    setTime(meeting.start.slice(11, 16));
    setMinutes(LENGTHS.includes(meeting.duration_minutes) ? meeting.duration_minutes : 30);
  };

  const move = (meeting: ZoomMeeting) => {
    if (!date || !time) { setProblem("Choose a date and a time."); return; }
    const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || meeting.timezone;
    setBusy(meeting.id);
    setProblem(null);
    rescheduleZoomMeeting(meeting.id, `${date}T${time}`, minutes, timezone).then(
      () => { setBusy(null); setMoving(null); setDone("Meeting moved. The link is the same, so tell the people invited the new time."); void load(); },
      (caught) => { setBusy(null); setProblem(message(caught)); void load(); },
    );
  };

  const cancel = (meeting: ZoomMeeting) => {
    setBusy(meeting.id);
    setProblem(null);
    cancelZoomMeeting(meeting.id).then(
      () => { setBusy(null); setCancelling(null); setDone("Meeting cancelled."); void load(); },
      (caught) => { setBusy(null); setProblem(message(caught)); void load(); },
    );
  };

  return (
    <div className="zoom-meetings" aria-label="Zoom meetings made here">
      <h4>Meetings made here</h4>
      {meetings === null && <p className="zoom-note"><LoaderCircle size={13} className="spin" /> Loading…</p>}
      {meetings !== null && meetings.length === 0 && <p className="zoom-note">Meetings you add to an email will show here, so you can move or cancel them.</p>}
      {meetings?.map((meeting) => (
        <div className="zoom-meeting" key={meeting.id}>
          <div className="zoom-meeting-line">
            <Video size={15} />
            <span><strong>{meeting.topic}</strong><small>{when(meeting)}</small></span>
            <button type="button" className="secondary" disabled={busy !== null} onClick={() => startMoving(meeting)}><CalendarClock size={13} /> Move</button>
            <button type="button" className="secondary danger" disabled={busy !== null} onClick={() => { setCancelling(meeting.id); setMoving(null); setProblem(null); setDone(null); }}><Trash2 size={13} /> Cancel meeting</button>
          </div>
          {moving === meeting.id && (
            <div className="zoom-edit">
              <input type="date" aria-label="New date" value={date} onChange={(event) => setDate(event.target.value)} />
              <input type="time" aria-label="New time" value={time} onChange={(event) => setTime(event.target.value)} />
              <select aria-label="New length" value={minutes} onChange={(event) => setMinutes(Number(event.target.value))}>
                {LENGTHS.map((length) => <option key={length} value={length}>{length} min</option>)}
              </select>
              <button type="button" disabled={busy !== null} onClick={() => move(meeting)}>{busy === meeting.id ? <><LoaderCircle size={13} className="spin" /> Moving…</> : "Save new time"}</button>
              <button type="button" className="secondary" disabled={busy !== null} onClick={() => setMoving(null)}>Keep as it is</button>
            </div>
          )}
          {cancelling === meeting.id && (
            <div className="zoom-edit">
              <span>Cancel this meeting? The link will stop working for everyone.</span>
              <button type="button" className="danger" disabled={busy !== null} onClick={() => cancel(meeting)}>{busy === meeting.id ? <><LoaderCircle size={13} className="spin" /> Cancelling…</> : "Yes, cancel it"}</button>
              <button type="button" className="secondary" disabled={busy !== null} onClick={() => setCancelling(null)}>Keep it</button>
            </div>
          )}
        </div>
      ))}
      {done && <p className="zoom-note" role="status">{done}</p>}
      {problem && <p className="connection-error" role="alert">{problem}</p>}
    </div>
  );
}
