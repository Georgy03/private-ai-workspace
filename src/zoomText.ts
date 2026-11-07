/** What a made Zoom meeting adds to an email. */
export interface ZoomLink {
  join_url: string;
  meeting_id?: string | null;
  passcode?: string | null;
}

/** A Zoom meeting ID in the groups people read it in: "812 3456 7890". */
function spaced(id: string): string {
  const digits = id.replace(/\D/g, "");
  if (digits.length < 9) return digits || id;
  return digits.length === 11 ? `${digits.slice(0, 3)} ${digits.slice(3, 7)} ${digits.slice(7)}` : digits.replace(/(\d{3})(?=\d)/g, "$1 ");
}

/**
 * The email with the Zoom link in it: the line that said a link would follow is taken out, and the
 * link, meeting ID, passcode and time zone go in above the sign-off.
 */
export function withZoomLink(body: string, zoom: ZoomLink, timeZone: string): string {
  const paragraphs = body.split(/\n{2,}/).filter((paragraph) => !(paragraph.length < 220 && /zoom link (will|to follow)|send (you )?the zoom (link|invitation)|zoom (link|invite) (will )?follow/i.test(paragraph)));
  const lines = ["Join Zoom Meeting", zoom.join_url];
  if (zoom.meeting_id) lines.push(`Meeting ID: ${spaced(zoom.meeting_id)}`);
  if (zoom.passcode) lines.push(`Passcode: ${zoom.passcode}`);
  if (timeZone) lines.push(`Time zone: ${timeZone}`);
  const block = lines.join("\n");
  const signOff = paragraphs.findIndex((paragraph) => /^\s*(best regards|kind regards|regards|sincerely|thanks|thank you|best),?\s*$/i.test(paragraph.trim().split("\n")[0] ?? ""));
  if (signOff >= 0) paragraphs.splice(signOff, 0, block);
  else paragraphs.push(block);
  return paragraphs.join("\n\n");
}
