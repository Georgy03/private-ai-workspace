// @vitest-environment jsdom
import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Markdown } from "./Markdown";

describe("Markdown", () => {
  it("shows headings, bold text, lists and tables as formatted text, not symbols", () => {
    const { container } = render(
      <Markdown
        text={[
          "# Expense Report",
          "",
          "**Summary:** spent **$5,670.77**.",
          "",
          "| Category | Actual |",
          "|---|---|",
          "| Food | $2,325.48 |",
          "| **Total** | **$5,670.77** |",
          "",
          "- First point",
          "- Second point",
          "",
          "1. One",
          "2. Two",
        ].join("\n")}
      />,
    );
    expect(screen.getByRole("heading", { name: "Expense Report" })).toBeTruthy();
    expect(screen.getAllByText("$5,670.77")[0].tagName).toBe("STRONG");
    const table = screen.getByRole("table");
    expect(within(table).getAllByRole("columnheader").map((h) => h.textContent)).toEqual(["Category", "Actual"]);
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
    // No leftover Markdown symbols.
    expect(container.textContent).not.toMatch(/\*\*|\|---|^#/);
  });

  it("never turns text into HTML or links", () => {
    const { container } = render(<Markdown text={'<img src=x onerror=alert(1)> [click](https://evil.example) <script>alert(1)</script>'} />);
    expect(container.querySelector("img, script, a")).toBeNull();
    expect(container.textContent).toContain("<img src=x");
  });
});
