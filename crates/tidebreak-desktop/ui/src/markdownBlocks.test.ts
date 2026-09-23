import { describe, expect, it } from "vitest";
import {
  openFenceCode,
  splitMarkdownBlocks,
  splitMarkdownSource,
  type MarkdownBlockSplit,
} from "./markdownBlocks";

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

/**
 * Documents built to catch an incremental split that disagrees with a full
 * parse: constructs whose meaning changes when a later line arrives.
 */
const DOCUMENTS = [
  [
    "Heading",
    "=======",
    "",
    "Intro with `inline` code and a [link](https://example.com).",
    "Title that becomes setext",
    "---",
    "",
    "- tight one",
    "- tight two",
    "",
    "  continued, which loosens the list",
    "",
    "1. ordered",
    "   1. nested",
    "",
    "> quoted",
    "lazily continued",
    "",
    "| a | b |",
    "| - | - |",
    "| 1 | 2 |",
    "trailing row",
    "",
    "    indented code",
    "",
    "***",
    "",
    "End.",
  ].join("\n"),
  [
    "Before the fence.",
    "",
    "```ts",
    "const a = 1;",
    "",
    "``` not a close, it has an info string",
    "const b = `template`;",
    "```",
    "",
    "  ~~~~ python",
    "  indented = True",
    "  ~~~",
    "  still = 'open'",
    "  ~~~~~",
    "",
    "````md",
    "```js",
    "nested();",
    "```",
    "````",
    "",
    "```",
    "left open at the end",
  ].join("\n"),
];

describe("splitMarkdownSource", () => {
  it.each(
    DOCUMENTS.flatMap((_document, index) =>
      [1, 3, 11].map((step) => [index, step] as const),
    ),
  )(
    "matches a full parse at every prefix of document %i, %i characters at a time",
    (index, step) => {
      const document = DOCUMENTS[index]!;
      let previous: MarkdownBlockSplit | null = null;
      for (let end = step; ; end = Math.min(end + step, document.length)) {
        const prefix = document.slice(0, end);
        const incremental = splitMarkdownSource(prefix, previous);
        const full = splitMarkdownSource(prefix);
        expect(incremental.blocks, prefix).toEqual(full.blocks);
        expect(incremental.openFence, prefix).toEqual(full.openFence);
        expect(incremental.blocks.join("")).toBe(prefix);
        previous = incremental;
        if (end === document.length) break;
      }
    },
  );

  it("extends an open fence without parsing the lines appended to it", () => {
    const opened = splitMarkdownSource("Intro.\n\n```ts\nconst a = 1;\n");
    expect(opened.openFence).toEqual({ marker: "```", language: "ts" });
    const extended = splitMarkdownSource(
      "Intro.\n\n```ts\nconst a = 1;\nconst b = 2;\n",
      opened,
    );
    expect(extended.blocks[0]).toBe(opened.blocks[0]);
    expect(extended.openFence).toBe(opened.openFence);
    expect(openFenceCode(extended.blocks.at(-1)!)).toBe(
      "const a = 1;\nconst b = 2;",
    );
  });

  it("notices a closing fence that arrives a few characters at a time", () => {
    let split = splitMarkdownSource("```\ncode\n`");
    expect(split.openFence).not.toBeNull();
    split = splitMarkdownSource("```\ncode\n``", split);
    expect(split.openFence).not.toBeNull();
    split = splitMarkdownSource("```\ncode\n```", split);
    expect(split.openFence).toBeNull();
  });

  it("starts over when the text is not an extension of the last split", () => {
    const first = splitMarkdownSource("One.\n\nTwo.");
    const replaced = splitMarkdownSource("Other.\n\nTwo.", first);
    expect(replaced.blocks).toEqual(["Other.\n\n", "Two."]);
  });

  it("does not take inline code on its own line for a fence", () => {
    expect(splitMarkdownSource("```a``` b\nmore").openFence).toBeNull();
  });
});
