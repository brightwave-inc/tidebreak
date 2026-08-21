import { describe, expect, it } from "vitest";

import { foregroundBrowserScope } from "./foregroundBrowserScope";

describe("foregroundBrowserScope", () => {
  it("returns the prefixed scope for a chat id", () => {
    expect(foregroundBrowserScope("abc-123")).toBe("foreground-chat:abc-123");
  });

  it("is deterministic", () => {
    expect(foregroundBrowserScope("chat-1")).toBe(foregroundBrowserScope("chat-1"));
  });

  it("creates disjoint scopes for different chat ids", () => {
    expect(foregroundBrowserScope("chat-a")).not.toBe(foregroundBrowserScope("chat-b"));
  });
});
