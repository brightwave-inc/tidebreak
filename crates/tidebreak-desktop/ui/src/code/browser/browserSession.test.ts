import { describe, expect, it } from "vitest";

import {
  beginBrowserNavigation,
  canBrowserGoBack,
  canBrowserGoForward,
  createBrowserSession,
  finishBrowserNavigation,
  moveBrowserHistory,
  setBrowserTitle,
} from "./browserSession";

describe("browser sessions", () => {
  it("branches history after navigating back", () => {
    let session = createBrowserSession({
      id: "browser-1",
      workspaceId: "workspace-1",
      initialUrl: "https://example.com/one",
      now: 1,
    });
    session = beginBrowserNavigation(session, "https://example.com/two", 2);
    session = finishBrowserNavigation(session, "https://example.com/two", 3);
    session = moveBrowserHistory(session, -1, 4);

    expect(canBrowserGoBack(session)).toBe(false);
    expect(canBrowserGoForward(session)).toBe(true);

    session = beginBrowserNavigation(session, "https://example.com/three", 5);
    expect(session.history.map((entry) => entry.url)).toEqual([
      "https://example.com/one",
      "https://example.com/three",
    ]);
    expect(canBrowserGoForward(session)).toBe(false);
  });

  it("updates the active history title without changing tab identity", () => {
    const session = setBrowserTitle(
      createBrowserSession({
        id: "browser-1",
        workspaceId: "workspace-1",
        initialUrl: "https://example.com",
      }),
      "  Example   documentation  ",
    );
    expect(session.id).toBe("browser-1");
    expect(session.title).toBe("Example documentation");
    expect(session.history[0]?.title).toBe("Example documentation");
  });

  it("bounds retained navigation history", () => {
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
    expect(session.history).toHaveLength(50);
    expect(session.history[0]?.url).toBe("https://example.com/10");
    expect(session.historyIndex).toBe(49);
  });
});
