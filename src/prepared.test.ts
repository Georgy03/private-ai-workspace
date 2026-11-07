import { describe, expect, it } from "vitest";
import { MAX_LINK_CHARS, calendarFile, draftKey, prepareEmail, prepareMeeting } from "./prepared";
import type { PreparedAction } from "./types";

const email = { kind: "email", to: ["client@other.example"], cc: ["me@firm.com"], subject: "Budget & dates", body: "Hello,\n\nThe budget is 5,000 dollars." } as Extract<PreparedAction, { kind: "email" }>;
const meeting = { kind: "meeting", title: "Budget review", start: "2026-11-05T14:00", end: "2026-11-05T15:30", attendees: ["bob@firm.com", "client@other.example"], location: "Room 2", notes: "Bring the sheet; and the letter, please." } as Extract<PreparedAction, { kind: "meeting" }>;

describe("prepared emails", () => {
  it("open a new message in each mail service with everything filled in and nothing able to break out", () => {
    const gmail = prepareEmail("gmail", email);
    expect(gmail.url.startsWith("https://mail.google.com/mail/?view=cm")).toBe(true);
    const params = new URL(gmail.url).searchParams;
    expect([params.get("to"), params.get("cc"), params.get("su"), params.get("body")]).toEqual(["client@other.example", "me@firm.com", "Budget & dates", email.body]);
    expect(prepareEmail("outlook", email).url).toMatch(/^https:\/\/outlook\.office\.com\/mail\/deeplink\/compose\?to=/);
    const mailto = prepareEmail("app", email).url;
    expect(mailto.startsWith("mailto:client%40other.example?")).toBe(true);
    expect(mailto).not.toMatch(/\s/);
    // A subject that tries to add parameters or a line stays inside its own value.
    const sneaky = prepareEmail("gmail", { ...email, subject: "Hi&bcc=spy@evil.example\r\nBcc: x@y.com" }).url;
    expect(new URL(sneaky).searchParams.get("bcc")).toBeNull();
    expect(sneaky).not.toMatch(/[\r\n ]/);
  });

  it("leave a message that is too long for a link out of it, to be copied instead", () => {
    const long = { ...email, body: "word ".repeat(1000) };
    const prepared = prepareEmail("gmail", long);
    expect(prepared.url.length).toBeLessThanOrEqual(MAX_LINK_CHARS);
    expect(prepared.copyBody).toBe(long.body);
    expect(new URL(prepared.url).searchParams.get("su")).toBe(email.subject);
    expect(prepareEmail("gmail", email).copyBody).toBeUndefined();
  });
});

describe("prepared meetings", () => {
  it("fill in a new event in each calendar with the times as the person's own local time", () => {
    const google = new URL(prepareMeeting("google", meeting).url);
    expect(google.origin + google.pathname).toBe("https://calendar.google.com/calendar/render");
    expect(google.searchParams.get("dates")).toBe("20261105T140000/20261105T153000");
    expect([google.searchParams.get("text"), google.searchParams.get("add")]).toEqual(["Budget review", "bob@firm.com,client@other.example"]);
    const outlook = new URL(prepareMeeting("outlook", meeting).url);
    expect([outlook.searchParams.get("startdt"), outlook.searchParams.get("enddt")]).toEqual(["2026-11-05T14:00:00", "2026-11-05T15:30:00"]);
  });

  it("make a calendar file that cannot be made to add lines or fields", () => {
    const text = calendarFile({ ...meeting, title: "Review\r\nATTENDEE:mailto:spy@evil.example", notes: "a,b;c\nd" }, new Date("2026-11-03T10:00:00Z"));
    const lines = text.split("\r\n");
    expect(lines[0]).toBe("BEGIN:VCALENDAR");
    expect(lines.at(-2)).toBe("END:VCALENDAR");
    expect(lines).toContain("DTSTART:20261105T140000");
    expect(lines).toContain("DESCRIPTION:a\\,b\\;c\\nd");
    expect(lines.filter((line) => line.startsWith("ATTENDEE"))).toHaveLength(2);
    expect(lines.every((line) => line.length <= 75)).toBe(true);
    expect(lines).toContain("SUMMARY:Review\\nATTENDEE:mailto:spy@evil.example");
  });
});

describe("duplicate drafts", () => {
  it("are recognised by who they go to and what they are about, not by their wording", () => {
    const a = { ...email, to: ["Client@Other.example"], subject: "Budget & dates", body: "version one" };
    const b = { ...email, subject: " budget & DATES ", body: "a different draft" };
    expect(draftKey(a)).toBe(draftKey(b));
    expect(draftKey({ ...email, to: ["someone.else@other.example"] })).not.toBe(draftKey(email));
    expect(draftKey({ ...email, subject: "Another subject" })).not.toBe(draftKey(email));
    expect(draftKey(meeting)).not.toBe(draftKey(email));
  });
});
