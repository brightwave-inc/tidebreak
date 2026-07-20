import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MessageMarkdown, safeMarkdownUrl } from "./MessageMarkdown";

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
});
