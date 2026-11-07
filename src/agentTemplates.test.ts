import { describe, expect, it } from "vitest";
import { TEMPLATES } from "./agentTemplates";

const TOOLS = ["search_documents", "read_document", "lookup_knowledge", "prepare_email", "prepare_meeting"];

describe("helper templates", () => {
  it("all fit the limits the server enforces, so none fails when chosen", () => {
    for (const { id, draft, title, blurb } of TEMPLATES) {
      expect(draft.name.trim().length, id).toBeGreaterThan(0);
      expect(draft.name.length, id).toBeLessThanOrEqual(60);
      expect(draft.role.length, id).toBeLessThanOrEqual(80);
      expect(draft.description.length, id).toBeLessThanOrEqual(400);
      expect(draft.instructions.length, id).toBeGreaterThan(0);
      expect(draft.instructions.length, id).toBeLessThanOrEqual(8000);
      expect(draft.tools.length, id).toBeGreaterThan(0);
      expect(draft.tools.every((tool) => TOOLS.includes(tool)), id).toBe(true);
      expect(["standard", "advanced"]).toContain(draft.model);
      expect(["low", "medium", "high"]).toContain(draft.effort);
      expect(title.length + blurb.length).toBeGreaterThan(10);
    }
  });

  it("have their own ids and names, and never promise to change anything", () => {
    expect(new Set(TEMPLATES.map((t) => t.id)).size).toBe(TEMPLATES.length);
    expect(new Set(TEMPLATES.map((t) => t.draft.name)).size).toBe(TEMPLATES.length);
    for (const { draft } of TEMPLATES) expect(draft.tools.some((tool) => /^send|^book|^write/.test(tool))).toBe(false);
  });
});
