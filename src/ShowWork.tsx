import { useState } from "react";
import { ChevronRight } from "lucide-react";

/** "Show work": what was done to answer, step by step. Closed until asked for. */
export function ShowWork({ steps }: { steps: string[] }) {
  const [open, setOpen] = useState(false);
  if (steps.length === 0) return null;
  return (
    <div className={`show-work ${open ? "open" : ""}`}>
      <button aria-expanded={open} onClick={() => setOpen(!open)}>Show work <ChevronRight size={13} /></button>
      {open && <ol aria-label="Steps taken to answer">{steps.map((step, index) => <li key={index}>{step}</li>)}</ol>}
    </div>
  );
}
