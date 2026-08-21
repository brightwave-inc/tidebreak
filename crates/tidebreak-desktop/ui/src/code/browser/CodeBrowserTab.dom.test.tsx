// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  BrowserAgentAccess,
  BrowserHostAction,
  BrowserHostEvent,
  BrowserHostSnapshot,
  CodeBrowserHost,
} from "./browserHost";
import { writeStoredBrowserSession } from "./browserPersistence";
import {
  beginBrowserNavigation,
  createBrowserSession,
} from "./browserSession";
import { CodeBrowserTab } from "./CodeBrowserTab";

type CommandCall = {
  workspaceId: string;
  browserId: string;
  action: BrowserHostAction;
};

const inspectEngine: NonNullable<BrowserHostSnapshot["engine"]> = {
  name: "wk_webview",
  capabilities: {
    lifecycle: true,
    persistentProfile: true,
    semanticSnapshot: true,
    semanticActions: false,
    screenshot: false,
    crossOriginFrames: false,
    profileReset: false,
  },
};

function agentAccess(
  update: Partial<BrowserAgentAccess> = {},
): BrowserAgentAccess {
  return {
    shared: false,
    paused: false,
    halted: false,
    origin: "https://example.com",
    canObserve: false,
    canControl: false,
    canTransferFiles: false,
    ...update,
  };
}

function browserHost(options?: {
  createGate?: Promise<void>;
  createError?: string;
  existing?: boolean;
  runtime?: Partial<BrowserHostSnapshot>;
}): {
  host: CodeBrowserHost;
  calls: CommandCall[];
  emit: (event: BrowserHostEvent) => void;
  openExternal: ReturnType<typeof vi.fn>;
} {
  const calls: CommandCall[] = [];
  let handler: ((event: BrowserHostEvent) => void) | null = null;
  const openExternal = vi.fn().mockResolvedValue(undefined);
  const host: CodeBrowserHost = {
    available: () => true,
    subscribe: vi.fn(async (next) => {
      handler = next;
      return () => {
        handler = null;
      };
    }),
    command: vi.fn(async (workspaceId, browserId, action) => {
      calls.push({ workspaceId, browserId, action });
      if (action.type === "snapshot") {
        return options?.existing
          ? {
              exists: true,
              ...options.runtime,
              workspaceId,
              browserId,
              url: "https://example.com/restored",
              title: "Restored page",
              loadState: "ready" as const,
            }
          : { exists: false, workspaceId, browserId };
      }
      if (action.type === "create" && options?.createError) {
        throw new Error(options.createError);
      }
      if (action.type === "create" && options?.createGate) {
        await options.createGate;
      }
      return {
        exists: true,
        ...options?.runtime,
        workspaceId,
        browserId,
        url: "url" in action ? action.url : options?.runtime?.url,
      } satisfies BrowserHostSnapshot;
    }),
    openExternal,
  };
  return {
    host,
    calls,
    emit: (event) => handler?.(event),
    openExternal,
  };
}

