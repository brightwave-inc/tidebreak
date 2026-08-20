// @vitest-environment jsdom
import { beforeEach, describe, expect, it } from "vitest";

import {
  readStoredBrowserSession,
  removeStoredBrowserSession,
  seedBrowserSession,
  storedBrowserTitle,
  writeStoredBrowserSession,
} from "./browserPersistence";
import { beginBrowserNavigation, createBrowserSession } from "./browserSession";

describe("browser persistence", () => {
  beforeEach(() => window.localStorage.clear());

  it("round-trips useful state but clears transient failures", () => {
    const session = {
      ...createBrowserSession({
        id: "browser-1",
        workspaceId: "workspace-1",
        initialUrl: "https://example.com/docs",
      }),
      loadState: "failed" as const,
      error: "network failed",
      notice: {
        kind: "popup" as const,
        url: "https://example.com/sign-in",
        message: "popup",
      },
    };
    writeStoredBrowserSession(session);
    expect(readStoredBrowserSession("browser-1")).toMatchObject({
      id: "browser-1",
      workspaceId: "workspace-1",
      url: "https://example.com/docs",
      loadState: "ready",
      error: null,
      notice: null,
    });
  });

  it("keeps the selected entry correct when stored history is trimmed", () => {
    let session = createBrowserSession({
      id: "browser-1",
      workspaceId: "workspace-1",
    });
    for (let index = 0; index < 60; index += 1) {
      session = beginBrowserNavigation(
        session,
        `https://example.com/${index}`,
        index,
      );
    }
    writeStoredBrowserSession(session);
    const restored = readStoredBrowserSession("browser-1");
    expect(restored?.history).toHaveLength(50);
    expect(restored?.history[restored.historyIndex]?.url).toBe(
      "https://example.com/59",
    );
  });

  it("drops malformed sessions and removes closed sessions", () => {
    window.localStorage.setItem(
      "tidebreak.code-browser-sessions.v1",
      JSON.stringify({ "browser-1": { version: 9 } }),
    );
    expect(readStoredBrowserSession("browser-1")).toBeNull();

    writeStoredBrowserSession(
      createBrowserSession({ id: "browser-1", workspaceId: "workspace-1" }),
    );
    removeStoredBrowserSession("browser-1");
    expect(readStoredBrowserSession("browser-1")).toBeNull();
  });

  it("seeds a new panel before mount and exposes its lightweight tab title", () => {
    seedBrowserSession({
      browserId: "browser-1",
      workspaceId: "workspace-1",
      initialUrl: "https://example.com/docs",
    });

    expect(readStoredBrowserSession("browser-1")).toMatchObject({
      workspaceId: "workspace-1",
      url: "https://example.com/docs",
    });
    expect(storedBrowserTitle("browser-1")).toBe("Browser");
  });
});
