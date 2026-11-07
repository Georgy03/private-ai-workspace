// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { applyTheme, setThemeChoice, shownTheme, themeChoice } from "./theme";

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("data-theme");
  vi.unstubAllGlobals();
});

const computerPrefers = (dark: boolean) =>
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: dark && query.includes("dark"), addEventListener: () => undefined, removeEventListener: () => undefined }));

describe("theme", () => {
  it("follows the computer until a choice is made, and says so", () => {
    computerPrefers(true);
    expect(themeChoice()).toBe("system");
    expect(shownTheme()).toBe("dark");
    computerPrefers(false);
    expect(shownTheme()).toBe("light");
  });

  it("keeps an explicit choice whatever the computer prefers, and goes back to following it", () => {
    computerPrefers(true);
    setThemeChoice("light");
    expect([themeChoice(), document.documentElement.dataset.theme]).toEqual(["light", "light"]);
    setThemeChoice("dark");
    computerPrefers(false);
    expect([themeChoice(), shownTheme()]).toEqual(["dark", "dark"]);
    setThemeChoice("system");
    expect([themeChoice(), localStorage.getItem("prism-theme"), document.documentElement.dataset.theme]).toEqual(["system", null, "light"]);
  });

  it("ignores anything saved that is not a choice", () => {
    localStorage.setItem("prism-theme", "purple");
    expect(themeChoice()).toBe("system");
    applyTheme();
    expect(["light", "dark"]).toContain(document.documentElement.dataset.theme);
  });
});
