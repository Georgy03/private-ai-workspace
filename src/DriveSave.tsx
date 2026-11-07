import { useState } from "react";
import { ExternalLink, HardDriveUpload, LoaderCircle, X } from "lucide-react";
import { cloudDriveSave } from "./api";

const message = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : "Something went wrong.");

/**
 * "Save to Google Drive" and the box that asks first. The box shows the exact name and text that will
 * be created, in the person's own Drive, and nothing is saved until they press the button in it.
 */
export function DriveSave({ title, text, onOpenLink }: { title: string; text: string; onOpenLink?: (url: string) => void }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState(title);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [saved, setSaved] = useState<{ link?: string | null } | null>(null);

  const ask = () => { setName(title); setProblem(null); setSaved(null); setOpen(true); };
  const approve = () => {
    setBusy(true);
    setProblem(null);
    cloudDriveSave(name.trim(), text).then(
      (result) => { setBusy(false); setSaved({ link: result.link }); },
      (caught) => { setBusy(false); setProblem(message(caught)); },
    );
  };

  return (
    <>
      <button className="export-button" onClick={ask}><HardDriveUpload size={14} /> Save to Google Drive</button>
      {open && (
        <div className="agent-modal" role="dialog" aria-modal="true" aria-label="Save to Google Drive" onMouseDown={(event) => { if (event.target === event.currentTarget && !busy) setOpen(false); }}>
          <div className="agent-modal-body drive-approval">
            <header>
              <h2>Save to your Google Drive?</h2>
              <button className="icon-button" aria-label="Close" disabled={busy} onClick={() => setOpen(false)}><X size={16} /></button>
            </header>
            {saved ? (
              <>
                <p className="knowledge-note" role="status">Saved to your Google Drive as a new document.</p>
                <div className="knowledge-actions">
                  {saved.link && onOpenLink && <button className="primary" onClick={() => onOpenLink(saved.link!)}><ExternalLink size={14} /> Open it</button>}
                  <button className="secondary" onClick={() => setOpen(false)}>Done</button>
                </div>
              </>
            ) : (
              <>
                <p className="knowledge-note">This creates one new Google Doc in your own Drive with exactly the text below. Nothing else in your Drive is changed. Formatting such as headings and bold is saved as plain text.</p>
                <label>File name<input value={name} maxLength={200} onChange={(event) => setName(event.target.value)} /></label>
                <label>Text that will be saved<textarea readOnly rows={10} value={text} aria-label="Text that will be saved" /></label>
                {problem && <p className="knowledge-error" role="alert">{problem}</p>}
                <div className="knowledge-actions">
                  <button className="primary" disabled={busy || !name.trim()} onClick={approve}>{busy ? <><LoaderCircle size={14} className="spin" /> Saving…</> : "Approve and save"}</button>
                  <button className="secondary" disabled={busy} onClick={() => setOpen(false)}>Cancel</button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </>
  );
}
