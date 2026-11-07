import { ReceiptText, ShieldCheck, X } from "lucide-react";
import type { AnswerResponse } from "./types";

/** What was sent for one answer, and where. */
export function ReceiptDialog({ answer, onClose }: { answer: AnswerResponse; onClose: () => void }) {
  const receipt = answer.receipt;
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal receipt-modal" onMouseDown={(event) => event.stopPropagation()}>
        <header><div><span className="modal-icon verified"><ReceiptText size={19} /></span><div><strong>Privacy receipt</strong><span>What happened for this answer</span></div></div><button onClick={onClose}><X size={19} /></button></header>
        <div className="receipt-status"><ShieldCheck size={24} /><div><strong>{!receipt.model ? (receipt.cloud_question_sent ? "Your question was sent to the firm cloud index" : "Nothing left this device") : receipt.provider === "anthropic" ? "Sent to the Claude API (test mode)" : "Sent to Amazon Bedrock"}</strong><span>{receipt.summary}</span></div></div>
        <dl><div><dt>Provider</dt><dd>{!receipt.model ? "None" : receipt.provider === "anthropic" ? "Claude API, no zero data retention" : "Claude on Amazon Bedrock"}</dd></div><div><dt>Model</dt><dd>{receipt.model ?? "No model"}</dd></div><div><dt>Local passages</dt><dd>{receipt.passages_local}</dd></div><div><dt>Cloud passages</dt><dd>{receipt.passages_cloud}</dd></div><div><dt>Question sent to cloud index</dt><dd>{receipt.cloud_question_sent ? "Yes" : "No"}</dd></div><div><dt>Time</dt><dd>{new Date(receipt.created_at * 1000).toLocaleString()}</dd></div><div><dt>Receipt ID</dt><dd className="mono">{receipt.id}</dd></div></dl>
        <footer><button className="primary" onClick={onClose}>Done</button></footer>
      </div>
    </div>
  );
}
