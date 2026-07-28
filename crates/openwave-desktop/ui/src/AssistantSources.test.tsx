import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AssistantSources, type AssistantSource } from "./AssistantSources";

function source(overrides: Partial<AssistantSource> = {}): AssistantSource {
  return {
    id: "private-source-id",
    ordinal: 1,
    documentId: "document-1",
    span: { start: 0, end: 26 },
    excerpt: "A short supporting excerpt.",
    heading: "Project notes",
    pages: [2, 3],
    ...overrides,
  };
}

describe("AssistantSources", () => {
  it("renders nothing for an empty source set", () => {
    expect(renderToStaticMarkup(<AssistantSources sources={[]} />)).toBe("");
  });

  it("renders compact source pills and a native accessible disclosure", () => {
    const markup = renderToStaticMarkup(
      <AssistantSources
        sources={[
          source(),
          source({ id: "second-private-id", ordinal: 2, pages: [7] }),
        ]}
      />,
    );

    expect(markup).toContain("<details");
    expect(markup).toContain("<summary");
    expect(markup).toContain("2 sources");
    expect(markup).toContain('class="assistant-source-pill"');
    expect(markup).toContain('aria-label="Source 1"');
    expect(markup).toContain("Pages 2, 3");
    expect(markup).toContain("Page 7");
  });

  it("orders sources by ordinal without mutating equal-ordinal input order", () => {
    const sources = [
      source({ id: "equal-first", ordinal: 2, heading: "First equal" }),
      source({ id: "later", ordinal: 4, heading: "Later" }),
      source({ id: "earlier", ordinal: 1, heading: "Earlier" }),
      source({ id: "equal-second", ordinal: 2, heading: "Second equal" }),
    ];
    const markup = renderToStaticMarkup(
      <AssistantSources sources={sources} />,
    );

    expect(markup.indexOf("Earlier")).toBeLessThan(
      markup.indexOf("First equal"),
    );
    expect(markup.indexOf("First equal")).toBeLessThan(
      markup.indexOf("Second equal"),
    );
    expect(markup.indexOf("Second equal")).toBeLessThan(
      markup.indexOf("Later"),
    );
    expect(sources.map(({ ordinal }) => ordinal)).toEqual([2, 4, 1, 2]);
  });

  it("renders the full server-bounded set of twenty sources", () => {
    const sources = Array.from({ length: 20 }, (_, index) =>
      source({
        id: `private-${index + 1}`,
        ordinal: index + 1,
        heading: `Source heading ${index + 1}`,
      }),
    );
    const markup = renderToStaticMarkup(
      <AssistantSources sources={sources} />,
    );

    expect(markup).toContain("20 sources");
    expect(markup).toContain("Source heading 20");
    expect(markup.match(/class="assistant-source"/g)).toHaveLength(20);
  });

  it("renders source copy as bounded plain text and never exposes identity", () => {
    const longHeading = `Heading <script>unsafe()</script> ${"h".repeat(180)}`;
    const longExcerpt = `<img src=x onerror=unsafe()>${"e".repeat(650)}`;
    const markup = renderToStaticMarkup(
      <AssistantSources
        sources={[
          source({
            id: "/Users/name/Documents/private.pdf#retrieval-token",
            heading: longHeading,
            excerpt: longExcerpt,
            pages: [9, 8, 7, 6, 5, 4, 3, 2, 1, 1, -4, 2.5],
          }),
        ]}
      />,
    );

    expect(markup).toContain("&lt;script&gt;unsafe()&lt;/script&gt;");
    expect(markup).toContain("&lt;img src=x onerror=unsafe()&gt;");
    expect(markup).not.toContain("<script>");
    expect(markup).not.toContain("<img");
    expect(markup).not.toContain("/Users/name");
    expect(markup).not.toContain("retrieval-token");
    expect(markup).toContain("Pages 1, 2, 3, 4, 5, 6, 7, 8");
    expect(markup).not.toContain("Pages 1, 2, 3, 4, 5, 6, 7, 8, 9");
    expect(markup.match(/…/g)).toHaveLength(2);
    expect(markup).not.toContain("href=");
  });

  it("omits empty headings and invalid page references", () => {
    const markup = renderToStaticMarkup(
      <AssistantSources
        sources={[source({ heading: "  ", pages: [0, -1, 1.5, Number.NaN] })]}
      />,
    );

    expect(markup).not.toContain("<strong>");
    expect(markup).not.toContain("assistant-source-pages");
  });
});
