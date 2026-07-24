import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  MessageMarkdown,
  preserveLineBreaks,
  rawCodeText,
  safeMarkdownUrl,
} from "./MessageMarkdown";

describe("preserveLineBreaks", () => {
  it("turns single newlines into hard breaks but leaves paragraph breaks", () => {
    expect(preserveLineBreaks("one\ntwo")).toBe("one  \ntwo");
    expect(preserveLineBreaks("para one\n\npara two")).toBe(
      "para one\n\npara two",
    );
  });
});

describe("safeMarkdownUrl", () => {
  it("permits only secure external links", () => {
    expect(safeMarkdownUrl("https://openwave.dev/docs")).toBe(
      "https://openwave.dev/docs",
    );
    expect(safeMarkdownUrl("http://openwave.dev")).toBeUndefined();
    expect(safeMarkdownUrl("javascript:alert(1)")).toBeUndefined();
    expect(safeMarkdownUrl("data:text/html,unsafe")).toBeUndefined();
    expect(safeMarkdownUrl("file:///Users/example/private.txt")).toBeUndefined();
  });
});

describe("MessageMarkdown", () => {
  it("renders useful Markdown without emitting embeds or unsafe links", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>
        {"## Heading\n\n- `inline` item\n\n> A quote\n\n[Docs](https://openwave.dev/docs) [Unsafe](javascript:alert(1))\n\n![remote](https://example.com/image.png)\n\n<iframe src=\"https://example.com\"></iframe>"}
      </MessageMarkdown>,
    );

    expect(markup).toContain("<h2>Heading</h2>");
    expect(markup).toContain("<code>inline</code>");
    expect(markup).toContain("<blockquote>");
    expect(markup).toContain('href="https://openwave.dev/docs"');
    expect(markup).toContain("Image omitted: remote");
    expect(markup).not.toContain("<img");
    expect(markup).not.toContain("<iframe");
    expect(markup).not.toContain("javascript:");
  });

  it("adds a table-level copy action without exposing Markdown attributes", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>
        {"| Name | Value |\n| --- | --- |\n| Alpha | **1** |"}
      </MessageMarkdown>,
    );

    expect(markup).toContain("<table>");
    expect(markup).toContain('aria-label="Copy table contents"');
    expect(markup).toContain("Alpha");
    expect(markup).toContain("<strong>1</strong>");
  });

  it("renders single newlines as line breaks without splitting paragraphs", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{"first line\nsecond line"}</MessageMarkdown>,
    );

    expect(markup).toContain("<br/>");
    // One paragraph with a break, not two separate paragraphs.
    expect(markup.match(/<p>/g)).toHaveLength(1);
  });

  it("keeps blank-line separated text as distinct paragraphs", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{"first para\n\nsecond para"}</MessageMarkdown>,
    );

    expect(markup.match(/<p>/g)).toHaveLength(2);
  });
});

describe("code blocks", () => {
  const FENCED = "```ts\nconst x: number = 1;\n```";

  it("highlights fence-tagged code and passes token classes through", () => {
    const markup = renderToStaticMarkup(<MessageMarkdown>{FENCED}</MessageMarkdown>);
    expect(markup).toContain("hljs-keyword");
    expect(markup).toContain('class="code-block"');
  });

  it("leaves unlabeled fences unhighlighted rather than guessing", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{"```\nplain block\n```"}</MessageMarkdown>,
    );
    expect(markup).not.toContain("hljs-keyword");
    expect(markup).toContain("plain block");
  });

  it("adds a copy control to blocks but not inline code", () => {
    const block = renderToStaticMarkup(<MessageMarkdown>{FENCED}</MessageMarkdown>);
    expect(block).toContain('aria-label="Copy code"');
    const inline = renderToStaticMarkup(
      <MessageMarkdown>{"use `inline()` here"}</MessageMarkdown>,
    );
    expect(inline).not.toContain('aria-label="Copy code"');
  });

  it("recovers the raw source from a highlighted tree", () => {
    expect(
      rawCodeText({
        type: "element",
        children: [
          { type: "text", value: "const " },
          {
            type: "element",
            children: [{ type: "text", value: "x" }],
          },
          { type: "text", value: " = 1;" },
        ],
      }),
    ).toBe("const x = 1;");
  });
});

describe("preserveLineBreaks and code fences", () => {
  it("never injects hard-break spaces into fenced code", () => {
    const input = "before\n```ts\nline one\nline two\n```\nafter\nend";
    const output = preserveLineBreaks(input);
    expect(output).toContain("line one\nline two");
    expect(output).toContain("before  \n");
    expect(output).toContain("after  \nend");
  });

  it("leaves a still-streaming unclosed fence untouched", () => {
    const output = preserveLineBreaks("intro\n```py\npartial\nstream");
    expect(output).toContain("partial\nstream");
    expect(output).toContain("intro  \n");
  });
});
