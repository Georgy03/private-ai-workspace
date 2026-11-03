// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import App from "./App";
import { setPreviewConnections, setPreviewServerDown } from "./api";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("glass workspace controls", () => {
  it("allows a suggested question to be searched and cited", async () => {
    render(<App />);
    const suggestion = await screen.findByRole("button", { name: /when is payment due/i });
    fireEvent.click(suggestion);
    const input = screen.getByRole("textbox", { name: /ask your workspace/i });
    expect((input as HTMLInputElement).value).toBe("When is payment due?");
    fireEvent.click(screen.getByRole("button", { name: /search workspace/i }));
    expect(await screen.findByText(/answer from your workspace/i)).toBeTruthy();
    expect(screen.getAllByText(/thirty days/i).length).toBeGreaterThan(0);
  });

  it("keeps navigation and settings usable", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /sources/i }));
    expect(screen.getByText("Turnip Engagement.txt")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    expect(screen.getByText("Workspace settings")).toBeTruthy();
  });

  it("labels passages from the firm cloud index and opens them without a file", async () => {
    render(<App />);
    const input = await screen.findByRole("textbox", { name: /ask your workspace/i });
    fireEvent.change(input, { target: { value: "engagement checklist letter" } });
    fireEvent.click(screen.getByRole("button", { name: /search workspace/i }));
    expect(await screen.findByText(/answer from your workspace/i)).toBeTruthy();
    expect(screen.getAllByText("Cloud").length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: /^\d+\s*engagement checklist\.docx/i }));
    expect(await screen.findByText(/passage from the firm cloud index/i)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /privacy receipt/i }));
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

    const input = await screen.findByRole("textbox", { name: /ask your workspace/i });
    fireEvent.change(input, { target: { value: "engagement checklist letter" } });
    fireEvent.click(screen.getByRole("button", { name: /search workspace/i }));
    expect(await screen.findByText(/answer from your workspace/i)).toBeTruthy();
    expect(screen.queryByText(/engagement checklist\.docx/i)).toBeNull();
    expect(screen.queryByText("Cloud")).toBeNull();

    // Leave it as it was for the other tests.
    fireEvent.click(screen.getByRole("button", { name: /settings/i }));
    fireEvent.click(await screen.findByRole("checkbox", { name: /search the firm cloud index/i }));
  });

  it("lists supported connectors under Sources and adds one once it is approved", async () => {
    setPreviewConnections([]);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /sources/i }));
    // Files stay the first tab.
    expect(screen.getByText("Turnip Engagement.txt")).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", { name: /connectors/i }));

    expect(await screen.findByText("Google Drive")).toBeTruthy();
    expect(screen.getByText("Microsoft 365")).toBeTruthy();
    // Connectors on the roadmap are shown but cannot be used yet.
    const planned = screen.getByRole("button", { name: /dropbox is coming soon/i }) as HTMLButtonElement;
    expect(planned.disabled).toBe(true);
    expect(screen.getByText("Nothing connected yet.")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /connect google drive/i }));
    // The sign-in opens in the browser; once approved the source shows up in the list.
    const list = await screen.findByRole("region", { name: /connected sources/i }, { timeout: 8000 });
    expect(await within(list).findByText(/12 files/i, {}, { timeout: 8000 })).toBeTruthy();
    expect(within(list).getByText("Active")).toBeTruthy();
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

  it("lets advanced analysis be turned on, and says it uses more of the allowance", async () => {
    render(<App />);
    const toggle = (await screen.findByRole("checkbox", { name: /advanced analysis/i })) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(screen.queryByText(/about 20 times as much of your monthly ai allowance/i)).toBeNull();

    fireEvent.click(toggle);
    expect(toggle.checked).toBe(true);
    expect(screen.getByText(/about 20 times as much of your monthly ai allowance/i)).toBeTruthy();

    fireEvent.click(toggle);
    expect(screen.queryByText(/about 20 times as much of your monthly ai allowance/i)).toBeNull();
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
    const input = await screen.findByRole("textbox", { name: /ask your workspace/i });
    fireEvent.change(input, { target: { value: "spaceship launch code" } });
    fireEvent.click(screen.getByRole("button", { name: /search workspace/i }));
    const retry = await screen.findByRole("button", { name: /try again with advanced analysis/i });
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
    fireEvent.click(await screen.findByRole("button", { name: /sources/i }));
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
    fireEvent.click(await screen.findByRole("button", { name: /sources/i }));
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
    fireEvent.click(await screen.findByRole("button", { name: /sources/i }));
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
    fireEvent.change(await screen.findByRole("textbox", { name: /ask your workspace/i }), { target: { value: "engagement checklist" } });
    fireEvent.click(screen.getByRole("button", { name: /search workspace/i }));
    const link = await screen.findAllByRole("button", { name: /open engagement checklist\.docx in google drive/i });
    fireEvent.click(link[0]);
    expect(open).toHaveBeenCalledWith("https://drive.google.com/open?id=sample-checklist", "_blank", "noopener,noreferrer");
  });

  it("lets an administrator create the research agent, give it a task, and read the report", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /^agents$/i }));
    // Planned agents are shown as planned, not as working ones.
    expect(await screen.findByLabelText(/alex \(planned\)/i)).toBeTruthy();
    fireEvent.click(await screen.findByRole("button", { name: /create john/i }));
    const editor = await screen.findByRole("region", { name: /new agent/i });
    expect(within(editor).getByText(/it can only read, never change anything/i)).toBeTruthy();
    fireEvent.click(within(editor).getByRole("button", { name: /create agent/i }));

    const john = (await screen.findByText("John")).closest("article") as HTMLElement;
    fireEvent.click(within(john).getByRole("button", { name: /give a task/i }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Task" }), { target: { value: "What is the retreat budget?" } });
    fireEvent.click(screen.getByRole("button", { name: /start task/i }));

    const details = await screen.findByRole("region", { name: /task details/i });
    expect(await within(details).findByText(/sample report/i)).toBeTruthy();
    expect(within(details).getByText(/searched documents/i)).toBeTruthy();
  });
});

