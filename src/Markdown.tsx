import type { ReactNode } from "react";

/**
 * Shows the Markdown an AI writes (headings, bold, lists, tables, quotes) as formatted text.
 * It builds React elements from the text and never inserts HTML, so nothing a document or the model
 * writes can run as a page or load anything. Links are left as plain text for the same reason.
 */

function inline(text: string, keyBase: string): ReactNode[] {
  const parts: ReactNode[] = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`|\*[^*\s][^*]*\*)/g;
  let last = 0;
  let index = 0;
  for (const match of text.matchAll(pattern)) {
    const at = match.index ?? 0;
    if (at > last) parts.push(text.slice(last, at));
    const token = match[0];
    const key = `${keyBase}-${index++}`;
    if (token.startsWith("**")) parts.push(<strong key={key}>{token.slice(2, -2)}</strong>);
    else if (token.startsWith("`")) parts.push(<code key={key}>{token.slice(1, -1)}</code>);
    else parts.push(<em key={key}>{token.slice(1, -1)}</em>);
    last = at + token.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

const cells = (line: string) =>
  line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());

const isTableRule = (line: string) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line);
const isTableRow = (line: string) => line.includes("|") && line.trim().length > 1;

export function Markdown({ text }: { text: string }) {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: ReactNode[] = [];
  let i = 0;
  let key = 0;
  while (i < lines.length) {
    const line = lines[i];
    const k = `b${key++}`;
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      const level = Math.min(heading[1].length + 1, 6); // the page already has its own top heading
      const Tag = `h${level}` as "h2";
      blocks.push(<Tag key={k}>{inline(heading[2], k)}</Tag>);
      i += 1;
    } else if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) {
      blocks.push(<hr key={k} />);
      i += 1;
    } else if (isTableRow(line) && i + 1 < lines.length && isTableRule(lines[i + 1])) {
      const head = cells(line);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && isTableRow(lines[i])) rows.push(cells(lines[i++]));
      blocks.push(
        <div className="md-table" key={k}>
          <table>
            <thead><tr>{head.map((cell, c) => <th key={c}>{inline(cell, `${k}h${c}`)}</th>)}</tr></thead>
            <tbody>
              {rows.map((row, r) => (
                <tr key={r}>{head.map((_, c) => <td key={c}>{inline(row[c] ?? "", `${k}r${r}c${c}`)}</td>)}</tr>
              ))}
            </tbody>
          </table>
        </div>,
      );
    } else if (/^\s*([-*•])\s+/.test(line) || /^\s*\d+[.)]\s+/.test(line)) {
      const ordered = /^\s*\d+[.)]\s+/.test(line);
      const items: string[] = [];
      while (i < lines.length && (ordered ? /^\s*\d+[.)]\s+/.test(lines[i]) : /^\s*([-*•])\s+/.test(lines[i]))) {
        items.push(lines[i].replace(ordered ? /^\s*\d+[.)]\s+/ : /^\s*([-*•])\s+/, ""));
        i += 1;
      }
      const List = ordered ? "ol" : "ul";
      blocks.push(<List key={k}>{items.map((item, n) => <li key={n}>{inline(item, `${k}i${n}`)}</li>)}</List>);
    } else if (/^\s*>\s?/.test(line)) {
      const quoted: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) quoted.push(lines[i++].replace(/^\s*>\s?/, ""));
      blocks.push(<blockquote key={k}>{inline(quoted.join(" "), k)}</blockquote>);
    } else {
      const paragraph: string[] = [];
      while (i < lines.length && lines[i].trim() && !/^(#{1,6}\s|\s*>|\s*([-*•]|\d+[.)])\s+)/.test(lines[i]) && !(isTableRow(lines[i]) && i + 1 < lines.length && isTableRule(lines[i + 1]))) {
        paragraph.push(lines[i++].trim());
      }
      if (paragraph.length === 0) {
        paragraph.push(lines[i++].trim());
      }
      blocks.push(<p key={k}>{inline(paragraph.join(" "), k)}</p>);
    }
  }
  return <div className="markdown">{blocks}</div>;
}
