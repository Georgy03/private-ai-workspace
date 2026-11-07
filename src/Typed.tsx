import { useEffect, useRef, useState } from "react";
import { Markdown } from "./Markdown";
import { partlyWritten, typing } from "./typing";

/**
 * An answer's paragraphs, written out smoothly while `animate` is on, then left as they are. It is
 * paced by the clock, a few letters on every frame, so it does not stutter however long the answer
 * is. `onDone` is called once it has all been shown (or at once when it is not animated), and
 * `onGrow` now and then so the page can keep the end in view.
 */
export function TypedClaims({ texts, animate, onDone, onGrow }: {
  texts: string[];
  animate: boolean;
  onDone: () => void;
  onGrow: () => void;
}) {
  const whole = texts.join("\n\n");
  const total = whole.length;
  const quick = typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const typed = animate && typing.enabled && !quick && total > 0;
  const [shown, setShown] = useState(typed ? 0 : total);
  const finished = useRef(false);
  const done = useRef(onDone);
  const grow = useRef(onGrow);
  useEffect(() => {
    done.current = onDone;
    grow.current = onGrow;
  });

  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      // Leaving the page part way through: the answer is shown whole when it is next opened. Waiting a
      // moment tells that apart from the page remounting this straight away (as it does while developing).
      window.setTimeout(() => {
        if (!mounted.current && !finished.current) { finished.current = true; done.current(); }
      }, 0);
    };
  }, []);

  useEffect(() => {
    if (!typed) {
      if (!finished.current) { finished.current = true; done.current(); }
      return;
    }
    const duration = Math.min(typing.maxMs, Math.max(typing.minMs, (total / typing.charsPerSecond) * 1000));
    const begun = performance.now();
    let frame = 0;
    let last = 0;
    let growAt = 0;
    const step = (now: number) => {
      const at = Math.min(total, Math.floor(((now - begun) / duration) * total));
      if (at !== last) {
        last = at;
        setShown(at);
      }
      // The page follows the end a few times a second, not on every frame.
      if (now - growAt > 120) { growAt = now; grow.current(); }
      if (at >= total) {
        grow.current();
        if (!finished.current) { finished.current = true; done.current(); }
        return;
      }
      frame = window.requestAnimationFrame(step);
    };
    frame = window.requestAnimationFrame(step);
    return () => window.cancelAnimationFrame(frame);
  }, [typed, total, whole]);

  // Each paragraph is shown in full, shown in part, or not yet shown.
  const starts = texts.map((_, index) => texts.slice(0, index).reduce((sum, earlier) => sum + earlier.length + 2, 0));
  return (
    <div className="claims">
      {texts.map((text, index) => {
        const visible = Math.max(0, Math.min(text.length, shown - starts[index]));
        if (visible <= 0 && typed) return null;
        return <article className="answer-copy" key={index}><Markdown text={typed && visible < text.length ? partlyWritten(text.slice(0, visible)) : text} /></article>;
      })}
    </div>
  );
}
