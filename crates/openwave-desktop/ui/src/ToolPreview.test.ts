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
      }).detail,
    ).toBe("ls\n# working directory: checkout/crates");
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
      }).headline,
    ).toBe(`python3 -c 'print('\\''two words'\\'')' '' 'a;rm -rf /'`);
  });
});

describe("search previews", () => {
  it("makes the query the headline and says where it goes", () => {
    // The consent question for a web search is "what leaves this machine",
    // and the answer is the query.
    const web = toolPreviewPresentation({
      tool: "web_search",
      query: "quarterly filings",
    });
    expect(web.headline).toBe("quarterly filings");
    expect(web.detail).toContain("web search provider");

    const local = toolPreviewPresentation({ tool: "search", query: "revenue" });
    expect(local.headline).toBe("revenue");
    // A local search shares nothing outward, and the copy must not imply it does.
    expect(local.detail).not.toContain("provider");
    expect(local.detail).toContain("this conversation's sources");
  });
});
