import { describe, expect, it } from "vitest";
import { withZoomLink } from "./zoomText";

const body = "Hello,\n\nI would like to arrange a Zoom meeting on November 10 at 10:00 AM. Would that work for you?\n\nOnce it is confirmed I will send the Zoom link.\n\nBest regards,";
const zoom = { join_url: "https://us06web.zoom.us/j/81234567890?pwd=abc123", meeting_id: "81234567890", passcode: "abc123" };

describe("withZoomLink", () => {
  it("puts the link above the sign-off and takes out the line that said it would follow", () => {
    const text = withZoomLink(body, zoom, "America/Chicago");
    expect(text).toBe(
      "Hello,\n\nI would like to arrange a Zoom meeting on November 10 at 10:00 AM. Would that work for you?\n\nJoin Zoom Meeting\nhttps://us06web.zoom.us/j/81234567890?pwd=abc123\nMeeting ID: 812 3456 7890\nPasscode: abc123\nTime zone: America/Chicago\n\nBest regards,",
    );
  });

  it("keeps the first paragraph, which only mentions the meeting, and adds the link at the end when there is no sign-off", () => {
    const text = withZoomLink("Hello,\n\nLet us meet by Zoom on Friday.", { join_url: "https://zoom.us/j/1" }, "");
    expect(text).toBe("Hello,\n\nLet us meet by Zoom on Friday.\n\nJoin Zoom Meeting\nhttps://zoom.us/j/1");
  });
});
