import { describe, expect, it } from "vitest";

import { answerQuestion, getWorkspace, searchDocuments } from "./api";

describe("browser preview bridge", () => {
  it("loads the fictional corpus", async () => {
    const workspace = await getWorkspace();
    expect(workspace.documents).toHaveLength(3);
expect(workspace.provider.kind).toBe("bedrock");
  });

  it("answers with retrieved citations", async () => {
    const answer = await answerQuestion("When is payment due?");
    expect(answer.abstained).toBe(false);
    expect(answer.claims[0].text).toContain("thirty days");
    expect(answer.claims[0].citation_ids).toEqual([answer.citations[0].id]);
  });

  it("abstains instead of inventing an answer", async () => {
    expect(await searchDocuments("spaceship launch code")).toEqual([]);
    expect((await answerQuestion("spaceship launch code")).abstained).toBe(true);
  });
});
