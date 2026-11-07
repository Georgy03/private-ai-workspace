import { LoaderCircle, Lock, X } from "lucide-react";
import { useState } from "react";

import type { SignInChoice } from "./types";

const MESSAGE_FALLBACK = "Sign-in did not finish. Please try again.";

function messageOf(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return MESSAGE_FALLBACK;
}

/** Looks like name@firm.com, the only shape the Microsoft route can use. */
export function isWorkEmail(text: string): boolean {
  return /^[^\s@]+@[^\s@./]+(\.[^\s@./]+)+$/.test(text.trim());
}

/**
 * Asks how the person wants to sign in, then hands the choice to `onSignIn`, which opens the browser and
 * resolves once they are back. Nothing here decides who may enter: the firm's server only lets in people an
 * administrator has invited, whichever button was pressed.
 */
export function SignInDialog({ onClose, onSignIn }: { onClose: () => void; onSignIn: (choice: SignInChoice) => Promise<void> }) {
  const [email, setEmail] = useState("");
  const [waiting, setWaiting] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const busy = waiting !== null;
  const emailOk = isWorkEmail(email);

  const go = async (choice: SignInChoice, label: string) => {
    setProblem(null);
    setWaiting(label);
    try {
      await onSignIn(choice);
      setWaiting(null);
      onClose();
    } catch (caught) {
      setProblem(messageOf(caught));
      setWaiting(null);
    }
  };

  return (
    <div className="modal-backdrop" onMouseDown={busy ? undefined : onClose}>
      <div className="modal signin" role="dialog" aria-modal="true" aria-label="Sign in" onMouseDown={(event) => event.stopPropagation()}>
        <header>
          <div>
            <span className="modal-icon"><Lock size={19} /></span>
            <div><strong>Sign in to your firm</strong><span>Use the account your firm invited</span></div>
          </div>
          <button aria-label="Close" disabled={busy} onClick={onClose}><X size={19} /></button>
        </header>
        <section className="provider-fields signin-body">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              if (emailOk && !busy) void go({ kind: "microsoft", email: email.trim() }, "Microsoft");
            }}
          >
            <label htmlFor="signin-email">Work email</label>
            <input id="signin-email" type="email" autoComplete="email" placeholder="name@yourfirm.com" value={email} disabled={busy} onChange={(event) => setEmail(event.target.value)} />
            <button type="submit" className="primary signin-wide" disabled={!emailOk || busy}>
              {waiting === "Microsoft" ? <LoaderCircle className="spin" size={14} /> : null} Continue with Microsoft
            </button>
            <p className="field-hint">We find the invitation for this email and take you to your own firm's Microsoft sign-in.</p>
          </form>
          <div className="signin-or"><span>or</span></div>
          <button className="secondary signin-wide" disabled={busy} onClick={() => void go({ kind: "google" }, "Google")}>
            {waiting === "Google" ? <LoaderCircle className="spin" size={14} /> : null} Continue with Google
          </button>
          <button className="secondary signin-wide" disabled={busy} onClick={() => void go({ kind: "password" }, "password")}>
            {waiting === "password" ? <LoaderCircle className="spin" size={14} /> : null} Use email and password
          </button>
          <button className="signin-link" disabled={busy} onClick={() => void go({ kind: "any" }, "other")}>Show every way to sign in</button>
          {busy && <p className="signin-waiting" role="status">Finish signing in in your browser, then come back here. This waits up to 3 minutes.</p>}
          {problem && <p className="connector-problem" role="alert">{problem}</p>}
          <p className="field-hint">Only people your firm's administrator has invited can sign in. If you cannot get in, ask them to check your invitation, and that your firm's sign-in has been set up.</p>
        </section>
      </div>
    </div>
  );
}
