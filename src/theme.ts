/** Light, dark, or whatever the computer is set to. The choice is kept on this computer. */
export type ThemeChoice = "system" | "light" | "dark";

const KEY = "prism-theme";

export function themeChoice(): ThemeChoice {
  try {
    const saved = localStorage.getItem(KEY);
    return saved === "light" || saved === "dark" ? saved : "system";
  } catch {
    return "system";
  }
}

const prefersDark = () => typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;

/** Which look is showing right now. */
export function shownTheme(choice: ThemeChoice = themeChoice()): "light" | "dark" {
  return choice === "system" ? (prefersDark() ? "dark" : "light") : choice;
}

/** Puts the look on the page, and keeps it following the computer while the choice is "system". */
export function applyTheme(choice: ThemeChoice = themeChoice()): void {
  document.documentElement.dataset.theme = shownTheme(choice);
}

export function setThemeChoice(choice: ThemeChoice): void {
  try {
    if (choice === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    // The choice still applies until the app closes.
  }
  applyTheme(choice);
}

/** Called once at start: shows the saved look before anything is drawn, and follows the computer's changes. */
export function startTheme(): void {
  applyTheme();
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    window.matchMedia("(prefers-color-scheme: dark)").addEventListener?.("change", () => {
      if (themeChoice() === "system") applyTheme("system");
    });
  }
}
