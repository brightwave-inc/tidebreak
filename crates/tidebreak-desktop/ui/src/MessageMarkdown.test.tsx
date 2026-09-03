import { renderToReadableStream, renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { stripCitationDirectives } from "./citationDirectives";
import {
  decodeUnicodeEscapes,
  joinWrappedMarkdownUrls,
  MessageMarkdown,
  preserveLineBreaks,
  safeMarkdownUrl,
} from "./MessageMarkdown";

async function renderMarkdownHtml(source: string): Promise<string> {
  const stream = await renderToReadableStream(
    <MessageMarkdown>{source}</MessageMarkdown>,
  );
  await stream.allReady;
  return new Response(stream).text();
}

describe("citation directives", () => {
  const id = "0b2b1f2c-9d3e-4a5b-8c7d-6e5f4a3b2c1d";

  it("renders the cited phrasing, with its Markdown, and never the syntax", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>
        {`The reef :cit[is the *largest* in the world]{doc=${id} page=2}, and it grows.`}
      </MessageMarkdown>,
    );

    expect(markup).toContain("The reef ");
    expect(markup).toContain("is the <em>largest</em> in the world");
    expect(markup).toContain(", and it grows.");
    expect(markup).not.toContain(":cit");
    expect(markup).not.toContain("page=2");
  });

  it("keeps directive-shaped prose that never closes into a citation", () => {
    const prose = [
      ":cit[unterminated",
      ":cit[phrase]{ref=0123456789abcdef0123456789abcdef}",
    ].join("\n");
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{prose}</MessageMarkdown>,
    );

    expect(markup).toContain(":cit[unterminated");
    expect(markup).toContain(
      ":cit[phrase]{ref=0123456789abcdef0123456789abcdef}",
    );
  });

  it("strips citations from the text the clipboard is handed", () => {
    expect(
      stripCitationDirectives(
        `The reef :cit[is the largest in the world]{doc=${id} page=2}, and it grows.`,
      ),
    ).toBe("The reef is the largest in the world, and it grows.");
  });
});

describe("safeMarkdownUrl", () => {
  it("permits http and https, and rejects executable or local schemes", () => {
    expect(safeMarkdownUrl("https://tidebreak.io/docs")).toBe(
      "https://tidebreak.io/docs",
    );
    expect(safeMarkdownUrl("http://127.0.0.1:6031/?path=/story/card")).toBe(
      "http://127.0.0.1:6031/?path=/story/card",
    );
    expect(safeMarkdownUrl("javascript:alert(1)")).toBeUndefined();
    expect(safeMarkdownUrl("data:text/html,unsafe")).toBeUndefined();
    expect(
      safeMarkdownUrl("file:///Users/example/private.txt"),
    ).toBeUndefined();
    expect(
      safeMarkdownUrl("https://user:secret@tidebreak.io/docs"),
    ).toBeUndefined();
  });
});

describe("joinWrappedMarkdownUrls", () => {
  it("rejoins a URL the model broke after the query marker", () => {
    expect(
      joinWrappedMarkdownUrls(
        "Storybook is still at http://127.0.0.1:6031/?\npath=/story/code-workspace-card--hover-idle-session if you want another look.",
      ),
    ).toBe(
      "Storybook is still at http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session if you want another look.",
    );
  });

  it("does not join a URL to the next sentence", () => {
    expect(
      joinWrappedMarkdownUrls("See http://127.0.0.1:6031/\nThe hover card."),
    ).toBe("See http://127.0.0.1:6031/\nThe hover card.");
  });

  it("leaves fenced code untouched", () => {
    const fenced = "```\nhttp://127.0.0.1:6031/?\npath=/story/x\n```";
    expect(joinWrappedMarkdownUrls(fenced)).toBe(fenced);
  });
});

describe("MessageMarkdown", () => {
  it("decodes literal Unicode escapes left by a double-serialized payload", () => {
    expect(decodeUnicodeEscapes("Done \\u2022 \\uD83D\\uDE80")).toBe(
      "Done • 🚀",
    );
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{"Done \\u2022 \\uD83D\\uDE80"}</MessageMarkdown>,
    );
    expect(markup).toContain("Done • 🚀");
  });

  it("renders useful Markdown without emitting embeds or unsafe links", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>
        {
          '## Heading\n\n- `inline` item\n\n> A quote\n\n[Docs](https://tidebreak.io/docs) [Unsafe](javascript:alert(1))\n\n![remote](https://example.com/image.png)\n\n<iframe src="https://example.com"></iframe>'
        }
      </MessageMarkdown>,
    );

    expect(markup).toContain("<h2>Heading</h2>");
    expect(markup).toContain("<code>inline</code>");
    expect(markup).toContain("<blockquote>");
    expect(markup).toContain('href="https://tidebreak.io/docs"');
    expect(markup).toContain("Image omitted: remote");
    expect(markup).not.toContain("<img");
    expect(markup).not.toContain("<iframe");
    expect(markup).not.toContain("javascript:");
  });

  it("renders a table through the table primitive, with a copy control", () => {
    const markup = renderToStaticMarkup(
      <MessageMarkdown>
        {"| Name | Value |\n| --- | --- |\n| Alpha | **1** |"}
      </MessageMarkdown>,
    );

    expect(markup).toContain('data-slot="table"');
    expect(markup).toContain('data-slot="table-head"');
    expect(markup).toContain("Alpha");
    expect(markup).toContain("<strong>1</strong>");
    expect(markup).toContain('aria-label="Copy table"');
  });

  it("renders display math through KaTeX rather than as literal source", {
    timeout: 15_000,
  }, async () => {
    const markup = await renderMarkdownHtml("$$E = mc^2$$");

    // KaTeX emits its own markup; the raw delimiters must not survive.
    expect(markup).toContain("katex");
    expect(markup).not.toContain("$$E = mc^2$$");
  });

  it("normalizes bracketed LaTeX delimiters into rendered math", {
    timeout: 15_000,
  }, async () => {
    const markup = await renderMarkdownHtml("\\[a^2 + b^2 = c^2\\]");

    expect(markup).toContain("katex");
    expect(markup).not.toContain("\\[");
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
    const markup = renderToStaticMarkup(
      <MessageMarkdown>{FENCED}</MessageMarkdown>,
    );
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
    const block = renderToStaticMarkup(
      <MessageMarkdown>{FENCED}</MessageMarkdown>,
    );
    expect(block).toContain('aria-label="Copy code"');
    const inline = renderToStaticMarkup(
      <MessageMarkdown>{"use `inline()` here"}</MessageMarkdown>,
    );
    expect(inline).not.toContain('aria-label="Copy code"');
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
