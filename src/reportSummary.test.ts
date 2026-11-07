import { describe, expect, it } from "vitest";
import { countUnder, FINDINGS, RECOMMENDATIONS, summaryOf } from "./reportSummary";

const REPORT = `# Quarterly Performance Report

## Executive summary

Revenue **decreased 8%** quarter-over-quarter. The report identifies three possible contributing factors, supported by company records.

## Findings

1. Churn rose in the second month.
2. Two large renewals slipped.
3. Pricing changed mid-quarter.

## Recommendations

- Review the pricing change.
- Contact the two accounts.

## Discrepancies found

| Where | Says | Should be | Difference |
|---|---|---|---|
| Sheet 1 | 100 | 90 | 10 |
| Sheet 2 | 5 | 6 | 1 |
`;

describe("report summary", () => {
  it("takes the summary section, without Markdown marks", () => {
    expect(summaryOf(REPORT)).toBe("Revenue decreased 8% quarter-over-quarter. The report identifies three possible contributing factors, supported by company records.");
  });
  it("falls back to the first paragraph, and cuts a long one at a sentence", () => {
    expect(summaryOf("# Title\n\nFirst paragraph here.\n\nSecond one.")).toBe("First paragraph here.");
    const long = `${"A sentence that goes on. ".repeat(30)}`;
    const text = summaryOf(long, 100);
    expect(text.length).toBeLessThanOrEqual(100);
    expect(text.endsWith(".")).toBe(true);
    expect(summaryOf("")).toBe("");
  });
  it("counts findings and recommendations, whether they are lists or tables", () => {
    expect(countUnder(REPORT, FINDINGS)).toBe(3);
    expect(countUnder(REPORT, RECOMMENDATIONS)).toBe(2);
    expect(countUnder("## Discrepancies found\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n", FINDINGS)).toBe(2);
    expect(countUnder("## Discrepancies found\n\nNo discrepancies found; I checked totals.", FINDINGS)).toBe(0);
    expect(countUnder("# Nothing here", RECOMMENDATIONS)).toBe(0);
  });
});
