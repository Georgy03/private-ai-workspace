import type { ReactNode } from "react";
import type { FileApp } from "./fileApp";

const LABELS: Record<FileApp, string> = {
  google_docs: "Google Docs", google_sheets: "Google Sheets", google_slides: "Google Slides", word: "Word document",
  excel: "Excel spreadsheet", powerpoint: "PowerPoint presentation", pdf: "PDF", image: "Image", csv: "CSV file", text: "Text file", other: "File",
};

/** A page with a folded corner, as Google draws its editors. */
function Page({ color, dark, children }: { color: string; dark: string; children: ReactNode }) {
  return (
    <>
      <path d="M7 2h13l7 7v19a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z" fill={color} />
      <path d="M20 2v5a2 2 0 0 0 2 2h5z" fill={dark} />
      {children}
    </>
  );
}

/** A square tile with one letter, as Microsoft draws its apps. */
function Tile({ color, letter }: { color: string; letter: string }) {
  return (
    <>
      <rect x="3" y="3" width="26" height="26" rx="5" fill={color} />
      <text x="16" y="23" textAnchor="middle" fontSize="17" fontWeight="700" fontFamily="Arial, Helvetica, sans-serif" fill="#fff">{letter}</text>
    </>
  );
}

const white = "#fff";

function Glyph({ app }: { app: FileApp }) {
  switch (app) {
    case "google_docs":
      return <Page color="#4285f4" dark="#a1c2fa"><path d="M11 15h10M11 19h10M11 23h7" stroke={white} strokeWidth="1.8" strokeLinecap="round" /></Page>;
    case "google_sheets":
      return <Page color="#0f9d58" dark="#87ceac"><path d="M11 14h10v10H11zM11 19h10M16 14v10" stroke={white} strokeWidth="1.6" fill="none" /></Page>;
    case "google_slides":
      return <Page color="#f4b400" dark="#fce8b2"><rect x="11" y="15" width="10" height="8" rx="1" stroke={white} strokeWidth="1.6" fill="none" /></Page>;
    case "word":
      return <Tile color="#2b579a" letter="W" />;
    case "excel":
      return <Tile color="#217346" letter="X" />;
    case "powerpoint":
      return <Tile color="#d24726" letter="P" />;
    case "pdf":
      return (
        <Page color="#e5252a" dark="#f4a6a8">
          <text x="16" y="25" textAnchor="middle" fontSize="7.5" fontWeight="700" fontFamily="Arial, Helvetica, sans-serif" fill={white}>PDF</text>
        </Page>
      );
    case "image":
      return (
        <>
          <rect x="3" y="5" width="26" height="22" rx="4" fill="#7c8aa5" />
          <circle cx="11" cy="12" r="2.5" fill="#fff" />
          <path d="M3 24l8-8 6 6 4-4 8 8v1a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4z" fill="#d9deea" />
        </>
      );
    case "csv":
      return <Page color="#5f9d6e" dark="#c2dcc8"><path d="M11 14h10v10H11zM11 19h10M16 14v10" stroke={white} strokeWidth="1.4" fill="none" /></Page>;
    case "text":
      return <Page color="#8a9099" dark="#d0d3d8"><path d="M11 15h10M11 19h10M11 23h6" stroke={white} strokeWidth="1.8" strokeLinecap="round" /></Page>;
    default:
      return <Page color="#a8adb5" dark="#dcdfe3"><span /></Page>;
  }
}

/** The icon of the program a file belongs to: Google Docs, Sheets, Slides, Word, Excel, PowerPoint, PDF… */
export function FileIcon({ app, size = 28 }: { app: FileApp; size?: number }) {
  return (
    <svg className="file-app-icon" width={size} height={size} viewBox="0 0 32 32" role="img" aria-label={LABELS[app]}>
      <Glyph app={app} />
    </svg>
  );
}
