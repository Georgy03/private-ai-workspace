// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import * as api from "./api";
import { setTyping } from "./typing";
import { addPreviewAction, cloudAgentCreate, resetPreviewActions, resetPreviewSpaces, setPreviewCanShareWithWorkspace, setPreviewConnections, setPreviewEmailSending, setPreviewServerDown } from "./api";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/** Types a question on Home and asks it. It opens in Chat; this returns the conversation. */
async function askFromHome(question: string) {
  const input = await screen.findByRole("textbox", { name: /ask your workspace/i });
  fireEvent.change(input, { target: { value: question } });
  fireEvent.click(screen.getByRole("button", { name: "Ask" }));
  return screen.findByLabelText("Conversation");
}

describe("glass workspace controls", () => {
  it("asks from Home, which opens the question in Chat and answers it with the files it came from", async () => {
    render(<App />);
    const thread = await askFromHome("When is payment due?");
    expect((await within(thread).findAllByText(/thirty days/i)).length).toBeGreaterThan(0);
    // The sources appear under the answer once it has been written out.
    expect(await within(thread).findByRole("button", { name: /^sources/i })).toBeTruthy();
    // Home no longer has an answer screen of its own.
    expect(screen.queryByText(/answer from your workspace/i)).toBeNull();
  });

  it("keeps navigation and settings usable", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    expect(screen.getByText("Turnip Engagement.txt")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    expect(screen.getByText("Workspace settings")).toBeTruthy();
  });

  it("labels passages from the firm cloud index and opens them without a file", async () => {
    render(<App />);
    const thread = await askFromHome("engagement checklist letter");
    fireEvent.click(await within(thread).findByRole("button", { name: /^sources/i }));
    const panel = await screen.findByLabelText("Search results");
    expect(within(panel).getAllByText(/firm cloud/i).length).toBeGreaterThan(0);

    fireEvent.click(within(thread).getByRole("button", { name: /^\d+\s*engagement checklist\.docx/i }));
    expect(await screen.findByText(/passage from the firm cloud index/i)).toBeTruthy();

    fireEvent.click(within(thread).getByRole("button", { name: /privacy receipt/i }));
    expect(await screen.findByText("Cloud passages")).toBeTruthy();
  });

  it("stops searching the firm cloud index when it is switched off in settings", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /settings/i }));
    const toggle = (await screen.findByRole("checkbox", { name: /search the firm cloud index/i })) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle.checked).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: /done/i }));

    const thread = await askFromHome("engagement checklist letter");
    await within(thread).findByRole("button", { name: /^sources/i });
    expect(within(thread).queryByText(/engagement checklist\.docx/i)).toBeNull();
    expect(within(thread).queryByText("Cloud")).toBeNull();

    // Leave it as it was for the other tests.
    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    fireEvent.click(await screen.findByRole("checkbox", { name: /search the firm cloud index/i }));
  });

  it("lists supported connectors under Sources and adds one once it is approved", async () => {
    setPreviewConnections([]);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    // Files stay the first tab.
    expect(screen.getByText("Turnip Engagement.txt")).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));

    expect(await screen.findByText("Google Drive")).toBeTruthy();
    expect(screen.getByText("Microsoft 365")).toBeTruthy();
    // Dropbox and DocuSign can be connected like the others.
    expect((screen.getByRole("button", { name: /connect dropbox/i }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: /connect docusign/i }) as HTMLButtonElement).disabled).toBe(false);
    // Connectors on the roadmap are shown but cannot be used yet.
    const planned = screen.getByRole("button", { name: /box is coming soon/i }) as HTMLButtonElement;
    expect(planned.disabled).toBe(true);
    expect(screen.getByText("Nothing connected yet.")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /connect google drive/i }));
    // First, who the files are for. "Only me" is chosen unless the person picks otherwise.
    const chooser = await screen.findByRole("region", { name: /who can search these files/i });
    expect((within(chooser).getByRole("radio", { name: /only me/i }) as HTMLInputElement).checked).toBe(true);
    expect((within(chooser).getByRole("radio", { name: /entire workspace/i }) as HTMLInputElement).disabled).toBe(false);
    fireEvent.click(within(chooser).getByRole("button", { name: /continue to google drive/i }));
    // The sign-in opens in the browser; once approved the source shows up in the list.
    const list = await screen.findByRole("region", { name: /connected sources/i }, { timeout: 8000 });
    expect(await within(list).findByText(/12 files/i, {}, { timeout: 8000 })).toBeTruthy();
    expect(within(list).getByText("Active")).toBeTruthy();
    // It is private to the person who connected it, and they can open it to the workspace.
    expect(within(list).getByText(/only you/i)).toBeTruthy();
    expect(within(list).getByRole("button", { name: /share with workspace/i })).toBeTruthy();
    expect(await screen.findByRole("status")).toBeTruthy();
  }, 20000);

  it("explains what turning on company knowledge does before it is turned on", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^knowledge$/i }));
    expect(await screen.findByText(/text of your connected documents/i)).toBeTruthy();
    expect(screen.getByText(/\$1 to \$2 for every 1,000 documents/i)).toBeTruthy();
    expect(screen.getByText(/never included/i)).toBeTruthy();

    // It cannot be turned on without agreeing to what it does.
    const turnOn = screen.getByRole("button", { name: /turn on company knowledge/i }) as HTMLButtonElement;
    expect(turnOn.disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: /i understand my documents/i }));
    expect(turnOn.disabled).toBe(false);
    fireEvent.click(turnOn);

    expect(await screen.findByRole("heading", { name: /what the ai understands about your business/i })).toBeTruthy();
    expect(screen.getByText(/replaces and repairs roofs/i)).toBeTruthy();
    expect(screen.getByText("Roof replacement")).toBeTruthy();
    expect(screen.getByText(/read 84 of 120 documents/i)).toBeTruthy();
  });

  it("shows where a fact came from and lets an administrator confirm or correct it", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^knowledge$/i }));
    // Already turned on by the previous test in this browser preview.
    const fact = await screen.findByText(/late invoices accrue interest at 1\.5% a month after 30 days\.$/i);
    const card = fact.closest("article") as HTMLElement;
    expect(within(card).getByText("Engagement letter - Whitfield.docx")).toBeTruthy();
    expect(within(card).getByText(/“Late invoices accrue interest/i)).toBeTruthy();
    expect(within(card).getByText("Needs verification")).toBeTruthy();

    // The other fact is confirmed but its source changed since.
    expect(screen.getByText(/check again: its source changed/i)).toBeTruthy();

    fireEvent.click(within(card).getByRole("button", { name: /edit/i }));
    const box = within(card).getByRole("textbox", { name: /corrected text/i });
    fireEvent.change(box, { target: { value: "Late invoices accrue 2% interest a month." } });
    fireEvent.click(within(card).getByRole("button", { name: /save and confirm/i }));
    await waitFor(() => expect(screen.queryByText(/accrue interest at 1\.5%/i)).toBeNull());

    fireEvent.click(screen.getByRole("tab", { name: /confirmed facts/i }));
    expect(await screen.findByText("Late invoices accrue 2% interest a month.")).toBeTruthy();
  });

  it("lets the business profile be confirmed", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^knowledge$/i }));
    const profile = (await screen.findByRole("region", { name: /business profile/i })) as HTMLElement;
    expect(within(profile).getByText("Needs verification")).toBeTruthy();
    fireEvent.click(within(profile).getByRole("button", { name: /this is right/i }));
    await waitFor(() => expect(within(profile).getByText("Confirmed")).toBeTruthy());
  });

  it("shows this month's AI allowance and lets it be changed", async () => {
    render(<App />);
    expect(await screen.findByText("$0.34 of $15.00")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    expect(await screen.findByText(/used \$0\.34 of \$15\.00 of AI answers this month \(24 answers\), the standard limit/i)).toBeTruthy();
    const input = screen.getByRole("spinbutton", { name: /monthly ai limit per person/i });
    const save = screen.getByRole("button", { name: /set limit/i });
    expect((save as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(input, { target: { value: "20" } });
    fireEvent.click(save);
    expect(await screen.findByText(/of \$20\.00 of AI answers this month/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /use standard limit/i })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /use standard limit/i }));
    expect(await screen.findByText(/of \$15\.00 of AI answers this month/i)).toBeTruthy();
  });

  it("lets advanced analysis be turned on in the chat, and says it uses more of the allowance", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    const choice = (await screen.findByRole("combobox", { name: /analysis/i })) as HTMLSelectElement;
    expect(choice.value).toBe("standard");
    expect(screen.queryByText(/about 20× more of your monthly allowance/i)).toBeNull();

    fireEvent.change(choice, { target: { value: "advanced" } });
    expect(screen.getByText(/about 20× more of your monthly allowance/i)).toBeTruthy();

    fireEvent.change(choice, { target: { value: "standard" } });
    expect(screen.queryByText(/about 20× more of your monthly allowance/i)).toBeNull();
  });

  it("offers advanced analysis when there was no answer, and uses it for the retry", async () => {
    const asked: Array<[string, boolean | undefined]> = [];
    const api = await import("./api");
    const original = api.answerQuestion;
    const spy = vi.spyOn(api, "answerQuestion").mockImplementation(async (question, advanced) => {
      asked.push([question, advanced]);
      return original(question, advanced);
    });
    render(<App />);
    const thread = await askFromHome("spaceship launch code");
    const retry = await within(thread).findByRole("button", { name: /try again with advanced analysis/i });
    fireEvent.click(retry);
    await waitFor(() => expect(asked.length).toBe(2));
    expect(asked).toEqual([["spaceship launch code", false], ["spaceship launch code", true]]);
    spy.mockRestore();
  });

  it("shows how far a sync has got and why some files are stored by name only", async () => {
    const now = Math.floor(Date.now() / 1000);
    setPreviewConnections([
      {
        id: "a1", kind: "google_drive", status: "active", last_sync_epoch: null, last_error: null, documents: 900,
        syncing: true, sync_started_epoch: now - 600, sync_files_read: 600, sync_files_failed: 2,
        readable: 540, name_only_reasons: { no_text: 300, unsupported_type: 60 },
      },
    ]);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));

    expect(await screen.findByText(/600 files read so far · about 60 a minute · running for 10 min/i)).toBeTruthy();
    expect(screen.getByText(/540 searchable by content/i)).toBeTruthy();
    expect(screen.getByText(/2 files could not be read this time/i)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /why\?/i }));
    expect(await screen.findByText(/no text found, even after reading scans/i)).toBeTruthy();
    expect(screen.getByText("300")).toBeTruthy();
  });

  it("syncs, reconnects and disconnects a connected source", async () => {
    setPreviewConnections([
      { id: "a1", kind: "google_drive", status: "active", last_sync_epoch: Math.floor(Date.now() / 1000) - 600, last_error: null, documents: 954 },
      { id: "m1", kind: "microsoft365", status: "needs_reauth", last_sync_epoch: null, last_error: "Reconnect required", documents: 0 },
    ]);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));

    expect(await screen.findByText(/954 files · Synced 10 min ago/i)).toBeTruthy();
    expect(screen.getByText("Needs reconnecting")).toBeTruthy();
    expect(screen.getByText("Reconnect required")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /sync google drive now/i }));
    expect(await screen.findByText(/sync requested/i)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /^reconnect$/i }));
    expect(await screen.findByText("Active", {}, { timeout: 8000 })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /disconnect microsoft 365/i }));
    await waitFor(() => expect(screen.queryByRole("button", { name: /disconnect microsoft 365/i })).toBeNull());
    expect(window.confirm).toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /disconnect google drive/i })).toBeTruthy();
    setPreviewConnections([]);
  }, 20000);

  it("still lists Google Drive and Microsoft 365, unusable, when the server cannot be reached", async () => {
    setPreviewServerDown(true);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.getByText("Google Drive")).toBeTruthy();
    expect(screen.getByText("Microsoft 365")).toBeTruthy();
    expect((screen.getByRole("button", { name: /connect google drive/i }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: /connect microsoft 365/i }) as HTMLButtonElement).disabled).toBe(true);
    setPreviewServerDown(false);
  });

  it("gives cloud results a link that opens the original file", async () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    render(<App />);
    const thread = await askFromHome("engagement checklist");
    const link = await within(thread).findAllByRole("button", { name: /^open engagement checklist\.docx$/i });
    fireEvent.click(link[0]);
    expect(open).toHaveBeenCalledWith("https://drive.google.com/open?id=sample-checklist", "_blank", "noopener,noreferrer");
  });

  it("shows what needs attention on Home, and each item opens where it can be dealt with", async () => {
    render(<App />);
    const attention = await screen.findByRole("region", { name: /needs your attention/i });
    // The sample drafts a helper prepared are waiting for approval.
    expect((await within(attention).findAllByText(/prepared an? (email|meeting) for you to approve/i)).length).toBeGreaterThan(0);
    expect(within(attention).getAllByRole("button").length).toBeGreaterThan(0);
    fireEvent.click(within(attention).getAllByRole("button")[0]);
    // It leaves Home for the Agents page, where the draft can be read.
    await waitFor(() => expect(screen.queryByRole("region", { name: /needs your attention/i })).toBeNull());
    expect(screen.getByRole("button", { name: /^agents$/i }).className).toContain("active");
  });

  it("lists recent chats on Home and reopens one", async () => {
    render(<App />);
    const thread = await askFromHome("What does the engagement letter say about payment");
    await within(thread).findByRole("button", { name: /^sources/i });
    fireEvent.click(screen.getByRole("button", { name: /^home$/i }));
    const recent = await screen.findByRole("region", { name: /recent chats/i });
    const entry = within(recent).getByRole("button", { name: /what does the engagement letter say about payment/i });
    fireEvent.click(entry);
    // The conversation is still there: chats are kept when going to another page.
    expect((await within(await screen.findByLabelText("Conversation")).findAllByText(/payment is due/i)).length).toBeGreaterThan(0);
  });

  it("asks a question typed on Home exactly once, even when effects run twice", async () => {
    render(<StrictMode><App /></StrictMode>);
    const thread = await askFromHome("What does the engagement letter say about payment");
    await within(thread).findByRole("button", { name: /^sources/i });
    expect(within(thread).getAllByText(/what does the engagement letter say about payment/i).length).toBe(1);
    expect(screen.getAllByRole("button", { name: /what does the engagement letter say about payment/i }).length).toBe(1);
  });

  it("carries a file attached on Home, and the chosen place to look, into the chat", async () => {
    const seen: Array<{ attachments?: string[]; scope?: string | null }> = [];
    const original = api.answerQuestion;
    vi.spyOn(api, "answerQuestion").mockImplementation(async (question, advanced, earlier, report, options) => {
      seen.push({ attachments: options?.attachments, scope: options?.scope });
      return original(question, advanced, earlier, report, options);
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /attach a file/i }));
    const included = await screen.findByRole("list", { name: /included with your question/i });
    expect(within(included).getByText(/example contract\.pdf/i)).toBeTruthy();

    // Where to look: the label says what is chosen.
    fireEvent.click(screen.getByRole("button", { name: /all sources/i }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /this device only/i }));
    expect(screen.getByRole("button", { name: /this device only/i })).toBeTruthy();

    const thread = await askFromHome("What does the contract say about payment");
    await within(thread).findByRole("button", { name: /^sources/i });
    expect(seen.length).toBe(1);
    expect(seen[0].attachments?.length).toBe(1);
    expect(seen[0].scope).toBe("local");
    // They stay on the chat's message box for the next question, and Home starts empty again.
    expect(within(await screen.findByRole("list", { name: /included with your message/i })).getByText(/example contract\.pdf/i)).toBeTruthy();
  });

  it("limits the firm cloud to chosen sources and kinds of file, with a search, and sends only those", async () => {
    setPreviewConnections([
      { id: "d1", kind: "google_drive", status: "active", last_sync_epoch: 1, last_error: null, documents: 20, scope: "workspace", yours: true, label: null },
      { id: "x1", kind: "dropbox", status: "active", last_sync_epoch: 1, last_error: null, documents: 5, scope: "workspace", yours: true, label: null },
    ]);
    const seen: Array<{ scope?: string | null; sources?: string[]; types?: string[] }> = [];
    const original = api.answerQuestion;
    vi.spyOn(api, "answerQuestion").mockImplementation(async (question, advanced, earlier, report, options) => {
      seen.push({ scope: options?.scope, sources: options?.sources, types: options?.types });
      return original(question, advanced, earlier, report, options);
    });
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /all sources/i }));
      // The list of sources and kinds only shows once the firm cloud is chosen.
      expect(screen.queryByRole("textbox", { name: /search sources and file types/i })).toBeNull();
      fireEvent.click(screen.getByRole("menuitemradio", { name: /firm cloud only/i }));
      const panel = screen.getByRole("dialog", { name: /where to look/i });
      const find = within(panel).getByRole("textbox", { name: /search sources and file types/i });
      await within(panel).findByRole("checkbox", { name: /google drive/i });
      expect(within(panel).getByRole("checkbox", { name: /dropbox/i })).toBeTruthy();
      expect(within(panel).getByRole("checkbox", { name: /documents/i })).toBeTruthy();

      // Typing narrows the list: "slides" finds Presentations, and no source.
      fireEvent.change(find, { target: { value: "slides" } });
      expect(within(panel).getByRole("checkbox", { name: /presentations/i })).toBeTruthy();
      expect(within(panel).queryByRole("checkbox", { name: /google drive/i })).toBeNull();
      expect(within(panel).queryByRole("checkbox", { name: /spreadsheets/i })).toBeNull();
      fireEvent.change(find, { target: { value: "zzzz" } });
      expect(within(panel).getByText(/nothing matches/i)).toBeTruthy();
      fireEvent.change(find, { target: { value: "" } });

      // Choose Google Drive and Documents: the button says so.
      fireEvent.click(within(panel).getByRole("checkbox", { name: /google drive/i }));
      fireEvent.click(within(panel).getByRole("checkbox", { name: /documents/i }));
      fireEvent.click(within(panel).getByRole("button", { name: /^done$/i }));
      expect(screen.getByRole("button", { name: /firm cloud · google drive, documents/i })).toBeTruthy();

      const thread = await askFromHome("engagement checklist letter");
      await within(thread).findByRole("button", { name: /^sources/i });
      expect(seen).toEqual([{ scope: "cloud", sources: ["google_drive"], types: ["document"] }]);
      // Only the firm cloud's Word document came back: no file from this device.
      expect(within(thread).getByRole("button", { name: /^\d+\s*engagement checklist\.docx/i })).toBeTruthy();
      expect(within(thread).queryByText(/turnip engagement\.txt/i)).toBeNull();
      // And it carries on to the chat's message box, where it can be changed or removed.
      expect(within(await screen.findByRole("list", { name: /included with your message/i })).getByText(/firm cloud · google drive, documents/i)).toBeTruthy();
    } finally {
      setPreviewConnections([]);
    }
  });

  it("says a file is attached in the chat's message box, and shows it under the question once sent", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.click(await screen.findByRole("button", { name: /add to your message/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: /attach file/i }));
    const chips = await screen.findByRole("list", { name: /included with your message/i });
    expect(within(chips).getByText("Attached")).toBeTruthy();
    expect(within(chips).getByText(/example contract\.pdf/i)).toBeTruthy();

    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "What does the contract say about payment" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    const sent = await within(thread).findByRole("list", { name: /sent with this question/i });
    expect(within(sent).getByText(/example contract\.pdf/i)).toBeTruthy();
  });

  it("writes an answer out a few words at a time, and shows its sources once it is all there", async () => {
    setTyping(true);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
      fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "What does the engagement letter say about payment" } });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      const thread = await screen.findByLabelText("Conversation");
      // Part way through: some of the answer is there, but not the end, and not the sources yet.
      await waitFor(() => expect(within(thread).queryByText(/engagement covers/i)).not.toBeNull(), { timeout: 3000 });
      expect(within(thread).queryByText(/signed engagement letter before any work begins/i)).toBeNull();
      expect(within(thread).queryByRole("button", { name: /^sources/i })).toBeNull();
      // Then all of it, and the sources.
      await within(thread).findByText(/signed engagement letter before any work begins/i, undefined, { timeout: 6000 });
      expect(await within(thread).findByRole("button", { name: /^sources/i }, { timeout: 3000 })).toBeTruthy();
    } finally {
      setTyping(false);
    }
  }, 15000);

  it("turns the chat into regular Claude on the Web tab, and carries that research into a question about the documents", async () => {
    const docsCalls: Array<{ question: string; earlier: Array<{ question: string; answer: string; web?: boolean }> }> = [];
    const webCalls: Array<{ question: string; earlier: Array<{ web?: boolean }> }> = [];
    const original = api.answerQuestion;
    vi.spyOn(api, "answerQuestion").mockImplementation(async (question, advanced, earlier, report, options) => {
      docsCalls.push({ question, earlier: earlier ?? [] });
      return original(question, advanced, earlier, report, options);
    });
    const originalWeb = api.askWeb;
    vi.spyOn(api, "askWeb").mockImplementation(async (question, advanced, earlier) => {
      webCalls.push({ question, earlier: earlier ?? [] });
      return originalWeb(question, advanced, earlier);
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    const webButton = await screen.findByRole("button", { name: /^web$/i });
    expect(webButton.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(webButton);
    expect(webButton.getAttribute("aria-pressed")).toBe("true");
    // Regular Claude: it says so, offers no document tools, and asks for anything.
    expect(screen.getByRole("heading", { name: /^ask anything$/i })).toBeTruthy();
    // Nothing is written inside the message box about it.
    expect(screen.queryByRole("note")).toBeNull();
    // No filters or tagging here, but a file can still be attached; and nothing on screen says "Claude".
    expect(screen.queryByRole("button", { name: /add to your message/i })).toBeNull();
    expect(screen.getByRole("button", { name: "Attach file" })).toBeTruthy();
    expect(screen.getByRole("textbox", { name: "Message" }).getAttribute("placeholder")).toMatch(/ask anything, or research the web/i);
    expect(document.querySelector(".chat-main")?.textContent ?? "").not.toMatch(/claude/i);

    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "find mens retreat bible verse" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    expect((await within(thread).findAllByText(/proverbs 27:17/i)).length).toBeGreaterThan(0);
    // Its sources are web pages, which open in the browser, not files.
    fireEvent.click(await within(thread).findByRole("button", { name: /^sources/i }));
    const panel = await screen.findByLabelText("Search results");
    expect(within(panel).getAllByText("Web").length).toBe(2);
    expect(docsCalls.length).toBe(0);
    expect(webCalls).toEqual([{ question: "find mens retreat bible verse", earlier: [] }]);

    // Back to Documents: the question about the plan goes with the web research as its context.
    // One button: clicking it again turns the web off.
    fireEvent.click(screen.getByRole("button", { name: /^web$/i }));
    expect(screen.getByRole("button", { name: /^web$/i }).getAttribute("aria-pressed")).toBe("false");
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "how can I modify my mens retreat planning to follow these verses" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(docsCalls.length).toBe(1));
    expect(docsCalls[0].earlier.length).toBe(1);
    expect(docsCalls[0].earlier[0]).toMatchObject({ question: "find mens retreat bible verse", web: true });
    expect(docsCalls[0].earlier[0].answer).toMatch(/proverbs 27:17/i);

    // And the web tab never receives what was answered from the documents.
    fireEvent.click(screen.getByRole("button", { name: /^web$/i }));
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "explain Hebrews 10:24 more" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(webCalls.length).toBe(2));
    expect(webCalls[1].earlier.length).toBe(1);
    expect(webCalls[1].earlier.every((turn) => turn.web === true)).toBe(true);
  });

  it("lets a file be attached on the web tab, and sends it with the web question", async () => {
    const sent: Array<string[] | undefined> = [];
    const originalWeb = api.askWeb;
    vi.spyOn(api, "askWeb").mockImplementation(async (question, advanced, earlier, attachments) => {
      sent.push(attachments);
      return originalWeb(question, advanced, earlier, attachments);
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.click(await screen.findByRole("button", { name: /^web$/i }));
    fireEvent.click(screen.getByRole("button", { name: "Attach file" }));
    const chips = await screen.findByRole("list", { name: /included with your message/i });
    expect(within(chips).getByText("Attached")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "summarize this and find related verses" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    expect(await within(thread).findByRole("list", { name: /sent with this question/i })).toBeTruthy();
    expect(sent.length).toBe(1);
    expect(sent[0]?.length).toBe(1);
  });

  it("writes an email from a request in the chat, shows it as a preview to change, and sends it from Outlook only when Send is pressed", async () => {
    setPreviewEmailSending(false);
    setPreviewConnections([]);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
      fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "draft an email to schedule a time for a zoom meeting on nov 10 at 10am" } });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      const card = await screen.findByRole("region", { name: /email preview/i });
      // Filled in from the request: the subject, the date and the time, and a greeting and sign-off.
      expect((within(card).getByLabelText("Subject") as HTMLInputElement).value).toBe("Zoom meeting on November 10 at 10:00 AM");
      const body = within(card).getByLabelText("Message body") as HTMLTextAreaElement;
      expect(body.value).toMatch(/^Hello,/);
      expect(body.value).toMatch(/November 10 at 10:00 AM/);
      expect(body.value).toMatch(/Best regards,$/);
      // It cannot be sent until there is someone to send it to.
      const send = within(card).getByRole("button", { name: /^send$/i }) as HTMLButtonElement;
      expect(send.disabled).toBe(true);
      fireEvent.change(within(card).getByLabelText("To"), { target: { value: "sam@client.example" } });
      expect(send.disabled).toBe(false);
      expect(api.lastPreviewEmail).toBeNull();

      // Not allowed yet, then Outlook not connected: said plainly, and nothing is sent.
      fireEvent.click(send);
      expect((await within(card).findByRole("alert")).textContent).toMatch(/switched off/i);
      setPreviewEmailSending(true);
      fireEvent.click(within(card).getByRole("button", { name: /^send$/i }));
      await waitFor(() => expect(within(card).getByRole("alert").textContent).toMatch(/connect your outlook first/i));
      setPreviewConnections([{ id: "mail-1", kind: "outlook_mail", status: "active", last_sync_epoch: null, last_error: null, documents: 0, scope: "private", yours: true, label: "you@outlook.example" }]);

      // Someone outside the firm: she is asked once more, naming the address, and then it goes as shown.
      fireEvent.change(within(card).getByLabelText("Subject"), { target: { value: "Zoom on Nov 10, 10am" } });
      fireEvent.click(within(card).getByRole("button", { name: /^send$/i }));
      const confirm = await within(card).findByText(/outside your organisation \(sam@client\.example\)/i);
      expect(confirm).toBeTruthy();
      expect(api.lastPreviewEmail).toBeNull();
      fireEvent.click(within(card).getByRole("button", { name: /send anyway/i }));
      expect((await within(card).findByRole("status")).textContent).toMatch(/sent from you@outlook\.example/i);
      expect(api.lastPreviewEmail).toEqual({ to: ["sam@client.example"], subject: "Zoom on Nov 10, 10am", body: body.value, confirmOutside: true });
      // Once sent, it is locked and cannot be sent twice.
      expect(within(card).queryByRole("button", { name: /^send$/i })).toBeNull();
      expect((within(card).getByLabelText("Subject") as HTMLInputElement).disabled).toBe(true);
    } finally {
      setPreviewEmailSending(false);
      setPreviewConnections([]);
    }
  });

  it("adds a real Zoom link to a meeting email, made in the person's own Zoom only when they ask for it", async () => {
    setPreviewConnections([]);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
      fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "draft an email to sam@client.example to schedule a zoom meeting on nov 10 at 10am" } });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      const card = await screen.findByRole("region", { name: /email preview/i });
      const zoom = within(card).getByLabelText("Zoom meeting");
      expect(within(zoom).getByText(/10:00/)).toBeTruthy();
      expect((within(card).getByLabelText("To") as HTMLInputElement).value).toBe("sam@client.example");
      expect((within(card).getByLabelText("Message body") as HTMLTextAreaElement).value).toMatch(/Zoom link/);
      expect(api.lastPreviewZoom).toBeNull();

      // Zoom is not connected: said plainly, and no meeting is made.
      fireEvent.click(within(zoom).getByRole("button", { name: /add a zoom link/i }));
      await waitFor(() => expect(within(card).getByRole("alert").textContent).toMatch(/connect your zoom first/i));
      expect(api.lastPreviewZoom).toBeNull();

      // Connected: the meeting is made as the card says, and its link goes into the message.
      setPreviewConnections([{ id: "zoom-1", kind: "zoom", status: "active", last_sync_epoch: null, last_error: null, documents: 0, scope: "private", yours: true, label: "you@zoom.example" }]);
      fireEvent.change(within(zoom).getByLabelText("Length"), { target: { value: "45" } });
      fireEvent.click(within(zoom).getByRole("button", { name: /add a zoom link/i }));
      await within(card).findByText(/zoom link added/i);
      expect(api.lastPreviewZoom).toMatchObject({ topic: "Zoom meeting on November 10 at 10:00 AM", duration_minutes: 45 });
      expect(api.lastPreviewZoom?.start).toMatch(/^\d{4}-11-10T10:00$/);
      const body = (within(card).getByLabelText("Message body") as HTMLTextAreaElement).value;
      expect(body).toContain("https://us06web.zoom.us/j/81234567890?pwd=preview");
      expect(body).toContain("Meeting ID: 812 3456 7890");
      expect(body).not.toMatch(/will send the Zoom link/);
      expect(body).toMatch(/Best regards,$/);
    } finally {
      setPreviewConnections([]);
    }
  });

  it("turns a plain request to have a meeting into an email preview with a Zoom row, without the word email", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "i want to have a meeting with a client at 10 am at nov 10" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const card = await screen.findByRole("region", { name: /email preview/i });
    expect(within(within(card).getByLabelText("Zoom meeting")).getByText(/10:00/)).toBeTruthy();
    expect(within(card).getByRole("button", { name: /add a zoom link/i })).toBeTruthy();
  });

  it("keeps everything about a client in one workspace: its files, its saved chats, and answers drawn only from those files", async () => {
    resetPreviewSpaces();
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^workspaces$/i }));
    expect(await screen.findByLabelText("No workspaces yet")).toBeTruthy();
    fireEvent.click(screen.getAllByRole("button", { name: /new workspace/i })[0]);
    // A name is needed, and nothing is made until it is given.
    expect((screen.getByRole("button", { name: "Create workspace" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Workspace name"), { target: { value: "Steve Miller" } });
    fireEvent.change(screen.getByLabelText("What it is for"), { target: { value: "Custody case" } });
    fireEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    expect(await screen.findByRole("heading", { name: "Steve Miller" })).toBeTruthy();
    expect(screen.getByText("Custody case")).toBeTruthy();
    expect(screen.getByText(/no files in this workspace yet/i)).toBeTruthy();

    // Files from this computer are added from the Files tab.
    fireEvent.click(screen.getByRole("tab", { name: /files/i }));
    fireEvent.click(await screen.findByRole("button", { name: /add files/i }));
    await screen.findByText(/added 2 files to steve miller/i);
    const list = await screen.findByLabelText("Files in this workspace");
    expect(within(list).getByText("Retainer agreement.pdf")).toBeTruthy();
    expect(within(list).getAllByText(/on this computer/i).length).toBe(2);

    // A question is answered from those files, and from nothing else.
    fireEvent.click(screen.getByRole("tab", { name: /chat/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "when is the hearing" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const used = await screen.findByLabelText("Documents used for this answer");
    expect(within(used).getByText(/retainer agreement\.pdf/i)).toBeTruthy();
    expect(within(used).queryByText(/engagement/i)).toBeNull();

    // Leaving and coming back finds the chat where it was left.
    fireEvent.click(screen.getByRole("button", { name: /^home$/i }));
    fireEvent.click(await screen.findByRole("button", { name: /^workspaces$/i }));
    const card = await screen.findByRole("button", { name: "Open Steve Miller" });
    expect(within(card).getByText(/2 files/)).toBeTruthy();
    fireEvent.click(card);
    fireEvent.click(await screen.findByRole("button", { name: "when is the hearing" }));
    expect((await screen.findAllByText(/retainer agreement/i)).length).toBeGreaterThan(0);
    expect(screen.getAllByText("when is the hearing").length).toBeGreaterThan(1);

    // A chat can be deleted, a file taken out, and the workspace renamed and deleted.
    fireEvent.click(screen.getByRole("button", { name: /delete the chat when is the hearing/i }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "when is the hearing" })).toBeNull());
    fireEvent.click(screen.getByRole("tab", { name: /files/i }));
    fireEvent.click(await screen.findByRole("button", { name: "Remove Retainer agreement.pdf" }));
    // It asks first: "Keep it" leaves the file in, and nothing is removed until the person confirms.
    const sure = await screen.findByRole("alertdialog", { name: /confirm removing a file/i });
    expect(within(sure).getByText(/the file itself is not touched/i)).toBeTruthy();
    fireEvent.click(within(sure).getByRole("button", { name: "Keep it" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(within(screen.getByLabelText("Files in this workspace")).getByText("Retainer agreement.pdf")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Remove Retainer agreement.pdf" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Yes, remove it" }));
    await screen.findByText(/removed retainer agreement\.pdf/i);
    expect(within(screen.getByLabelText("Files in this workspace")).queryByText("Retainer agreement.pdf")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Edit this workspace" }));
    fireEvent.change(screen.getByLabelText("Workspace name"), { target: { value: "Steve M." } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("heading", { name: "Steve M." })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Delete this workspace" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete workspace" }));
    expect(await screen.findByLabelText("No workspaces yet")).toBeTruthy();
  });

  it("will not make two workspaces with one name", async () => {
    resetPreviewSpaces();
    await api.spaceCreate("Steve", "");
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^workspaces$/i }));
    fireEvent.click((await screen.findAllByRole("button", { name: /new workspace/i }))[0]);
    fireEvent.change(screen.getByLabelText("Workspace name"), { target: { value: "  steve " } });
    fireEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/already have a workspace with that name/i);
    resetPreviewSpaces();
  });

  it("switches to dark mode from the sidebar or Settings and remembers the choice", async () => {
    localStorage.removeItem("prism-theme");
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^dark mode$/i }));
      expect(document.documentElement.dataset.theme).toBe("dark");
      expect(localStorage.getItem("prism-theme")).toBe("dark");
      // The button now offers the way back, and Settings shows the same choice.
      fireEvent.click(screen.getByRole("button", { name: /settings/i }));
      const choices = screen.getByRole("radiogroup", { name: /appearance/i });
      expect(within(choices).getByRole("radio", { name: "Dark" }).getAttribute("aria-checked")).toBe("true");
      fireEvent.click(within(choices).getByRole("radio", { name: "Light" }));
      expect(document.documentElement.dataset.theme).toBe("light");
      expect(localStorage.getItem("prism-theme")).toBe("light");
      fireEvent.click(within(choices).getByRole("radio", { name: "System" }));
      expect(localStorage.getItem("prism-theme")).toBeNull();
    } finally {
      localStorage.removeItem("prism-theme");
      document.documentElement.removeAttribute("data-theme");
    }
  });

  it("lists the Zoom meetings made from the app and moves or cancels them only when the person chooses", async () => {
    setPreviewConnections([{ id: "zoom-1", kind: "zoom", status: "active", last_sync_epoch: null, last_error: null, documents: 0, scope: "private", yours: true, label: "you@zoom.example" }]);
    try {
      await api.createZoomMeeting({ topic: "Planning call", start: "2999-11-10T10:00", duration_minutes: 45, timezone: "America/Chicago", agenda: "" });
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /connectors/i }));
      const all = await screen.findByLabelText("Zoom meetings made here");
      // The preview keeps meetings from earlier tests too: work with this one's own row.
      const list = (await within(all).findByText("Planning call")).closest(".zoom-meeting") as HTMLElement;
      expect(within(list).getByText(/45 min/)).toBeTruthy();

      // Moving needs the person to pick a time and press Save: nothing changes before that.
      fireEvent.click(within(list).getByRole("button", { name: /move/i }));
      expect(api.lastPreviewZoomChange).toBeNull();
      fireEvent.change(within(list).getByLabelText("New date"), { target: { value: "2999-11-12" } });
      fireEvent.change(within(list).getByLabelText("New time"), { target: { value: "14:30" } });
      fireEvent.change(within(list).getByLabelText("New length"), { target: { value: "60" } });
      fireEvent.click(within(list).getByRole("button", { name: /save new time/i }));
      await within(all).findByText(/meeting moved/i);
      expect(api.lastPreviewZoomChange).toMatchObject({ action: "moved", start: "2999-11-12T14:30", duration_minutes: 60 });
      expect(within(list).getByText(/60 min/)).toBeTruthy();

      // Cancelling asks again, and "Keep it" leaves the meeting alone.
      fireEvent.click(within(list).getByRole("button", { name: /cancel meeting/i }));
      fireEvent.click(within(list).getByRole("button", { name: /keep it/i }));
      expect(api.lastPreviewZoomChange?.action).toBe("moved");
      fireEvent.click(within(list).getByRole("button", { name: /cancel meeting/i }));
      fireEvent.click(within(list).getByRole("button", { name: /yes, cancel it/i }));
      await within(all).findByText(/^meeting cancelled/i);
      expect(api.lastPreviewZoomChange?.action).toBe("cancelled");
      await waitFor(() => expect(within(all).queryByText("Planning call")).toBeNull());
    } finally {
      setPreviewConnections([]);
    }
  });

  it("lets a task be given to a helper from Home", async () => {
    await cloudAgentCreate({ name: "Zed", role: "Checks", description: "Checks.", instructions: "Check.", tools: ["search_documents"], model: "standard", effort: "low" });
    render(<App />);
    const helpers = await screen.findByRole("group", { name: /give a task to a helper/i });
    fireEvent.click(within(helpers).getByRole("button", { name: /zed/i }));
    expect((screen.getByRole("textbox", { name: /ask your workspace/i }) as HTMLTextAreaElement).placeholder).toBe("Give Zed a task…");
  });

  it("lets an administrator create the research helper, ask it something, and read the report", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    // The page explains itself in three steps.
    expect(await screen.findByRole("list", { name: /how it works/i })).toBeTruthy();
    // Helpers that do not exist yet are only mentioned, not shown as if they worked.
    expect(await screen.findByText(/more helpers are on the way/i)).toBeTruthy();
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    // Ready-made helpers to start from, and a blank one.
    const picker = await screen.findByRole("region", { name: /choose a helper to start from/i });
    expect(within(picker).getByRole("button", { name: /start from scratch/i })).toBeTruthy();
    expect(within(picker).getAllByRole("button", { name: /use the template/i }).length).toBeGreaterThanOrEqual(8);
    fireEvent.click(within(picker).getByRole("button", { name: /use the template: research & reports/i }));
    const editor = await screen.findByRole("region", { name: /new helper/i });
    expect((within(editor).getByLabelText("Name") as HTMLInputElement).value).toBe("John");
    expect(within(editor).getByText(/it can only read, never change anything/i)).toBeTruthy();
    fireEvent.click(within(editor).getByRole("button", { name: /create helper/i }));

    const john = (await screen.findByRole("heading", { name: "John" })).closest("article") as HTMLElement;
    fireEvent.click(within(john).getByRole("button", { name: /ask john/i }));
    const ask = await screen.findByRole("region", { name: /ask john/i });
    fireEvent.change(within(ask).getByRole("textbox", { name: "Your question" }), { target: { value: "What is the retreat budget?" } });
    fireEvent.click(within(ask).getByRole("button", { name: /^ask john$/i }));

    const details = await screen.findByRole("region", { name: /task details/i });
    expect((await within(details).findAllByText(/sample report/i)).length).toBeGreaterThan(0);
    expect(within(details).getByText(/searched documents/i)).toBeTruthy();
    // The report is formatted, and the automatic check says what could not be found.
    expect(within(details).getByRole("table")).toBeTruthy();
    expect(within(details).queryByText(/\|---\|/)).toBeNull();
    // The report is a card first: who made it, whether it held up, and two ways to question it.
    const card = within(details).getByRole("region", { name: "Report" });
    expect(within(card).getByText(/created by john/i)).toBeTruthy();
    expect(within(card).getByText(/1 to check/i)).toBeTruthy();
    expect(within(card).getByText("0 sources")).toBeTruthy();
    // The evidence is one press away.
    fireEvent.click(within(details).getByRole("button", { name: /view evidence/i }));
    const check = within(details).getByRole("region", { name: /report check/i });
    expect(within(check).getByText(/4 of 5 figures appear/i)).toBeTruthy();
    expect(within(check).getByText(/\$4,006\.75/)).toBeTruthy();
    // Questions about the report are answered from it and its files, and are not kept.
    fireEvent.click(within(details).getByRole("button", { name: /ask about report/i }));
    const reportAsk = within(details).getByRole("region", { name: /ask about this report/i });
    fireEvent.change(within(reportAsk).getByRole("textbox", { name: /question about this report/i }), { target: { value: "Which figures should I double-check?" } });
    fireEvent.click(within(reportAsk).getByRole("button", { name: "Ask" }));
    expect(await within(reportAsk).findByText(/the report states this/i)).toBeTruthy();
    expect(within(reportAsk).getByText("The report")).toBeTruthy();
    fireEvent.change(within(reportAsk).getByRole("textbox", { name: /question about this report/i }), { target: { value: "What is the weather like?" } });
    fireEvent.click(within(reportAsk).getByRole("button", { name: "Ask" }));
    expect(await within(reportAsk).findByText(/do not say/i)).toBeTruthy();
    // The report can be saved as a Word or Excel file, made from the text shown.
    fireEvent.click(within(details).getByRole("button", { name: /save as excel/i }));
    expect(await within(details).findByText(/^Saved to /)).toBeTruthy();
    expect(api.lastPreviewExport?.format).toBe("xlsx");
    expect(api.lastPreviewExport?.text).toContain("|");
    fireEvent.click(within(details).getByRole("button", { name: /save as word/i }));
    await waitFor(() => expect(api.lastPreviewExport?.format).toBe("docx"));
  });

  it("starts a new helper completely empty, or from a template when one is chosen", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    expect(await screen.findByRole("heading", { name: "John" })).toBeTruthy();

    // From scratch: nothing is filled in, nothing ticked, and it cannot be created yet.
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    fireEvent.click(await screen.findByRole("button", { name: /start from scratch/i }));
    let editor = await screen.findByRole("region", { name: /new helper/i });
    for (const label of ["Name", "Role", "What it is for", "Instructions"]) {
      expect((within(editor).getByLabelText(label) as HTMLInputElement).value).toBe("");
    }
    for (const box of within(editor).getAllByRole("checkbox")) {
      if ((box as HTMLInputElement).closest("fieldset")) expect((box as HTMLInputElement).checked).toBe(false);
    }
    expect((within(editor).getByLabelText("Model") as HTMLSelectElement).value).toBe("");
    expect((within(editor).getByRole("button", { name: /create helper/i }) as HTMLButtonElement).disabled).toBe(true);
    expect(within(editor).queryByText(/based on the/i)).toBeNull();
    fireEvent.click(within(editor).getByRole("button", { name: /close editor/i }));

    // From a template: filled in, and the note says where it came from.
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    fireEvent.click(await screen.findByRole("button", { name: /use the template: check a budget or expense report/i }));
    editor = await screen.findByRole("region", { name: /new helper/i });
    expect((within(editor).getByLabelText("Name") as HTMLInputElement).value).toBe("Finn");
    expect((within(editor).getByLabelText("Instructions") as HTMLTextAreaElement).value).toMatch(/discrepancies found/i);
    expect(within(editor).getByText(/based on the “check a budget or expense report” template/i)).toBeTruthy();
    expect((within(editor).getByRole("button", { name: /create helper/i }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("drafts a helper from a described wish, for review before it is created", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    const draft = await screen.findByRole("button", { name: /draft this helper/i });
    expect((draft as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText(/describe the helper you want/i), { target: { value: "checks every engagement letter for a missing signature and emails me a list" } });
    fireEvent.click(screen.getByRole("button", { name: /draft this helper/i }));
    const editor = await screen.findByRole("region", { name: /new helper/i });
    expect(within(editor).getByText(/drafted from your description/i)).toBeTruthy();
    expect((within(editor).getByLabelText("Instructions") as HTMLTextAreaElement).value).toMatch(/engagement letter/i);
    // Nothing exists until the administrator creates it.
    expect((within(editor).getByRole("button", { name: /create helper/i }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("heading", { name: "Alex" })).toBeNull();
  });

  it("shows what was done to answer, only when asked", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "what does the engagement letter say about payment" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    const toggle = await within(thread).findByRole("button", { name: /show work/i });
    expect(within(thread).queryByLabelText("Steps taken to answer")).toBeNull();
    fireEvent.click(toggle);
    const steps = within(thread).getByLabelText("Steps taken to answer");
    expect(within(steps).getAllByRole("listitem")[0].textContent).toMatch(/searched .* for “/i);
    expect(steps.textContent).toMatch(/checked each statement against its source/i);
    fireEvent.click(toggle);
    expect(within(thread).queryByLabelText("Steps taken to answer")).toBeNull();
  });

  it("opens a long answer in a canvas to edit by hand, and saves what was edited", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "create me an excel sheet with the engagement letter payment terms" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    fireEvent.click(await within(thread).findByRole("button", { name: /open in canvas/i }));
    const canvas = await screen.findByLabelText("Canvas");
    const box = within(canvas).getByLabelText("Canvas text") as HTMLTextAreaElement;
    const original = box.value;
    expect(original).toMatch(/payment/i);
    expect((within(canvas).getByRole("button", { name: /go back to the original/i }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(box, { target: { value: "# Mine\n\nPayment is due in **45 days**." } });
    fireEvent.click(within(canvas).getByRole("tab", { name: "Preview" }));
    expect(within(canvas).getByText("45 days").tagName).toBe("STRONG");
    fireEvent.click(within(canvas).getByRole("tab", { name: "Edit" }));

    // What is saved is the edited text, and the chat's own answer is unchanged.
    fireEvent.click(within(canvas).getByRole("button", { name: /save as excel/i }));
    await waitFor(() => expect(api.lastPreviewExport?.text).toContain("45 days"));
    expect(within(thread).queryByText(/45 days/)).toBeNull();

    fireEvent.click(within(canvas).getByRole("button", { name: /go back to the original/i }));
    expect((within(canvas).getByLabelText("Canvas text") as HTMLTextAreaElement).value).toBe(original);
    fireEvent.click(within(canvas).getByRole("button", { name: /make the canvas wider/i }));
    expect(canvas.className).toContain("expanded");
    fireEvent.click(within(canvas).getByRole("button", { name: /close canvas/i }));
    expect(screen.queryByLabelText("Canvas")).toBeNull();
  });

  it("saves the canvas text to Google Drive only after the person approves exactly that text", async () => {
    setPreviewConnections([]);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
      fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "create me an excel sheet with the engagement letter payment terms" } });
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      fireEvent.click(await within(await screen.findByLabelText("Conversation")).findByRole("button", { name: /open in canvas/i }));
      const canvas = await screen.findByLabelText("Canvas");
      fireEvent.change(within(canvas).getByLabelText("Canvas text"), { target: { value: "# Mine\n\nPayment is due in 45 days." } });

      // Asking shows exactly what would be saved. Nothing is saved yet.
      fireEvent.click(within(canvas).getByRole("button", { name: /save to google drive/i }));
      const dialog = await screen.findByRole("dialog", { name: /save to google drive/i });
      expect((within(dialog).getByLabelText("Text that will be saved") as HTMLTextAreaElement).value).toBe("# Mine\n\nPayment is due in 45 days.");
      expect(api.lastPreviewDriveSave).toBeNull();

      // Without a Google Drive of their own it says so plainly, and nothing is saved.
      fireEvent.click(within(dialog).getByRole("button", { name: /approve and save/i }));
      await waitFor(() => expect(within(dialog).getByRole("alert").textContent).toMatch(/connect your own google drive first/i));
      expect(api.lastPreviewDriveSave).toBeNull();

      // Connected: the approved text, under the name shown, is what is saved.
      setPreviewConnections([{ id: "drive-1", kind: "google_drive", status: "active", last_sync_epoch: 1, last_error: null, documents: 12, scope: "private", yours: true, label: null }]);
      fireEvent.change(within(dialog).getByLabelText("File name"), { target: { value: "Payment terms" } });
      fireEvent.click(within(dialog).getByRole("button", { name: /approve and save/i }));
      expect((await within(dialog).findByRole("status")).textContent).toMatch(/saved to your google drive/i);
      expect(api.lastPreviewDriveSave).toEqual({ title: "Payment terms", content: "# Mine\n\nPayment is due in 45 days." });
    } finally {
      setPreviewConnections([]);
    }
  });

  it("connects a QuickBooks company for the person alone, with no choice of who sees it, and names the company", async () => {
    setPreviewConnections([]);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^connectors$/i }));
      fireEvent.click(await screen.findByRole("button", { name: /^connect quickbooks online$/i }));
      // It goes straight to sign-in: books are never shared with the whole workspace.
      expect(screen.queryByText(/who can search these files/i)).toBeNull();
      const connected = await screen.findByText(/quickbooks online: firm books ltd/i, undefined, { timeout: 8000 });
      expect(connected).toBeTruthy();
    } finally {
      setPreviewConnections([]);
    }
  }, 15000);

  it("narrows the connector list by words, by kind, and to the ones already connected", async () => {
    setPreviewConnections([{ id: "drive-1", kind: "google_drive", status: "active", last_sync_epoch: 1, last_error: null, documents: 12, scope: "private", yours: true, label: null }]);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^connectors$/i }));
      await screen.findByRole("button", { name: /connect another google drive/i });
      expect(screen.getByRole("button", { name: /^connect dropbox$/i })).toBeTruthy();
      expect(screen.getByRole("button", { name: /box is coming soon/i })).toBeTruthy();

      // Words match the name or what it does.
      fireEvent.change(screen.getByLabelText("Search connectors"), { target: { value: "invoices" } });
      expect(screen.getByRole("button", { name: /^connect quickbooks online$/i })).toBeTruthy();
      expect(screen.queryByRole("button", { name: /^connect dropbox$/i })).toBeNull();
      fireEvent.change(screen.getByLabelText("Search connectors"), { target: { value: "zzzz" } });
      expect(screen.getByText(/no connectors match/i)).toBeTruthy();
      fireEvent.click(screen.getByRole("button", { name: /clear search/i }));

      // A kind narrows it, planned ones included.
      const filters = screen.getByRole("group", { name: /filter connectors/i });
      fireEvent.click(within(filters).getByRole("button", { name: "Signing" }));
      expect(screen.getByRole("button", { name: /^connect docusign$/i })).toBeTruthy();
      expect(screen.queryByRole("button", { name: /^connect dropbox$/i })).toBeNull();
      fireEvent.click(within(filters).getByRole("button", { name: "Files" }));
      expect(screen.getByRole("button", { name: /box is coming soon/i })).toBeTruthy();
      expect(screen.queryByRole("button", { name: /clio is coming soon/i })).toBeNull();
      fireEvent.click(within(filters).getByRole("button", { name: "All" }));

      // Only what is connected.
      fireEvent.click(within(filters).getByRole("button", { name: "Connected" }));
      expect(screen.getByRole("button", { name: /connect another google drive/i })).toBeTruthy();
      expect(screen.queryByRole("button", { name: /^connect dropbox$/i })).toBeNull();
      expect(screen.queryByRole("button", { name: /box is coming soon/i })).toBeNull();
    } finally {
      setPreviewConnections([]);
    }
  });

  it("searches the connected sources with filters and shows counts for each source", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^search$/i }));
    const box = await screen.findByRole("textbox", { name: /search the firm's sources/i });
    fireEvent.change(box, { target: { value: "retreat" } });
    fireEvent.submit(box.closest("form") as HTMLFormElement);

    const results = await screen.findByRole("region", { name: /search results/i });
    expect(await within(results).findByText(/found 3 results/i)).toBeTruthy();
    const sources = screen.getByRole("complementary", { name: /^sources/i });
    expect(within(sources).getByText("Google Drive")).toBeTruthy();

    // Choosing one source narrows the results but keeps the other source listed.
    fireEvent.click(within(sources).getByRole("button", { name: /microsoft 365/i }));
    expect(await within(results).findByText(/found 1 result$/i)).toBeTruthy();
    expect(within(results).getByText("Retreat budget 2026.xlsx")).toBeTruthy();
    expect(within(sources).getByText("Google Drive")).toBeTruthy();
  });

  it("offers to save the answer as a file when the chat is asked to make one, and not otherwise", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    const box = await screen.findByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "create me an excel sheet with the engagement letter payment terms" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    const excel = await within(thread).findByRole("button", { name: /save as excel/i });
    expect(excel.className).toContain("export-first");
    fireEvent.click(excel);
    await waitFor(() => expect(api.lastPreviewExport?.format).toBe("xlsx"));
    expect(api.lastPreviewExport?.text).toMatch(/payment/i);

    // An ordinary question gets no save buttons.
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "what does the engagement letter say about payment" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(within(screen.getByLabelText("Conversation")).getAllByText(/payment/i).length).toBeGreaterThan(2));
    expect(within(screen.getByLabelText("Conversation")).getAllByRole("button", { name: /save as excel/i }).length).toBe(1);
  });

  it("opens the sources of an answer in a results panel beside the chat, in citation order, and closes it", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "what does the engagement letter say about payment" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    const toggle = await within(thread).findByRole("button", { name: /^sources/i });
    expect(screen.queryByLabelText("Search results")).toBeNull();
    fireEvent.click(toggle);
    const panel = await screen.findByLabelText("Search results");
    const names = within(panel).getAllByRole("listitem").map((item) => within(item).getAllByRole("button")[0].textContent);
    const cited = within(thread).getAllByLabelText("Documents used for this answer")[0].querySelectorAll(".chat-source > button:first-child");
    expect(names).toEqual(Array.from(cited).map((button) => button.textContent!.replace(/^\d+\s*/, "").trim()));
    fireEvent.click(within(panel).getByRole("button", { name: /make the results wider/i }));
    expect(panel.className).toContain("expanded");
    fireEvent.click(within(panel).getByRole("button", { name: /make the results narrower/i }));
    expect(panel.className).not.toContain("expanded");
    fireEvent.click(within(panel).getByRole("button", { name: /close search results/i }));
    expect(screen.queryByLabelText("Search results")).toBeNull();
  });

  it("tucks the list of chats away and brings it back, but never the main menu", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    expect(await screen.findByRole("complementary", { name: "Chats" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /hide the list of chats/i }));
    expect(screen.queryByRole("complementary", { name: "Chats" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /show the list of chats/i }));
    expect(await screen.findByRole("complementary", { name: "Chats" })).toBeTruthy();

    // The main menu stays where it is: it cannot be hidden.
    expect(screen.queryByRole("button", { name: /hide the menu/i })).toBeNull();
  });

  it("adds options to a chat message: attach a file, tag one, and limit where it looks", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    const plus = await screen.findByRole("button", { name: /add to your message/i });

    // Attach a file from this computer: it shows as a chip and can be taken off again.
    fireEvent.click(plus);
    const menu = screen.getByRole("menu", { name: /add to your message/i });
    expect(within(menu).getByText("Upload a local document")).toBeTruthy();
    fireEvent.click(within(menu).getByRole("menuitem", { name: /attach file/i }));
    const chips = await screen.findByRole("list", { name: /included with your message/i });
    expect(within(chips).getByText("Example contract.pdf")).toBeTruthy();

    // Tag one file from this device.
    fireEvent.click(plus);
    fireEvent.click(screen.getByRole("menuitem", { name: /tag a file/i }));
    const picker = screen.getByRole("dialog", { name: /tag a file/i });
    fireEvent.change(within(picker).getByRole("textbox", { name: /find a file/i }), { target: { value: "engagement" } });
    fireEvent.click(within(picker).getAllByRole("button")[0]);
    expect(within(screen.getByRole("list", { name: /included with your message/i })).getAllByRole("listitem").length).toBe(2);

    // Limit where it looks.
    fireEvent.click(plus);
    fireEvent.click(screen.getByRole("menuitem", { name: /filter data/i }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /this device only/i }));
    expect(within(screen.getByRole("list", { name: /included with your message/i })).getByText("This device only")).toBeTruthy();

    // The chosen file is part of the answer, and the chips stay for the next question.
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "What does it say about payment?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    await waitFor(() => expect(within(thread).queryByText(/searching your sources/i)).toBeNull());
    expect(within(thread).getAllByText(/was read for this answer/i).length).toBeGreaterThan(0);

    // Taking a chip off removes it; the main menu has no way to attach a fourth file.
    fireEvent.click(screen.getByRole("button", { name: "Remove the filter" }));
    expect(within(screen.getByRole("list", { name: /included with your message/i })).queryByText("This device only")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove Example contract.pdf" }));
    expect(within(screen.getByRole("list", { name: /included with your message/i })).queryByText("Example contract.pdf")).toBeNull();
  });

  it("chats with follow-ups, keeps a list of chats, and starts a new one", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    expect(await screen.findByRole("heading", { name: /ask about your documents/i })).toBeTruthy();

    // A starter prompt fills the box instead of sending.
    fireEvent.click(screen.getByRole("button", { name: /find a policy/i }));
    const box = screen.getByRole("textbox", { name: "Message" }) as HTMLTextAreaElement;
    expect(box.value).toMatch(/what is our policy on/i);

    fireEvent.change(box, { target: { value: "What does the engagement letter say about payment?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const thread = await screen.findByLabelText("Conversation");
    expect(within(thread).getByText(/what does the engagement letter say about payment/i)).toBeTruthy();
    await waitFor(() => expect(within(thread).queryByText(/searching your sources/i)).toBeNull());

    // The chat appears in the list and a follow-up stays in the same chat.
    const list = screen.getByRole("complementary", { name: "Chats" });
    expect(within(list).getByText("Today")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "and when is it due?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(within(screen.getByLabelText("Conversation")).getByText("and when is it due?")).toBeTruthy());
    expect(within(list).getAllByRole("button").filter((b) => b.className.includes("chat-item")).length).toBe(1);

    fireEvent.click(within(list).getByRole("button", { name: /new chat/i }));
    expect(await screen.findByRole("heading", { name: /ask about your documents/i })).toBeTruthy();
  });

  it("sets up a repeat job as its owner and approves the report that comes back", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    // A helper exists from the earlier test in this browser preview.
    expect(await screen.findByRole("heading", { name: "John" })).toBeTruthy();
    fireEvent.click(await screen.findByRole("tab", { name: /repeat jobs/i }));
    fireEvent.click(await screen.findByRole("button", { name: /new repeat job/i }));
    const form = await screen.findByRole("region", { name: /new repeat job/i });
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Weekly performance report" } });
    fireEvent.change(within(form).getByLabelText(/what should the helper do each time/i), { target: { value: "Create the weekly report and compare it with last week." } });
    expect(within(form).getByText(/runs as you/i)).toBeTruthy();
    fireEvent.click(within(form).getByRole("button", { name: /start repeating/i }));

    const repeat = await screen.findByRole("region", { name: "Repeat jobs" });
    expect(await within(repeat).findByText("Weekly performance report")).toBeTruthy();
    expect(within(repeat).getByText(/every friday at 9/i)).toBeTruthy();

    // Running it now puts the report in front of its owner, waiting for a decision.
    fireEvent.click(within(repeat).getByRole("button", { name: /run now/i }));
    const details = await screen.findByRole("region", { name: /task details/i });
    const bar = await within(details).findByRole("group", { name: /review this report/i });
    fireEvent.click(within(bar).getByRole("button", { name: "Approve" }));
    expect(await within(details).findByText(/you approved this report/i)).toBeTruthy();

    // Nothing is left waiting, so the reports tab has no "waiting for you" list.
    fireEvent.click(within(details).getByRole("button", { name: /close task/i }));
    fireEvent.click(screen.getByRole("tab", { name: /my reports/i }));
    expect(await screen.findByRole("region", { name: "All reports" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Waiting for you" })).toBeNull();
  });

  it("asks whether to keep an old report before it is deleted, and the answer is remembered", async () => {
    render(<App />);
    const ask = await screen.findByRole("dialog", { name: /keep your old reports/i });
    expect(within(ask).getByText("Men's retreat expense report")).toBeTruthy();
    expect(within(ask).getByText(/deleted on/i)).toBeTruthy();
    expect(within(ask).getByText(/only you can see them/i)).toBeTruthy();

    // "Ask me later" closes it without deciding, so it comes back next time the app opens.
    fireEvent.click(within(ask).getByRole("button", { name: /ask me later/i }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /keep your old reports/i })).toBeNull());
    cleanup();

    render(<App />);
    const again = await screen.findByRole("dialog", { name: /keep your old reports/i });
    fireEvent.click(within(again).getByRole("button", { name: /^keep it$/i }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /keep your old reports/i })).toBeNull());
    cleanup();

    // Decided: it is not asked about again.
    render(<App />);
    await screen.findByRole("button", { name: /^agents$/i });
    expect(screen.queryByRole("dialog", { name: /keep your old reports/i })).toBeNull();
  });

  it("lets an ordinary employee connect a source only for themselves, and an administrator for everyone", async () => {
    setPreviewConnections([]);
    setPreviewCanShareWithWorkspace(false);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
      fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));
      fireEvent.click(await screen.findByRole("button", { name: /connect dropbox/i }));
      const chooser = await screen.findByRole("region", { name: /who can search these files/i });
      // The workspace choice is there but cannot be taken, and says why.
      expect((within(chooser).getByRole("radio", { name: /entire workspace/i }) as HTMLInputElement).disabled).toBe(true);
      expect(within(chooser).getByText(/only an administrator can connect a source for the whole workspace/i)).toBeTruthy();
      expect(within(chooser).getByText(/nobody else at your firm can find them/i)).toBeTruthy();
      fireEvent.click(within(chooser).getByRole("button", { name: /cancel/i }));
      expect(screen.queryByRole("region", { name: /who can search these files/i })).toBeNull();
    } finally {
      setPreviewCanShareWithWorkspace(true);
    }

    // An administrator can choose the whole workspace, and it is shown as such.
    cleanup();
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^sources/i }));
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));
    fireEvent.click(await screen.findByRole("button", { name: /connect docusign/i }));
    const chooser = await screen.findByRole("region", { name: /who can search these files/i });
    fireEvent.click(within(chooser).getByRole("radio", { name: /entire workspace/i }));
    fireEvent.click(within(chooser).getByRole("button", { name: /continue to docusign/i }));
    const list = await screen.findByRole("region", { name: /connected sources/i }, { timeout: 8000 });
    expect(await within(list).findByText(/entire workspace/i, {}, { timeout: 8000 })).toBeTruthy();
    expect(within(list).getByRole("button", { name: /make private/i })).toBeTruthy();
  }, 20000);

  it("puts the newest draft first and warns when there is another draft to the same people with the same subject", async () => {
    resetPreviewActions();
    addPreviewAction({ id: "act-0", task_id: "task-0", task_title: "Write to the client again", agent_name: "Dana", kind: "email", to: ["client@other.example"], cc: [], subject: "Retreat budget update", body: "A second draft of the same email.", status: "proposed", edited: false, outside: ["client@other.example"], created_epoch: Math.floor(Date.now() / 1000) });
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
      fireEvent.click(await screen.findByRole("tab", { name: /my reports/i }));
      const section = await screen.findByRole("region", { name: "Prepared for you" });
      const emails = within(section).getAllByRole("article", { name: /email: retreat budget update/i });
      expect(emails).toHaveLength(2);
      // The newest is on top, and each says when it was prepared.
      expect(within(emails[0]).getByText(/a second draft of the same email/i)).toBeTruthy();
      expect(within(emails[0]).getByText(/prepared \d/i)).toBeTruthy();
      // Both say there is another like it, so the person looks before choosing one.
      for (const card of emails) expect(within(card).getByText(/another draft to the same people with the same subject/i)).toBeTruthy();
      // The meeting has no twin, so it has no such warning.
      expect(within(within(section).getByRole("article", { name: /meeting: budget review/i })).queryByText(/another draft/i)).toBeNull();
    } finally {
      resetPreviewActions();
    }
  });

  it("sends a prepared email from the person's own Outlook only after they have read it and confirmed outside recipients", async () => {
    resetPreviewActions();
    setPreviewEmailSending(true);
    setPreviewConnections([{ id: "mail-1", kind: "outlook_mail", status: "active", last_sync_epoch: null, last_error: null, documents: 0, scope: "private", yours: true, label: "you@outlook.example" }]);
    const opened = vi.spyOn(window, "open").mockReturnValue(null);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
      fireEvent.click(await screen.findByRole("tab", { name: /my reports/i }));
      const section = await screen.findByRole("region", { name: "Prepared for you" });
      const email = within(section).getByRole("article", { name: /email: retreat budget update/i });
      // The draft is there to read, and the Send button says where it sends from.
      expect(within(email).getByText(/spent \$5,670\.77/)).toBeTruthy();
      const group = within(email).getByRole("group", { name: /send this email/i });
      expect(within(group).getByText("you@outlook.example")).toBeTruthy();
      // It goes to someone outside the organisation, so Send waits for the person to say they checked.
      const send = within(group).getByRole("button", { name: "Send" }) as HTMLButtonElement;
      expect(send.disabled).toBe(true);
      fireEvent.click(within(group).getByRole("checkbox", { name: /checked the people outside/i }));
      expect(send.disabled).toBe(false);
      fireEvent.click(send);
      expect(await within(section).findByText(/sent from you@outlook\.example/i)).toBeTruthy();
      await waitFor(() => expect(within(section).queryByRole("article", { name: /email: retreat budget update/i })).toBeNull());
      // Sending from the app opened nothing in a browser.
      expect(opened).not.toHaveBeenCalled();
    } finally {
      opened.mockRestore();
      setPreviewEmailSending(false);
      setPreviewConnections([]);
      resetPreviewActions();
    }
  });

  it("says why an email cannot be sent from the app, and offers to connect Outlook, when it is switched off or not connected", async () => {
    resetPreviewActions();
    setPreviewEmailSending(false);
    setPreviewConnections([]);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    fireEvent.click(await screen.findByRole("tab", { name: /my reports/i }));
    let email = within(await screen.findByRole("region", { name: "Prepared for you" })).getByRole("article", { name: /email: retreat budget update/i });
    expect(within(email).getByText(/switched off for your firm/i)).toBeTruthy();
    expect(within(email).queryByRole("button", { name: "Send" })).toBeNull();
    // The open-in-my-own-email buttons are still there.
    expect(within(email).getByRole("button", { name: /open in gmail/i })).toBeTruthy();
    cleanup();

    // Allowed, but this person has not connected Outlook yet.
    setPreviewEmailSending(true);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    fireEvent.click(await screen.findByRole("tab", { name: /my reports/i }));
    email = within(await screen.findByRole("region", { name: "Prepared for you" })).getByRole("article", { name: /email: retreat budget update/i });
    expect(within(email).getByRole("button", { name: /connect your outlook/i })).toBeTruthy();
    expect(within(email).queryByRole("button", { name: "Send" })).toBeNull();
    setPreviewEmailSending(false);
  });

  it("shows what an agent prepared, flags outside recipients, and opens the person's own mail or calendar only once they choose", async () => {
    resetPreviewActions();
    const opened = vi.spyOn(window, "open").mockReturnValue(null);
    try {
      render(<App />);
      fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
      // The tab and the banner say that something is waiting.
      fireEvent.click(await screen.findByRole("tab", { name: /my reports/i }));
      const section = await screen.findByRole("region", { name: "Prepared for you" });

      const email = within(section).getByRole("article", { name: /email: retreat budget update/i });
      expect(within(email).getByText(/nothing has been sent/i)).toBeTruthy();
      expect(within(email).getByRole("alert").textContent).toMatch(/outside your organisation: client@other\.example/);
      expect(within(email).getByText(/spent \$5,670\.77/)).toBeTruthy();
      // Nothing opens until a button is pressed.
      expect(opened).not.toHaveBeenCalled();
      fireEvent.click(within(email).getByRole("button", { name: /open in gmail/i }));
      await waitFor(() => expect(opened).toHaveBeenCalledTimes(1));
      expect(String(opened.mock.calls[0][0])).toMatch(/^https:\/\/mail\.google\.com\/mail\/\?view=cm.*to=client%40other\.example/);
      expect(await within(section).findByText(/opened in gmail\. check it and press send yourself/i)).toBeTruthy();
      await waitFor(() => expect(within(section).queryByRole("article", { name: /email: retreat budget update/i })).toBeNull());

      // A meeting can be corrected first, and what opens is the corrected one.
      const meeting = within(section).getByRole("article", { name: /meeting: budget review/i });
      fireEvent.click(within(meeting).getByRole("button", { name: /edit first/i }));
      fireEvent.change(within(meeting).getByLabelText("Title"), { target: { value: "Budget review (moved)" } });
      fireEvent.click(within(meeting).getByRole("button", { name: /open in google calendar/i }));
      await waitFor(() => expect(opened).toHaveBeenCalledTimes(2));
      expect(decodeURIComponent(String(opened.mock.calls[1][0]))).toContain("text=Budget review (moved)");
      expect(String(opened.mock.calls[1][0])).toContain("dates=20261105T140000/20261105T150000");
    } finally {
      opened.mockRestore();
    }
  });

  it("gives a helper a task from the chat by starting with @name, and shows its report in the conversation", async () => {
    await cloudAgentCreate({ name: "Rae", role: "Research", description: "Finds things.", instructions: "Research.", tools: ["search_documents"], model: "standard", effort: "low" });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^chat$/i }));
    // There is no menu for it in the message box: the person starts the message with @name.
    expect(screen.queryByRole("combobox", { name: "Send to" })).toBeNull();
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "@Rae Find the retreat budget figures" } });
    // The box says what will happen, and the quick-answer menu is not shown for a helper.
    expect(await screen.findByText(/rae will work on this in the background/i)).toBeTruthy();
    expect(screen.queryByRole("combobox", { name: "Analysis" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    // The helper's report appears in the thread, with a way to the full report.
    const thread = await screen.findByLabelText("Conversation");
    expect(within(thread).getByText("@Rae Find the retreat budget figures")).toBeTruthy();
    const report = await within(thread).findByRole("region", { name: /rae's report/i });
    expect(within(report).getByText(/sample report/i)).toBeTruthy();
    expect(within(report).getByRole("button", { name: /open the full report/i })).toBeTruthy();

    // Starting a message with @name does the same without touching the menu, in a new chat.
    fireEvent.click(within(screen.getByRole("complementary", { name: "Chats" })).getByRole("button", { name: /new chat/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "@Rae look up the lease dates" } });
    expect(screen.getByText(/rae will work on this in the background/i)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const second = await screen.findByLabelText("Conversation");
    // What was typed is shown as typed, and the helper got the task without the @name.
    expect(within(second).getByText("@Rae look up the lease dates")).toBeTruthy();
    expect(await within(second).findByRole("region", { name: /rae's report/i })).toBeTruthy();

    // The full report opens on the Agents page.
    fireEvent.click(within(second).getByRole("button", { name: /open the full report/i }));
    expect(await screen.findByRole("region", { name: /task details/i })).toBeTruthy();
  });
});

