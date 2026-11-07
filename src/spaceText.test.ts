import { describe, expect, it } from "vitest";
import { ago, formatBytes, parseChat, savedTurns, serializeChat } from "./spaceText";
import type { Turn } from "./Chat";

const turn = (extra: Partial<Turn>): Turn => ({ id: 1, question: "Q", advanced: false, ...extra });

describe("formatBytes", () => {
  it("says room in words", () => {
    expect([0, 500, 1_500, 820_000, 3_400_000, 48_000_000, 2_300_000_000].map(formatBytes)).toEqual(["0 KB", "0 KB", "2 KB", "820 KB", "3.4 MB", "48 MB", "2.3 GB"]);
  });
});

describe("ago", () => {
  const now = Date.UTC(2026, 10, 6, 12, 0, 0);
  const at = (secondsAgo: number) => Math.floor(now / 1000) - secondsAgo;
  it("says how long ago in plain words", () => {
    expect([10, 600, 7_200, 86_400, 3 * 86_400].map((seconds) => ago(at(seconds), now))).toEqual(["Just now", "10 min ago", "2 hr ago", "1 day ago", "3 days ago"]);
  });
});

describe("saved chats", () => {
  it("keep only finished questions, and never the typing flag", () => {
    const kept = savedTurns([turn({ id: 1, error: "x", fresh: true }), turn({ id: 2 })]);
    expect(kept.map((t) => t.id)).toEqual([1]);
    expect("fresh" in kept[0]).toBe(false);
  });

  it("read back as they were written, finished and not typing", () => {
    const back = parseChat(serializeChat([turn({ id: 7, question: "When?", error: "none", fresh: true })]));
    expect(back).toEqual([{ id: 7, question: "When?", advanced: false, error: "none", fresh: false }]);
  });

  it("give nothing, never a crash, when what was saved is not a chat", () => {
    for (const bad of ["", "not json", "null", "{}", "{\"turns\":5}", "[1,2]", "{\"turns\":[{\"id\":\"a\"},null,3]}"]) expect(parseChat(bad)).toEqual([]);
  });
});
