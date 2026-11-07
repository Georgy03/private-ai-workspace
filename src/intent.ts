/** Does the message ask for an email to be written? Checked on the device, nothing is sent. */
export const wantsEmail = (text: string) =>
  /\b(draft|write|compose|prepare|create|make|send)\b[^.?!]{0,60}\be-?mail\b/i.test(text) || /^\s*(please\s+)?e-?mail\s+\S+/i.test(text);

const MONTH = "(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)";
const DAY = "(?:mon|tues?|wed(?:nes)?|thu(?:rs)?|fri|sat(?:ur)?|sun)(?:day)?";

/**
 * Does the message ask to arrange a meeting for a given time ("I want to have a meeting with a client
 * at 10 am on Nov 10", "set up a call with Sam tomorrow at 3pm")? Such a message is answered with an
 * email to send, with a Zoom link to add, not by searching documents. It needs all three: a wish to
 * arrange something, a meeting, and a day or a time. A question about meetings ("when is my meeting
 * with the client?", "find the meeting notes from Nov 10") is left to the documents.
 */
export function wantsMeeting(text: string): boolean {
  const message = text.trim();
  if (message.length < 8 || message.length > 600) return false;
  const asksToArrange = /\b(?:(?:i|we)(?:'d| would| want| need| wish| plan|'ll| will)?\s*(?:like\s+)?(?:to\s+)?(?:want|need|like|wish|plan)s?\s+to|(?:i|we)'?(?:ll| will| 'd| would)?\s+(?:schedule|set up|book|arrange)|(?:schedule|set up|setup|book|arrange|organi[sz]e|plan)\b|let'?s\s+(?:have|set|schedule|meet|do|book)|(?:can|could|would) you (?:please )?(?:schedule|set up|book|arrange))/i.test(message);
  // Something that only asks about what is already there is not a request to arrange.
  const isQuestion = /^\s*(?:who|what|when|where|why|how|which|did|does|do|is|are|was|were|show|find|list|search|summari[sz]e|tell|give|any|have)\b/i.test(message) || (/\?\s*$/.test(message) && !/^\s*(?:can|could|would) you (?:please )?(?:schedule|set up|book|arrange)/i.test(message));
  const aMeeting = /\b(?:meeting|meet|call|zoom|appointment|catch[- ]?up|consultation|video call|conference call|session)\b/i.test(message);
  const aTime = new RegExp(`\\b\\d{1,2}(?::\\d{2})?\\s*(?:am|pm|a\\.m\\.|p\\.m\\.)|\\b${MONTH}\\.?\\s*\\d{1,2}\\b|\\b\\d{1,2}(?:st|nd|rd|th)?\\s+(?:of\\s+)?${MONTH}\\b|\\b(?:today|tomorrow|tonight)\\b|\\b(?:next|this|on)\\s+${DAY}\\b|\\b\\d{1,2}/\\d{1,2}\\b`, "i").test(message);
  return asksToArrange && !isQuestion && aMeeting && aTime;
}
