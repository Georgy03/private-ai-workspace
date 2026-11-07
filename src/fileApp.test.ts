import { describe, expect, it } from "vitest";
import { appFromLink } from "./fileApp";

describe("appFromLink", () => {
  it("tells Google's editors apart from the link when the name has no extension", () => {
    expect(appFromLink("https://docs.google.com/document/d/1/edit", "Plan")).toEqual({ app: "google_docs", source: "google_drive" });
    expect(appFromLink("https://docs.google.com/spreadsheets/d/1/edit", "Budget").app).toBe("google_sheets");
    expect(appFromLink("https://docs.google.com/presentation/d/1/edit", "Deck").app).toBe("google_slides");
  });
  it("falls back to the file name, and names the service for unknown files", () => {
    expect(appFromLink("https://contoso.sharepoint.com/x", "Letter.docx")).toEqual({ app: "word", source: "microsoft365" });
    expect(appFromLink("https://drive.google.com/file/d/1/view", "Notes")).toEqual({ app: "other", source: "google_drive" });
    expect(appFromLink(null, "Example.pdf")).toEqual({ app: "pdf", source: null });
  });
});
