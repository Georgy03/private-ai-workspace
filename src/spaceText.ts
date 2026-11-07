import type { Turn } from "./Chat";

/** "3.4 MB", "820 KB", "0 KB": room on this computer, in words a person reads. */
export function formatBytes(bytes: number): string {
  if (bytes < 1_000) return "0 KB";
  if (bytes < 1_000_000) return `${Math.max(1, Math.round(bytes / 1_000))} KB`;
  if (bytes < 1_000_000_000) return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
  return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
}

/** "Just now", "3 days ago", "Mar 4": when a workspace last changed. */
export function ago(epochSeconds: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.floor(now / 1000) - epochSeconds);
  if (seconds < 90) return "Just now";
  if (seconds < 3_600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86_400) return `${Math.round(seconds / 3_600)} hr ago`;
  if (seconds < 7 * 86_400) return `${Math.round(seconds / 86_400)} day${Math.round(seconds / 86_400) === 1 ? "" : "s"} ago`;
  return new Date(epochSeconds * 1000).toLocaleDateString([], { month: "short", day: "numeric", year: new Date(epochSeconds * 1000).getFullYear() === new Date(now).getFullYear() ? undefined : "numeric" });
}

/** The parts of a chat that are kept: finished questions only, never a half-written answer, never the typing flag. */
export function savedTurns(turns: Turn[]): Omit<Turn, "fresh">[] {
  return turns.filter((turn) => turn.answer || turn.error || turn.email || turn.task).map((turn) => {
    const { fresh, ...kept } = turn;
    void fresh;
    return kept;
  });
}

export const serializeChat = (turns: Turn[]): string => JSON.stringify({ version: 1, turns: savedTurns(turns) });

/** A saved chat read back. Anything that does not look like a chat gives nothing, never a crash. */
export function parseChat(body: string): Turn[] {
  try {
    const value = JSON.parse(body) as { turns?: unknown };
    if (!Array.isArray(value.turns)) return [];
    return value.turns
      .filter((turn): turn is Turn => typeof turn === "object" && turn !== null && typeof (turn as Turn).id === "number" && typeof (turn as Turn).question === "string")
      .map((turn) => ({ ...turn, fresh: false }));
  } catch {
    return [];
  }
}
