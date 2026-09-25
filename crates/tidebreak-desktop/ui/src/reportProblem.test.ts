import { describe, expect, it } from "vitest";

import {
  BUG_REPORT_TEMPLATE,
  NEW_ISSUE_URL,
  problemReportIssueUrl,
  type ProblemReportFacts,
} from "./reportProblem";

describe("problemReportIssueUrl", () => {
  it("prefills the version, operating system, and architecture, and nothing else", () => {
    const facts = {
      version: "0.117.0",
      os: "macOS 15.6",
      arch: "arm64",
      // A caller holding more than the three facts cannot leak the rest:
      // the address reaches GitHub before anyone reads it.
      error: "store error: could not open the database",
      log: "a line a conversation wrote",
    } as ProblemReportFacts;

    const url = new URL(problemReportIssueUrl(facts));

    expect(`${url.origin}${url.pathname}`).toBe(NEW_ISSUE_URL);
    expect([...url.searchParams.keys()]).toEqual([
      "template",
      "version",
      "os",
      "arch",
    ]);
    expect(Object.fromEntries(url.searchParams)).toEqual({
      template: BUG_REPORT_TEMPLATE,
      version: "0.117.0",
      os: "macOS 15.6",
      arch: "arm64",
    });
  });

  it("leaves out a fact the shell could not read", () => {
    const url = new URL(
      problemReportIssueUrl({ version: "0.117.0", os: null, arch: " " }),
    );
    expect(Object.fromEntries(url.searchParams)).toEqual({
      template: BUG_REPORT_TEMPLATE,
      version: "0.117.0",
    });
  });

  it("fills fields the bug report form actually has", async () => {
    const { readFileSync } = await import("node:fs");
    const form = readFileSync(
      new URL(
        `../../../../.github/ISSUE_TEMPLATE/${BUG_REPORT_TEMPLATE}`,
        import.meta.url,
      ),
      "utf8",
    );
    // GitHub prefills an issue form's inputs by field id; a renamed field
    // would silently drop the fact.
    for (const field of ["version", "os", "arch"]) {
      expect(form).toMatch(new RegExp(`- type: input\\n\\s+id: ${field}\\n`));
    }
  });
});
