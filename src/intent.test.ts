import { describe, expect, it } from "vitest";
import { wantsEmail, wantsMeeting } from "./intent";

describe("wantsMeeting", () => {
  it.each([
    "i want to have a meeting with a client at 10 am at nov 10",
    "I need to schedule a call with Sam tomorrow at 3pm",
    "set up a zoom with the Johnson family on November 12 at 2:30 pm",
    "Let's have a meeting next Tuesday at 9am",
    "book a consultation with a new client on Nov 14 at 11am",
    "Can you schedule a meeting with the client on 11/10 at 10am?",
    "we would like to arrange a call on Friday at 4pm",
  ])("treats %j as a request to arrange a meeting", (text) => {
    expect(wantsMeeting(text)).toBe(true);
  });

  it.each([
    "when is my meeting with the client on Nov 10?",
    "find the meeting notes from Nov 10",
    "what did we agree in the call on Friday at 4pm",
    "summarize the meeting with Sam tomorrow",
    "i want to have a meeting with a client",
    "schedule a backup of the files",
    "show the zoom meetings from nov 10 at 10am",
    "did the client call on Nov 10 at 10 am?",
    "",
  ])("leaves %j to the documents", (text) => {
    expect(wantsMeeting(text)).toBe(false);
  });
});

describe("wantsEmail", () => {
  it("still recognises a request for an email", () => {
    expect(wantsEmail("draft an email to sam@client.example")).toBe(true);
    expect(wantsEmail("what does the email from Sam say")).toBe(false);
  });
});
