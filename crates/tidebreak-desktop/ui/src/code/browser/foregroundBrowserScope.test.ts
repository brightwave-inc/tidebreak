import { describe, expect, it } from "vitest";

import { foregroundBrowserScope } from "./foregroundBrowserScope";

describe("foregroundBrowserScope", () => {
  it("returns the prefixed scope for a chat id", () => {
    expect(foregroundBrowserScope("abc-123")).toBe("foreground-chat:abc-123");
  });
});
