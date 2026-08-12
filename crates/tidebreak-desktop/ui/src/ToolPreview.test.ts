import { describe, expect, it } from "vitest";
import { toolPreviewPresentation } from "./ToolPreview";

describe("toolPreviewPresentation", () => {
  it("reads a command back as the argument vector it will run", () => {
    expect(
      toolPreviewPresentation({
        tool: "exec",
        command: "cargo",
        args: ["test", "--workspace"],
        cwd: ".",
        files: [],
      }),
    ).toMatchObject({
      headline: "cargo test --workspace",
      detail: "cargo test --workspace",
    });
  });

  it("names a working directory as a fact about the command, not part of it", () => {
    expect(
      toolPreviewPresentation({
        tool: "exec",
        command: "ls",
        args: [],
        cwd: "checkout/crates",
        files: [],
      }).detail,
    ).toBe("ls\n# working directory: checkout/crates");
  });

  it("names the files the command is being handed", () => {
    // Which documents a command can read is part of the action under review:
    // the same script over an expense sheet is not the same consent.
    expect(
      toolPreviewPresentation({
        tool: "exec",
        command: "python3",
        args: ["analyze.py"],
        cwd: ".",
        files: ["documents/salaries.csv", "documents/q3.xlsx"],
      }).detail,
    ).toBe("python3 analyze.py\n# staged files: documents/salaries.csv, documents/q3.xlsx");
  });

  it("quotes an argument whose boundaries would otherwise be invisible", () => {
    // One vector element containing a space is one argument. Printed bare it
    // would read as two, which is exactly the misreading an approval can least
    // afford.
    expect(
      toolPreviewPresentation({
        tool: "exec",
        command: "python3",
        args: ["-c", "print('two words')", "", "a;rm -rf /"],
        cwd: ".",
        files: [],
      }).headline,
    ).toBe(`python3 -c 'print('\\''two words'\\'')' '' 'a;rm -rf /'`);
  });
});

describe("workspace write previews", () => {
  it("names the file the write will land on", () => {
    // The Ask-mode card used to ask about workspace files without saying
    // which one; the path is the resource under review.
    const write = toolPreviewPresentation({
      tool: "write_file",
      path: "reports/q3.md",
    });
    expect(write.headline).toBe("reports/q3.md");
    expect(write.detail).toContain("this chat's workspace");
  });
});

describe("search previews", () => {
  const unfiltered = {
    domains: [] as string[],
    start_published_at: null,
    end_published_at: null,
  };

  it("makes the query the headline and says where it goes", () => {
    // The consent question for a web search is "what leaves this machine",
    // and the answer is the query.
    const web = toolPreviewPresentation({
      tool: "web_search",
      query: "quarterly filings",
      ...unfiltered,
    });
    expect(web.headline).toBe("quarterly filings");
    expect(web.detail).toContain("web search provider");

    const local = toolPreviewPresentation({ tool: "search", query: "revenue" });
    expect(local.headline).toBe("revenue");
    // A local search shares nothing outward, and the copy must not imply it does.
    expect(local.detail).not.toContain("provider");
    expect(local.detail).toContain("this conversation's sources");
  });

  it("shows the filters that go out with the query", () => {
    // The consent copy promises the query "and its explicit filters", and the
    // filters reach the provider too — a card that showed only the query was
    // describing part of the action it asked about.
    expect(
      toolPreviewPresentation({
        tool: "web_search",
        query: "quarterly filings",
        domains: ["sec.gov", "ft.com"],
        start_published_at: "2024-01-01T00:00:00Z",
        end_published_at: "2024-06-30T00:00:00Z",
      }).detail,
    ).toBe(
      "quarterly filings\n" +
        "# limited to sec.gov, ft.com\n" +
        "# published between 2024-01-01T00:00:00Z and 2024-06-30T00:00:00Z\n" +
        "# sent to the configured web search provider",
    );
  });

  it("states an open-ended window from the end that is actually set", () => {
    expect(
      toolPreviewPresentation({
        tool: "web_search",
        query: "layoffs",
        domains: [],
        start_published_at: "2025-01-01T00:00:00Z",
        end_published_at: null,
      }).detail,
    ).toContain("# published on or after 2025-01-01T00:00:00Z");
  });
});
