import type { SearchLimits } from "./types";

/** Nothing chosen: a question looks everywhere. */
export const NO_LIMITS: SearchLimits = { scope: null, sources: [], types: [] };

export const SOURCE_NAMES: Record<string, string> = {
  google_drive: "Google Drive",
  microsoft365: "Microsoft 365",
  dropbox: "Dropbox",
  docusign: "DocuSign",
  quickbooks: "QuickBooks",
};

/** The kinds of file a question can be limited to. Google's formats are told apart by what they are. */
export const KINDS: { value: string; label: string; hint: string; words: string }[] = [
  { value: "document", label: "Documents", hint: "Google Docs, Word, text files", words: "docs doc word text google" },
  { value: "spreadsheet", label: "Spreadsheets", hint: "Google Sheets, Excel, CSV", words: "sheets sheet excel csv google" },
  { value: "presentation", label: "Presentations", hint: "Google Slides, PowerPoint", words: "slides slide deck powerpoint google" },
  { value: "pdf", label: "PDFs", hint: "PDF files", words: "pdf" },
  { value: "image", label: "Images", hint: "Photos and scans", words: "images photos scans png jpg" },
  { value: "other", label: "Other files", hint: "Anything else", words: "other misc" },
];

/** What the button or chip says about where a question looks. */
export function limitsLabel(limits: SearchLimits, everything = "All sources"): string {
  if (limits.scope === "local") return "This device only";
  if (limits.scope === "cloud") {
    const names = [
      ...limits.sources.map((source) => SOURCE_NAMES[source] ?? source.replace(/_/g, " ")),
      ...limits.types.map((type) => KINDS.find((kind) => kind.value === type)?.label ?? type),
    ];
    if (names.length === 0) return "Firm cloud only";
    return names.length <= 2 ? "Firm cloud · " + names.join(", ") : "Firm cloud · " + names.length + " filters";
  }
  return everything;
}
