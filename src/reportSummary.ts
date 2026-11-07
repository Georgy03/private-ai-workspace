/** Small readings of a report written in Markdown, for the card shown above it. */

const HEADING = /^#{1,6}\s+(.*)$/;
const BULLET = /^\s*([-*•]|\d+[.)])\s+/;

/** The lines under the first heading that matches, up to the next heading. */
function section(markdown: string, title: RegExp): string[] | null {
  const lines = markdown.replace(/\r\n/g, "\n").split("\n");
  const start = lines.findIndex((line) => HEADING.test(line) && title.test(line.replace(/^#+\s+/, "")));
  if (start < 0) return null;
  const body: string[] = [];
  for (const line of lines.slice(start + 1)) {
    if (HEADING.test(line)) break;
    body.push(line);
  }
  return body;
}

const plain = (text: string) => text.replace(/\*\*([^*]+)\*\*/g, "$1").replace(/(^|\s)\*([^*\s][^*]*)\*/g, "$1$2").replace(/`([^`]+)`/g, "$1");

/**
 * A short summary for the top of the card: the "summary" or "overview" section when the report has
 * one, otherwise its first paragraph. Cut at a sentence end when it is long.
 */
export function summaryOf(markdown: string, limit = 360): string {
  const body = section(markdown, /summary|overview|key points|bottom line/i) ?? markdown.replace(/\r\n/g, "\n").split("\n").filter((line) => !HEADING.test(line));
  const paragraphs = body.join("\n").split(/\n\s*\n/).map((p) => p.trim()).filter((p) => p && !p.startsWith("|"));
  const text = plain((paragraphs[0] ?? "").replace(/\s*\n\s*/g, " "));
  if (text.length <= limit) return text;
  const cut = text.slice(0, limit);
  const end = Math.max(cut.lastIndexOf(". "), cut.lastIndexOf("? "), cut.lastIndexOf("! "));
  return end > limit / 2 ? cut.slice(0, end + 1) : `${cut.trimEnd()}…`;
}

/** How many list items or table rows sit under the first heading that matches. */
export function countUnder(markdown: string, title: RegExp): number {
  const body = section(markdown, title);
  if (!body) return 0;
  const items = body.filter((line) => BULLET.test(line)).length;
  const rows = body.filter((line) => line.trim().startsWith("|"));
  // A table is a header row, a rule and then the rows.
  const tableRows = rows.length >= 3 ? rows.length - 2 : 0;
  return items + tableRows;
}

export const FINDINGS = /findings?|discrepanc/i;
export const RECOMMENDATIONS = /recommendations?|next steps/i;
