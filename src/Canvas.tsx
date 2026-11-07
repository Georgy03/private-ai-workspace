import { useState } from "react";
import { Check, Copy, Maximize2, Minimize2, RotateCcw, X } from "lucide-react";
import { DriveSave } from "./DriveSave";
import { ExportButtons } from "./ExportButtons";
import { Markdown } from "./Markdown";

/**
 * A long answer opened beside the chat, to be changed by hand. What is written here is the person's
 * own copy: the chat's answer and its sources are not touched, and nothing is sent anywhere.
 */
export function Canvas({ title, original, text, expanded, first, onChange, onExpand, onClose, onOpenLink }: {
  title: string;
  original: string;
  text: string;
  expanded: boolean;
  first?: "docx" | "xlsx" | null;
  onChange: (text: string) => void;
  onExpand: () => void;
  onClose: () => void;
  onOpenLink?: (url: string) => void;
}) {
  const [mode, setMode] = useState<"edit" | "preview">("edit");
  const [copied, setCopied] = useState(false);
  const words = text.trim() ? text.trim().split(/\s+/).length : 0;
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1500); }, () => undefined);
  };
  return (
    <aside className={`sources-panel canvas-panel ${expanded ? "expanded" : ""}`} aria-label="Canvas">
      <header>
        <div>
          <h2>Canvas</h2>
          <small>{title} · {words} word{words === 1 ? "" : "s"} · your copy, kept until you close the app</small>
        </div>
        <div className="sources-actions">
          <button className="icon-button" aria-label={expanded ? "Make the canvas narrower" : "Make the canvas wider"} aria-pressed={expanded} onClick={onExpand}>{expanded ? <Minimize2 size={15} /> : <Maximize2 size={15} />}</button>
          <button className="icon-button" aria-label="Close canvas" onClick={onClose}><X size={16} /></button>
        </div>
      </header>
      <div className="canvas-bar">
        <div role="tablist" aria-label="Canvas view">
          <button role="tab" aria-selected={mode === "edit"} className={mode === "edit" ? "active" : ""} onClick={() => setMode("edit")}>Edit</button>
          <button role="tab" aria-selected={mode === "preview"} className={mode === "preview" ? "active" : ""} onClick={() => setMode("preview")}>Preview</button>
        </div>
        <div>
          <button className="icon-button" aria-label="Copy the text" title="Copy the text" onClick={copy}>{copied ? <Check size={15} /> : <Copy size={15} />}</button>
          <button className="icon-button" aria-label="Go back to the original answer" title="Go back to the original answer" disabled={text === original} onClick={() => onChange(original)}><RotateCcw size={15} /></button>
        </div>
      </div>
      {mode === "edit" ? (
        <textarea className="canvas-text" aria-label="Canvas text" spellCheck value={text} onChange={(event) => onChange(event.target.value)} />
      ) : (
        <div className="canvas-preview answer-copy"><Markdown text={text} /></div>
      )}
      <footer>
        <ExportButtons title={title} text={text} first={first ?? undefined} />
        <div className="export-row"><DriveSave title={title} text={text} onOpenLink={onOpenLink} /></div>
      </footer>
    </aside>
  );
}
