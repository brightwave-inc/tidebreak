import { describe, expect, it } from "vitest";

import { formatDocumentTitle, pageNameForPath } from "./pageTitle";

describe("document title", () => {
  it("names library routes", () => {
    expect(formatDocumentTitle(pageNameForPath("/inbox"))).toBe(
      "Inbox – Tidebreak",
    );
    expect(formatDocumentTitle(pageNameForPath("/code/analytics"))).toBe(
      "Analytics – Tidebreak",
    );
  });

  it("uses the conversation name when one is open", () => {
    expect(
      formatDocumentTitle(
        pageNameForPath("/c/abc", { conversation: "Fix login" }),
      ),
    ).toBe("Fix login – Tidebreak");
  });
});
