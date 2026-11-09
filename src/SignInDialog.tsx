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
export function SignInDialog({ onClose, onSignIn, onCreateFirm }: { onClose: () => void; onSignIn: (choice: SignInChoice) => Promise<void>; onCreateFirm: (name: string, withProvider: "google" | "microsoft_personal") => Promise<void> }) {
  const [mode, setMode] = useState<"in" | "new">("in");
  const [firmName, setFirmName] = useState("");
  const [email, setEmail] = useState("");
  const [waiting, setWaiting] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const busy = waiting !== null;
  const emailOk = isWorkEmail(email);

  const nameOk = firmName.trim().length > 0 && firmName.trim().length <= 120;

  const go = async (choice: SignInChoice | { kind: "new_firm"; withProvider: "google" | "microsoft_personal" }, label: string) => {
    setProblem(null);
    setWaiting(label);
    try {
      if (choice.kind === "new_firm") await onCreateFirm(firmName, choice.withProvider);
      else await onSignIn(choice);
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
            <div><strong>{mode === "in" ? "Sign in to your firm" : "Create your firm"}</strong><span>{mode === "in" ? "Use the account your firm invited" : "Start your own, in a minute"}</span></div>
          </div>
          <button aria-label="Close" disabled={busy} onClick={onClose}><X size={19} /></button>
        </header>
        <section className="provider-fields signin-body">
          <div className="theme-choice signin-mode" role="radiogroup" aria-label="New or returning">
            <button type="button" role="radio" aria-checked={mode === "in"} className={mode === "in" ? "active" : ""} disabled={busy} onClick={() => setMode("in")}>I have an account</button>
            <button type="button" role="radio" aria-checked={mode === "new"} className={mode === "new" ? "active" : ""} disabled={busy} onClick={() => setMode("new")}>Create my firm</button>
          </div>
          {mode === "new" ? (
            <form
              onSubmit={(event) => {
                event.preventDefault();
                if (nameOk && !busy) void go({ kind: "new_firm", withProvider: "google" }, "new");
              }}
            >
              <label htmlFor="signin-firm">Firm name</label>
              <input id="signin-firm" type="text" autoComplete="organization" maxLength={120} placeholder="Sam Rivera, CPA" value={firmName} disabled={busy} onChange={(event) => setFirmName(event.target.value)} />
              <button type="submit" className="primary signin-wide" disabled={!nameOk || busy}>
                {waiting === "new" ? <LoaderCircle className="spin" size={14} /> : null} Continue with Google
              </button>
              <button type="button" className="secondary signin-wide" disabled={!nameOk || busy} onClick={() => void go({ kind: "new_firm", withProvider: "microsoft_personal" }, "new-microsoft")}>
                {waiting === "new-microsoft" ? <LoaderCircle className="spin" size={14} /> : null} Continue with a personal Microsoft account
              </button>
              <p className="field-hint">You sign in with Google or a personal Microsoft account (outlook.com, hotmail.com, live.com), and your firm is made with you as its administrator. Everything you add stays in your own firm, under its own encryption key. You can invite people to it afterwards.</p>
              {busy && <p className="signin-waiting" role="status">Finish signing in in your browser, then come back here. This waits up to 3 minutes.</p>}
              {problem && <p className="connector-problem" role="alert">{problem}</p>}
            </form>
          ) : (<>
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
          </>)}
        </section>
      </div>
    </div>
  );
}
