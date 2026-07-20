import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  copyMarkdownTable,
  MarkdownTable,
  markdownTablePlainText,
} from "./MarkdownTable";

const table = (
  <>
    <thead>
      <tr>
        <th>Name</th>
        <th>
          <strong>Value</strong>
        </th>
      </tr>
    </thead>
    <tbody>
      <tr data-private-id="row-private-id">
        <td>
          Alpha <code>one</code>
        </td>
        <td>
          <a href="https://example.com/private-target">1</a>
        </td>
      </tr>
    </tbody>
  </>
);

describe("MarkdownTable", () => {
  it("renders a keyboard-accessible action below the scrollable table", () => {
    const markup = renderToStaticMarkup(<MarkdownTable>{table}</MarkdownTable>);

    expect(markup).toContain('class="markdown-table-frame"');
    expect(markup).toContain('class="markdown-table-wrap"');
    expect(markup).toContain('aria-label="Copy table contents"');
    expect(markup.indexOf("<table>")).toBeLessThan(
      markup.indexOf("Copy table contents"),
    );
  });

  it("serializes only visible cell text as spreadsheet-friendly TSV", () => {
    const plainText = markdownTablePlainText(table);

    expect(plainText).toBe("Name\tValue\nAlpha one\t1\n");
    expect(plainText).not.toContain("row-private-id");
    expect(plainText).not.toContain("private-target");
  });

  it("copies the exact TSV and propagates clipboard failure", async () => {
    const writeText = vi.fn(async () => undefined);

    await copyMarkdownTable(table, { writeText });
    expect(writeText).toHaveBeenCalledWith("Name\tValue\nAlpha one\t1\n");

    await expect(copyMarkdownTable(table, undefined)).rejects.toThrow(
      "Clipboard access is unavailable",
    );
  });

  it("does not render a copy action for a table without visible rows", () => {
    const markup = renderToStaticMarkup(
      <MarkdownTable>
        <tbody>
          <tr>
            <td> </td>
          </tr>
        </tbody>
      </MarkdownTable>,
    );

    expect(markup).toContain("<table>");
    expect(markup).not.toContain("Copy table contents");
  });
});
