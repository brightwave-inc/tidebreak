// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  clearLegacyBrowserSession,
  LEGACY_BROWSER_STORAGE_KEY,
  readLegacyBrowserSession,
} from "./browserPersistence";

function storeLegacySession(
  browserId: string,
  update: Record<string, unknown> = {},
): void {
  window.localStorage.setItem(
    LEGACY_BROWSER_STORAGE_KEY,
    JSON.stringify({
      [browserId]: {
        version: 1,
        id: browserId,
        workspaceId: "workspace-1",
        url: "https://example.com/docs",
        title: "Documentation",
        loadState: "failed",
        error: "network failed",
        inspectEnabled: true,
        history: [
          { url: "https://example.com/one" },
          { url: "https://example.com/docs" },
        ],
        historyIndex: 1,
        updatedAt: 17,
        ...update,
      },
    }),
  );
}

describe("legacy browser persistence migration", () => {
  beforeEach(() => window.localStorage.clear());

  it("reads only URL and title from a valid legacy session", () => {
    storeLegacySession("browser-1");

    expect(readLegacyBrowserSession("browser-1")).toEqual({
      kind: "valid",
      state: {
        version: 1,
        id: "browser-1",
        workspaceId: "workspace-1",
        url: "https://example.com/docs",
        title: "Documentation",
      },
    });
  });

  it("marks malformed and cross-workspace metadata for native disposal", () => {
    storeLegacySession("browser-1", { url: "javascript:alert(1)" });
    expect(readLegacyBrowserSession("browser-1")).toEqual({
      kind: "invalid",
      state: null,
    });

    storeLegacySession("browser-1", { workspaceId: "workspace-2" });
    expect(readLegacyBrowserSession("browser-1")).toMatchObject({
      kind: "valid",
      state: { workspaceId: "workspace-2" },
    });
  });

  it("clears only the acknowledged entry and never writes during reads", () => {
    storeLegacySession("browser-1");
    const first = JSON.parse(
      window.localStorage.getItem(LEGACY_BROWSER_STORAGE_KEY)!,
    );
    window.localStorage.setItem(
      LEGACY_BROWSER_STORAGE_KEY,
      JSON.stringify({ ...first, "browser-2": { version: 9 } }),
    );
    const setItem = vi.spyOn(Storage.prototype, "setItem");

    readLegacyBrowserSession("browser-1");
    expect(setItem).not.toHaveBeenCalled();

    clearLegacyBrowserSession("browser-1");
    expect(
      JSON.parse(window.localStorage.getItem(LEGACY_BROWSER_STORAGE_KEY)!),
    ).toEqual({ "browser-2": { version: 9 } });
  });

  it("removes an unreadable legacy payload after native acknowledgement", () => {
    window.localStorage.setItem(LEGACY_BROWSER_STORAGE_KEY, "not json");
    expect(readLegacyBrowserSession("browser-1")?.kind).toBe("invalid");

    clearLegacyBrowserSession("browser-1");
    expect(window.localStorage.getItem(LEGACY_BROWSER_STORAGE_KEY)).toBeNull();
  });
});
