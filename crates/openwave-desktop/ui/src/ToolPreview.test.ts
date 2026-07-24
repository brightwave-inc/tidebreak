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
