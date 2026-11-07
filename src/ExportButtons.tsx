import { useState } from "react";
import { FileSpreadsheet, FileText } from "lucide-react";
import { exportReport } from "./api";

/** "Save as Word" and "Save as Excel" for a report. The file is made on this computer. */
export function ExportButtons({ title, text, first }: { title: string; text: string; first?: "docx" | "xlsx" }) {
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const save = (format: "docx" | "xlsx") => {
    setMessage(null);
    exportReport(format, title, text)
      .then((place) => setMessage(place ? { ok: true, text: `Saved to ${place}` } : null))
      .catch((error) => setMessage({ ok: false, text: error instanceof Error ? error.message : String(error) }));
  };
  return (
    <div className="export-row">
      {(first === "xlsx" ? ["xlsx", "docx"] : ["docx", "xlsx"]).map((format) =>
        format === "docx" ? (
          <button key={format} className={first === format ? "export-button export-first" : "export-button"} onClick={() => save("docx")}><FileText size={14} /> Save as Word</button>
        ) : (
          <button key={format} className={first === format ? "export-button export-first" : "export-button"} onClick={() => save("xlsx")}><FileSpreadsheet size={14} /> Save as Excel</button>
        ),
      )}
      {message && <span className={message.ok ? "export-note" : "export-note export-error"} role={message.ok ? "status" : "alert"}>{message.text}</span>}
    </div>
  );
}
