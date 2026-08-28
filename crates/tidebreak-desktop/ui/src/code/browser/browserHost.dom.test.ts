// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setAttachedRemotely } from "@/host";
import {
  browserUnavailableMessage,
  closeCodeBrowser,
  nativeCodeBrowserHost,
  resetCodeBrowserProfile,
  type CodeBrowserHost,
} from "./browserHost";
import {
  LEGACY_BROWSER_STORAGE_KEY,
  readLegacyBrowserSession,
} from "./browserPersistence";

// The gate is `hasNativeHost() && !attachedRemotely()`, so the native half has
// to be true for the attachment half to be observable at all.
const isTauri = vi.hoisted(() => vi.fn(() => true));
vi.mock("@tauri-apps/api/core", () => ({ isTauri, invoke: vi.fn() }));

function storeLegacySession(browserId: string): void {
  window.localStorage.setItem(
    LEGACY_BROWSER_STORAGE_KEY,
    JSON.stringify({
      [browserId]: {
        version: 1,
        id: browserId,
        workspaceId: "workspace-1",
        url: "https://example.com",
        title: "Example",
        updatedAt: 17,
      },
    }),
  );
}

describe("browser host lifecycle", () => {
  beforeEach(() => window.localStorage.clear());

  it("removes persisted state and closes the native view on explicit tab close", async () => {
    storeLegacySession("browser-1");
    const command = vi.fn().mockResolvedValue({
      exists: false,
      workspaceId: "workspace-1",
      browserId: "browser-1",
    });
    const host: CodeBrowserHost = {
      available: () => true,
      importLegacyState: vi.fn(),
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await closeCodeBrowser("workspace-1", "browser-1", host);

    expect(readLegacyBrowserSession("browser-1")).toBeNull();
    expect(command).toHaveBeenCalledWith("workspace-1", "browser-1", {
      type: "close",
    });
  });

  it("still removes persisted state when no native desktop host exists", async () => {
    storeLegacySession("browser-1");
    const command = vi.fn();
    const host: CodeBrowserHost = {
      available: () => false,
      importLegacyState: vi.fn(),
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await closeCodeBrowser("workspace-1", "browser-1", host);

    expect(readLegacyBrowserSession("browser-1")).toBeNull();
    expect(command).not.toHaveBeenCalled();
  });
});

describe("managed browser profile reset", () => {
  it("sends only session identity and a non-authoritative reset correlation id", async () => {
    const command = vi.fn().mockResolvedValue({
      exists: false,
      workspaceId: "workspace-1",
      browserId: "browser-1",
    });
    const host: CodeBrowserHost = {
      available: () => true,
      importLegacyState: vi.fn(),
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await resetCodeBrowserProfile("workspace-1", "browser-1", 17, host);

    expect(command).toHaveBeenCalledWith("workspace-1", "browser-1", {
      type: "reset_profile",
      resetId: 17,
    });
  });

  it("refuses reset when this renderer has no local host authority", async () => {
    const command = vi.fn();
    const host: CodeBrowserHost = {
      available: () => false,
      importLegacyState: vi.fn(),
      command,
      subscribe: vi.fn(async () => () => undefined),
      openExternal: vi.fn(async () => undefined),
    };

    await expect(
      resetCodeBrowserProfile("workspace-1", "browser-1", 18, host),
    ).rejects.toThrow(
      "The managed browser profile is available only on this computer",
    );
    expect(command).not.toHaveBeenCalled();
  });
});

describe("browser host availability", () => {
  afterEach(() => {
    setAttachedRemotely(false);
    isTauri.mockReturnValue(true);
  });

  // Every browser control asks this one predicate, so the gate belongs here
  // rather than at each of the nine call sites — including "share with agent",
  // which would otherwise hand an agent elsewhere a browser on this laptop.
  it("offers no browser while this window works on another machine", () => {
    expect(nativeCodeBrowserHost.available()).toBe(true);
    setAttachedRemotely(true);
    expect(nativeCodeBrowserHost.available()).toBe(false);
  });

  it("names the machine rather than the build when a tab has to explain", () => {
    setAttachedRemotely(true);
    expect(browserUnavailableMessage()).toBe(
      "The in-app browser runs on this computer, and your work is on another machine",
    );
    setAttachedRemotely(false);
    expect(browserUnavailableMessage()).toBe(
      "The in-app browser is available in the Tidebreak desktop app",
    );
  });
});
