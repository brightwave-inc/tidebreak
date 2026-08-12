import { describe, expect, it } from "vitest";

import { extractHeadings, slugify } from "./markdownHeadings";

describe("extractHeadings", () => {
  it("reads a document's headings with their depth, ignoring what is not one", () => {
    const headings = extractHeadings(
      [
        "# Quarterly report",
        "",
        "Not a heading. #hashtag either.",
        "",
        "## Revenue by **segment**",
        "",
        "```",
        "# a comment in a fenced block",
        "```",
        "",
        "####### Seven hashes is not a heading",
        "",
        "###### Footnotes",
      ].join("\n"),
    );

    expect(headings).toEqual([
      { level: 1, text: "Quarterly report", id: "quarterly-report" },
      // Inline emphasis is stripped, so the id matches the text as rendered.
      { level: 2, text: "Revenue by segment", id: "revenue-by-segment" },
      { level: 6, text: "Footnotes", id: "footnotes" },
    ]);
  });

  // A `#` line inside a fence is a comment, not a heading, and listing it puts
  // an entry in the outline that scrolls nowhere — the renderer never made a
  // heading with that id.
  it("does not read comments inside fenced code as headings", () => {
    expect(
      extractHeadings("# Setup\n\n~~~sh\n# install the thing\n~~~\n\n## Done"),
    ).toEqual([
      { level: 1, text: "Setup", id: "setup" },
      { level: 2, text: "Done", id: "done" },
    ]);

    // An unclosed fence runs to the end, as the renderer also treats it.
    expect(extractHeadings("# Setup\n\n```\n# still code\n")).toEqual([
      { level: 1, text: "Setup", id: "setup" },
    ]);
  });

  // The outline slugs the raw source and the renderer slugs the rendered node.
  // Nothing is passed between them, so agreement rests entirely on slugify —
  // a heading whose two ids differ is one the outline silently cannot reach.
  it("reduces punctuation and spacing the same way from either side", () => {
    expect(slugify("Q3 2026 — Revenue & Margin (draft)")).toBe(
      "q3-2026-revenue-margin-draft",
    );
    expect(slugify("  Leading and trailing  ")).toBe("leading-and-trailing");
    expect(slugify("Hyphens---collapse")).toBe("hyphens-collapse");
  });
});
