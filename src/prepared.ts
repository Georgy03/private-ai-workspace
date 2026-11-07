import type { PreparedAction } from "./types";

/**
 * Turns an email or a meeting an agent prepared into something the person's own mail or calendar can
 * open already filled in. Nothing here sends or books anything: the person presses Send, or Save,
 * in their own app. The addresses are checked again by the desktop app before they are opened.
 */

type Email = Extract<PreparedAction, { kind: "email" }>;
type Meeting = Extract<PreparedAction, { kind: "meeting" }>;

/** Web addresses much longer than this are cut off by some mail services. */
export const MAX_LINK_CHARS = 1800;

const enc = encodeURIComponent;

/** "2026-11-05T14:00" as "20261105T140000", the form Google Calendar wants. */
const compact = (time: string) => time.replace(/[-:]/g, "").padEnd(15, "0").slice(0, 15);
const withSeconds = (time: string) => (time.length === 16 ? `${time}:00` : time);

export type MailService = "gmail" | "outlook" | "app";
export type CalendarService = "google" | "outlook" | "file";

export interface Prepared {
  /** What to open. */
  url: string;
  /** The message was too long for a link, so it was left out of it and should be copied for pasting. */
  copyBody?: string;
}

function mailUrl(service: MailService, email: Email, body: string): string {
  const to = email.to.join(service === "outlook" ? ";" : ",");
  const cc = email.cc.join(service === "outlook" ? ";" : ",");
  if (service === "gmail") {
    return `https://mail.google.com/mail/?view=cm&fs=1&to=${enc(to)}${cc ? `&cc=${enc(cc)}` : ""}&su=${enc(email.subject)}&body=${enc(body)}`;
  }
  if (service === "outlook") {
    return `https://outlook.office.com/mail/deeplink/compose?to=${enc(to)}${cc ? `&cc=${enc(cc)}` : ""}&subject=${enc(email.subject)}&body=${enc(body)}`;
  }
  return `mailto:${email.to.map(enc).join(",")}?${cc ? `cc=${email.cc.map(enc).join(",")}&` : ""}subject=${enc(email.subject)}&body=${enc(body)}`;
}

/** A link that opens a new message in the chosen mail. A message too long for a link opens with the
 *  recipients and subject only, and the text is handed back to be copied. */
export function prepareEmail(service: MailService, email: Email): Prepared {
  const whole = mailUrl(service, email, email.body);
  if (whole.length <= MAX_LINK_CHARS) return { url: whole };
  return { url: mailUrl(service, email, ""), copyBody: email.body };
}

export function prepareMeeting(service: Exclude<CalendarService, "file">, meeting: Meeting): Prepared {
  const details = meeting.notes;
  if (service === "google") {
    const url = `https://calendar.google.com/calendar/render?action=TEMPLATE&text=${enc(meeting.title)}&dates=${compact(meeting.start)}/${compact(meeting.end)}${meeting.location ? `&location=${enc(meeting.location)}` : ""}&details=${enc(details)}${meeting.attendees.length ? `&add=${enc(meeting.attendees.join(","))}` : ""}`;
    if (url.length <= MAX_LINK_CHARS) return { url };
    return { url: url.replace(/&details=[^&]*/, "&details="), copyBody: details };
  }
  const build = (text: string) =>
    `https://outlook.office.com/calendar/0/deeplink/compose?path=${enc("/calendar/action/compose")}&rru=addevent&subject=${enc(meeting.title)}&startdt=${enc(withSeconds(meeting.start))}&enddt=${enc(withSeconds(meeting.end))}${meeting.location ? `&location=${enc(meeting.location)}` : ""}&body=${enc(text)}${meeting.attendees.length ? `&to=${enc(meeting.attendees.join(";"))}` : ""}`;
  const url = build(details);
  return url.length <= MAX_LINK_CHARS ? { url } : { url: build(""), copyBody: details };
}

/** Text escaped as a calendar file requires, so nothing in it can add a line or a field. */
const ics = (text: string) => text.replace(/\\/g, "\\\\").replace(/;/g, "\\;").replace(/,/g, "\\,").replace(/\r?\n/g, "\\n");

/** Lines longer than 75 characters are continued on the next line, as the calendar file format asks. */
function fold(line: string): string {
  const parts: string[] = [];
  let rest = line;
  while (rest.length > 74) {
    parts.push(rest.slice(0, 74));
    rest = ` ${rest.slice(74)}`;
  }
  parts.push(rest);
  return parts.join("\r\n");
}

/** A calendar file for the meeting, which the person's calendar app opens and asks them to add. */
export function calendarFile(meeting: Meeting, now = new Date()): string {
  const stamp = now.toISOString().replace(/[-:]/g, "").replace(/\.\d+/, "");
  const uid = `${stamp}-${Math.abs([...meeting.title].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 7))}@private-ai-workspace`;
  const lines = [
    "BEGIN:VCALENDAR",
    "VERSION:2.0",
    "PRODID:-//Private AI Workspace//EN",
    "METHOD:PUBLISH",
    "BEGIN:VEVENT",
    `UID:${uid}`,
    `DTSTAMP:${stamp}`,
    `DTSTART:${compact(meeting.start)}`,
    `DTEND:${compact(meeting.end)}`,
    `SUMMARY:${ics(meeting.title)}`,
    ...(meeting.location ? [`LOCATION:${ics(meeting.location)}`] : []),
    ...(meeting.notes ? [`DESCRIPTION:${ics(meeting.notes)}`] : []),
    ...meeting.attendees.map((address) => `ATTENDEE:mailto:${address}`),
    "END:VEVENT",
    "END:VCALENDAR",
  ];
  return `${lines.map(fold).join("\r\n")}\r\n`;
}

/** The same email or meeting prepared more than once: same kind, same people, same subject or title. */
export function draftKey(item: PreparedAction): string {
  return item.kind === "email"
    ? `email|${[...item.to, ...item.cc].map((a) => a.toLowerCase()).sort().join(",")}|${item.subject.trim().toLowerCase()}`
    : `meeting|${item.title.trim().toLowerCase()}|${item.start}|${item.end}`;
}
