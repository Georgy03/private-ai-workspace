import type { ReactNode } from "react";

/** Text for something the app is working on: a light band keeps sweeping across it, left to right. */
export function Shimmer({ children }: { children: ReactNode }) {
  return <span className="shimmer">{children}</span>;
}