beforeEach(() => {
  window.localStorage.clear();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 120,
    y: 180,
    width: 840,
    height: 560,
    top: 180,
    right: 960,
    bottom: 740,
    left: 120,
    toJSON: () => ({}),
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("CodeBrowserTab", () => {
  it("creates a native surface, follows navigation, and exposes browser controls", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="http://localhost:3000"
        host={runtime.host}
      />,
    );

    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );
    expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled();

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "navigation_finished",
      url: "http://localhost:3000/dashboard",
    });
    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "title_changed",
      title: "Local dashboard",
    });

    expect(await screen.findByLabelText("Browser: Local dashboard")).toBeInTheDocument();
    expect(screen.getByText("Local")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload" })).toBeEnabled();

    const input = screen.getByRole("textbox", { name: "Address or search" });
    expect(input).toHaveAccessibleDescription("Local");
    await userEvent.click(input);
    await userEvent.clear(input);
    await userEvent.type(input, "docs.rs/tauri{enter}");
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "navigate", url: "https://docs.rs/tauri" },
      }),
    );

    await userEvent.click(screen.getByRole("button", { name: "Open externally" }));
    expect(runtime.openExternal).toHaveBeenCalledWith("https://docs.rs/tauri");

    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "back" },
      }),
    );
  });

  it("keeps invalid input in the omnibox and reports the precise error", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        host={runtime.host}
      />,
    );

    const input = screen.getByRole("textbox", { name: "Address or search" });
    await userEvent.type(input, "file:///tmp/secret{enter}");

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Only HTTP and HTTPS addresses can open here",
    );
    expect(input).toHaveValue("file:///tmp/secret");
    expect(runtime.calls.some(({ action }) => action.type === "navigate")).toBe(false);
  });

  it("hides the native surface for app overlays and restores it afterwards", async () => {
    const runtime = browserHost();
    const view = render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
        obscured
      />,
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );

    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      }),
    );
  });

  it("hides a native view that finishes creating after its tab unmounts", async () => {
    let releaseCreate: (() => void) | undefined;
    const createGate = new Promise<void>((resolve) => {
      releaseCreate = resolve;
    });
    const runtime = browserHost({ createGate });
    const view = render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    view.unmount();
    releaseCreate?.();

    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );
  });

  it("replaces the native session when the same panel slot selects another browser tab", async () => {
    const runtime = browserHost();
    const view = render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com/one"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: expect.objectContaining({
          type: "create",
          url: "https://example.com/one",
        }),
      }),
    );

    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-2"
        initialUrl="https://example.com/two"
        host={runtime.host}
      />,
    );

    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-2",
        action: expect.objectContaining({
          type: "create",
          url: "https://example.com/two",
        }),
      }),
    );
    expect(screen.getByRole("textbox", { name: "Address or search" })).toHaveValue(
      "example.com/two",
    );
  });

  it("uses persisted history after recreating a missing native webview", async () => {
    const runtime = browserHost();
    const first = createBrowserSession({
      id: "browser-1",
      workspaceId: "workspace-1",
      initialUrl: "https://example.com/one",
    });
    writeStoredBrowserSession(
      beginBrowserNavigation(first, "https://example.com/two"),
    );

    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: expect.objectContaining({
          type: "create",
          url: "https://example.com/two",
        }),
      }),
    );

    await userEvent.click(screen.getByRole("button", { name: "Back" }));

    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "navigate", url: "https://example.com/one" },
      }),
    );
    expect(runtime.calls).not.toContainEqual({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      action: { type: "back" },
    });
  });

  it("ignores a late title event from the page navigated away from", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com/one"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "navigation_finished",
      url: "https://example.com/two",
    });
    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "title_changed",
      url: "https://example.com/one",
      title: "Old page",
    });
    expect(screen.queryByLabelText("Browser: Old page")).toBeNull();

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "title_changed",
      url: "https://example.com/two",
      title: "Current page",
    });
    expect(
      await screen.findByLabelText("Browser: Current page"),
    ).toBeInTheDocument();
  });

  it("turns popup requests into a controlled notice instead of opening a window", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "popup_blocked",
      url: "https://example.com/sign-in",
    });
    expect(await screen.findByText("This page tried to open a new window")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open here" }));
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "navigate", url: "https://example.com/sign-in" },
      }),
    );
  });

  it("routes blocked downloads externally and offers no retry for unsafe navigation", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "download_blocked",
      url: "https://example.com/archive.zip",
    });
    const downloadNotice = await screen.findByText(
      "Downloads are not available in the in-app browser yet",
    );
    await userEvent.click(
      within(downloadNotice.parentElement!).getByRole("button", {
        name: "Open externally",
      }),
    );
    expect(runtime.openExternal).toHaveBeenCalledWith(
      "https://example.com/archive.zip",
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "navigation_blocked",
      url: "file:///tmp/secret",
    });
    expect(
      await screen.findByText("This address cannot open in the in-app browser"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open here" })).toBeNull();
  });

  it("offers native sharing for an unshared origin without claiming ambient access", async () => {
    const runtime = browserHost({
      runtime: {
        engine: inspectEngine,
        agentAccess: agentAccess(),
      },
    });
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Share with agent" }),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "share_with_agent" },
      }),
    );
    expect(screen.queryByText("Agent can inspect")).toBeNull();
  });

  it("shows the exact shared origin and lets the user revoke access", async () => {
    const runtime = browserHost({
      runtime: {
        engine: inspectEngine,
        agentAccess: agentAccess({
          shared: true,
          scope: "origin",
          canObserve: true,
          canControl: true,
        }),
      },
    });
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    expect(await screen.findByText("example.com shared")).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Stop sharing" }),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "revoke_agent_access" },
      }),
    );
  });

  it("updates a paused origin from an access-only event and offers native resume", async () => {
    const runtime = browserHost({
      runtime: {
        engine: inspectEngine,
        agentAccess: agentAccess(),
      },
    });
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "agent_navigation_paused",
      origin: "https://accounts.example.org",
      agentAccess: agentAccess({
        paused: true,
        halted: true,
        origin: "https://accounts.example.org",
      }),
    });

    expect(await screen.findByText("Agent paused")).toBeInTheDocument();
    expect(screen.getByLabelText(
      "Agent paused before https://accounts.example.org",
    )).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Review & resume" }),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "share_with_agent" },
      }),
    );
  });

  it("shows live agent control and wires Stop and Take over to the native host", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "controller_changed",
      controller: {
        kind: "agent",
        label: "Review agent",
        action: "Checking the preview form",
      },
    });

    const activeControl = await screen.findByRole("status");
    expect(activeControl).toHaveTextContent("Review agent is using this tab");
    expect(activeControl).toHaveTextContent("Checking the preview form");
    await userEvent.click(
      within(activeControl).getByRole("button", { name: "Stop" }),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "stop_agent_control" },
      }),
    );

    runtime.emit({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      type: "controller_changed",
      controller: {
        kind: "agent",
        label: "Review agent",
        action: "Enter the one-time code",
        takeoverRequired: true,
      },
    });

    const takeover = await screen.findByRole("status");
    expect(takeover).toHaveTextContent("Waiting for you");
    expect(takeover).toHaveTextContent("Enter the one-time code");
    await userEvent.click(
      within(takeover).getByRole("button", { name: "Take over" }),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "take_human_control" },
      }),
    );
  });

  it("renders a retryable failure without losing the requested address", async () => {
    const runtime = browserHost({ createError: "native view failed" });
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com/docs"
        host={runtime.host}
      />,
    );

    expect(await screen.findByText("This page did not open")).toBeInTheDocument();
    expect(screen.getByText("native view failed")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Address or search" })).toHaveValue(
      "example.com/docs",
    );
    expect(screen.getByRole("button", { name: "Try again" })).toBeEnabled();
  });

  it("does not create an iframe fallback outside the native desktop", async () => {
    const runtime = browserHost();
    runtime.host.available = () => false;
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );
    expect(document.querySelector("iframe")).toBeNull();
    expect(
      await screen.findByText(
        "The in-app browser is available in the Tidebreak desktop app",
      ),
    ).toBeInTheDocument();
    fireEvent.submit(screen.getByRole("textbox", { name: "Address or search" }).closest("form")!);
  });
});
