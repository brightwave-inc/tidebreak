import { describe, expect, it } from "vitest";
import { splitMarkdownBlocks } from "./markdownBlocks";

describe("splitMarkdownBlocks", () => {
  it("keeps a fenced block whole even when it contains blank lines", () => {
    const source = "before\n\n```ts\nline one\n\nline two\n```\n\nafter";
    const blocks = splitMarkdownBlocks(source);

    const fence = blocks.find((block) => block.includes("```"));
    expect(fence).toBeDefined();
    // The blank line inside the fence must not split it into two blocks.
    expect(fence).toContain("line one");
    expect(fence).toContain("line two");
    // Prose on either side is its own block, so the trailing one can re-parse
    // alone while streaming.
    expect(blocks.some((block) => block.startsWith("before"))).toBe(true);
    expect(blocks.some((block) => block.startsWith("after"))).toBe(true);
  });

  it("reproduces the input when the blocks are concatenated", () => {
    const source =
      "# Title\n\nA paragraph.\n\n- one\n- two\n\n```\ncode\n```\n\nEnd.";
    expect(splitMarkdownBlocks(source).join("")).toBe(source);
  });

  it("returns no blocks for empty input", () => {
    expect(splitMarkdownBlocks("")).toEqual([]);
  });
});
