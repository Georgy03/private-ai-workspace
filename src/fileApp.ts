export type FileApp = "google_docs" | "google_sheets" | "google_slides" | "word" | "excel" | "powerpoint" | "pdf" | "image" | "csv" | "text" | "other";

/** Which program a file belongs to, from its type and name. Mirrors the server's rule. */
export function fileApp(mime: string, name: string): FileApp {
  const m = mime.toLowerCase();
  const n = name.toLowerCase();
  const ends = (...exts: string[]) => exts.some((ext) => n.endsWith(ext));
  if (m === "application/vnd.google-apps.document") return "google_docs";
  if (m === "application/vnd.google-apps.spreadsheet") return "google_sheets";
  if (m === "application/vnd.google-apps.presentation") return "google_slides";
  if (m.includes("wordprocessingml") || m.includes("msword") || ends(".docx", ".doc", ".docm")) return "word";
  if (m.includes("spreadsheetml") || m.includes("ms-excel") || ends(".xlsx", ".xls", ".xlsm")) return "excel";
  if (m.includes("presentationml") || m.includes("ms-powerpoint") || ends(".pptx", ".ppt", ".pptm")) return "powerpoint";
  if (m.includes("pdf") || ends(".pdf")) return "pdf";
  if (m.startsWith("image/") || ends(".png", ".jpg", ".jpeg", ".gif", ".tif", ".tiff", ".heic")) return "image";
  if (m.includes("csv") || ends(".csv")) return "csv";
  if (m.startsWith("text/") || ends(".txt", ".md", ".rtf")) return "text";
  return "other";
}

/** For a cloud passage that carries no file type: what its link says about the program and the service. */
export function appFromLink(link: string | null | undefined, name: string): { app: FileApp; source: string | null } {
  let host = "";
  let path = "";
  try { const url = new URL(link ?? ""); host = url.hostname; path = url.pathname; } catch { /* no usable link */ }
  const byName = fileApp("", name);
  const source = host.endsWith("google.com") ? "google_drive"
    : /sharepoint\.com$|office\.com$|live\.com$|microsoft\.com$/.test(host) ? "microsoft365"
    : host.endsWith("dropbox.com") ? "dropbox"
    : host.endsWith("docusign.com") ? "docusign" : null;
  if (host === "docs.google.com") {
    if (path.startsWith("/document")) return { app: "google_docs", source };
    if (path.startsWith("/spreadsheets")) return { app: "google_sheets", source };
    if (path.startsWith("/presentation")) return { app: "google_slides", source };
  }
  return { app: byName, source };
}
