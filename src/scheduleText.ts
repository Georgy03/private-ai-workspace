import type { Cadence } from "./types";

const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

const clock = (minutes: number) => new Date(2000, 0, 1, Math.floor(minutes / 60), minutes % 60).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
const ordinal = (n: number) => `${n}${["th", "st", "nd", "rd"][n % 100 > 10 && n % 100 < 14 ? 0 : Math.min(n % 10, 4) % 4] ?? "th"}`;

/** "Every Friday at 9:00 AM", in words. */
export function describeCadence(cadence: Cadence): string {
  if (cadence.kind === "daily") return `Every day at ${clock(cadence.minutes)}`;
  if (cadence.kind === "monthly") return `On the ${ordinal(cadence.day)} of every month at ${clock(cadence.minutes)}`;
  return `Every ${WEEKDAYS[cadence.weekday]} at ${clock(cadence.minutes)}`;
}

