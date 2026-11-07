/**
 * How an answer is written out on screen. The answer is checked against its sources before anything
 * is shown, so it arrives whole; it is then typed out, paced by the clock, the way a chat reads.
 */
export const typing = {
  /** Off in the tests, which would otherwise wait for every answer to be written out. */
  enabled: !(typeof import.meta !== "undefined" && import.meta.env?.MODE === "test"),
  /** About how fast it goes, in characters a second. */
  charsPerSecond: 320,
  /** The slowest and the quickest a whole answer is written out in, however long it is. */
  minMs: 700,
  maxMs: 4500,
};

/** For the tests that look at the typing itself. */
export function setTyping(on: boolean): void {
  typing.enabled = on;
}

/**
 * Text cut off part way, tidied so that it reads steadily: a word being made bold shows as bold
 * from its first letter, instead of showing stray asterisks until the closing ones arrive.
 */
export function partlyWritten(text: string): string {
  let tidy = text.replace(/[*_`]+$/, "");
  const bold = (tidy.match(/\*\*/g) ?? []).length;
  if (bold % 2 === 1) tidy += "**";
  const code = (tidy.match(/`/g) ?? []).length;
  if (code % 2 === 1) tidy += "`";
  return tidy;
}
