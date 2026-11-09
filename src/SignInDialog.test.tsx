// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SignInDialog, isWorkEmail } from "./SignInDialog";

afterEach(cleanup);

const noFirm = vi.fn().mockResolvedValue(undefined);

describe("SignInDialog", () => {
  it("accepts only an email that looks like name@firm.com", () => {
    for (const good of ["sam@acme.com", " sam@mail.acme.co.uk "]) expect(isWorkEmail(good)).toBe(true);
    for (const bad of ["", "sam", "sam@acme", "@acme.com", "sam@@acme.com", "sam@acme..com", "sa m@acme.com"]) expect(isWorkEmail(bad)).toBe(false);
  });

  it("sends the typed email for Microsoft, only once it looks right, and closes when sign-in finishes", async () => {
    const onSignIn = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<SignInDialog onClose={onClose} onSignIn={onSignIn} onCreateFirm={noFirm} />);
    const microsoft = screen.getByRole("button", { name: /continue with microsoft/i });
    expect((microsoft as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Work email"), { target: { value: "sam@acme.com" } });
    fireEvent.click(microsoft);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onSignIn).toHaveBeenCalledWith({ kind: "microsoft", email: "sam@acme.com" });
  });

  it("offers Google, password and every way, each as its own choice", async () => {
    const onSignIn = vi.fn().mockResolvedValue(undefined);
    render(<SignInDialog onClose={() => {}} onSignIn={onSignIn} onCreateFirm={noFirm} />);
    for (const [name, kind] of [[/continue with google/i, "google"], [/email and password/i, "password"], [/every way/i, "any"]] as const) {
      fireEvent.click(screen.getByRole("button", { name }));
      await waitFor(() => expect(onSignIn).toHaveBeenLastCalledWith({ kind }));
    }
  });

  it("stays open and says why when sign-in fails, and can be tried again", async () => {
    const onSignIn = vi.fn().mockRejectedValueOnce("sign-in took too long").mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<SignInDialog onClose={onClose} onSignIn={onSignIn} onCreateFirm={noFirm} />);
    fireEvent.click(screen.getByRole("button", { name: /continue with google/i }));
    expect((await screen.findByRole("alert")).textContent).toContain("sign-in took too long");
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /continue with google/i }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("cannot be closed while waiting for the browser", async () => {
    let finish: () => void = () => {};
    const onSignIn = vi.fn(() => new Promise<void>((resolve) => { finish = resolve; }));
    render(<SignInDialog onClose={() => {}} onSignIn={onSignIn} onCreateFirm={noFirm} />);
    fireEvent.click(screen.getByRole("button", { name: /continue with google/i }));
    expect((await screen.findByRole("status")).textContent).toMatch(/finish signing in/i);
    expect((screen.getByRole("button", { name: "Close" }) as HTMLButtonElement).disabled).toBe(true);
    finish();
  });

  it("starts a firm from the Create my firm tab only once it has a name, and closes when done", async () => {
    const onCreateFirm = vi.fn().mockResolvedValue(undefined);
    const onClose = vi.fn();
    render(<SignInDialog onClose={onClose} onSignIn={vi.fn()} onCreateFirm={onCreateFirm} />);
    fireEvent.click(screen.getByRole("radio", { name: /create my firm/i }));
    const go = screen.getByRole("button", { name: /continue with google/i }) as HTMLButtonElement;
    expect(go.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Firm name"), { target: { value: "  Sam Rivera, CPA " } });
    expect(go.disabled).toBe(false);
    fireEvent.click(go);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onCreateFirm).toHaveBeenCalledWith("  Sam Rivera, CPA ", "google");
  });

  it("shows why a firm could not be started and lets the person try again", async () => {
    const onCreateFirm = vi.fn().mockRejectedValueOnce("this Google account already belongs to a firm").mockResolvedValue(undefined);
    render(<SignInDialog onClose={() => {}} onSignIn={vi.fn()} onCreateFirm={onCreateFirm} />);
    fireEvent.click(screen.getByRole("radio", { name: /create my firm/i }));
    fireEvent.change(screen.getByLabelText("Firm name"), { target: { value: "Sam" } });
    fireEvent.click(screen.getByRole("button", { name: /continue with google/i }));
    expect((await screen.findByRole("alert")).textContent).toContain("already belongs to a firm");
    expect((screen.getByRole("button", { name: /continue with google/i }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("can start a firm with a personal Microsoft account too", async () => {
    const onCreateFirm = vi.fn().mockResolvedValue(undefined);
    render(<SignInDialog onClose={() => {}} onSignIn={vi.fn()} onCreateFirm={onCreateFirm} />);
    fireEvent.click(screen.getByRole("radio", { name: /create my firm/i }));
    const personal = screen.getByRole("button", { name: /personal microsoft account/i }) as HTMLButtonElement;
    expect(personal.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Firm name"), { target: { value: "Sam" } });
    fireEvent.click(personal);
    await waitFor(() => expect(onCreateFirm).toHaveBeenCalledWith("Sam", "microsoft_personal"));
  });
});
