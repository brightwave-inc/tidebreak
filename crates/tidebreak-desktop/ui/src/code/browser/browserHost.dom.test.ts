// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  closeCodeBrowser,
  type CodeBrowserHost,
} from "./browserHost";
import {
  readStoredBrowserSession,
  writeStoredBrowserSession,
} from "./browserPersistence";
import { createBrowserSession } from "./browserSession";

describe("browser host lifecycle", () => {
  beforeEach(() => window.localStorage.clear());

  it("removes persisted state and closes the native view on explicit tab close", async () => {
    writeStoredBrowserSession(
      createBrowserSession({
        id: "browser-1",
        workspaceId: "workspace-1",
        initialUrl: "https://example.com",
      }),
    );
    const command = vi.fn().mockResolvedValue({
      exists: false,
      workspaceId: "workspace-1",
      browserId: "browser-1",
    });
    const host: CodeBrowserHost = {
      available: () => true,
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await closeCodeBrowser("workspace-1", "browser-1", host);

    expect(readStoredBrowserSession("browser-1")).toBeNull();
    expect(command).toHaveBeenCalledWith(
      "workspace-1",
      "browser-1",
      { type: "close" },
    );
  });

  it("still removes persisted state when no native desktop host exists", async () => {
    writeStoredBrowserSession(
      createBrowserSession({ id: "browser-1", workspaceId: "workspace-1" }),
    );
    const command = vi.fn();
    const host: CodeBrowserHost = {
      available: () => false,
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await closeCodeBrowser("workspace-1", "browser-1", host);

    expect(readStoredBrowserSession("browser-1")).toBeNull();
    expect(command).not.toHaveBeenCalled();
  });
});
