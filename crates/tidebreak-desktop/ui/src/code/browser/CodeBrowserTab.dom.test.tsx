// @vitest-environment jsdom
import {
  act,
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
  snapshotGate?: Promise<void>;
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
        if (options?.snapshotGate) await options.snapshotGate;
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

let mockedClientWidth = 1024;

beforeEach(() => {
  window.localStorage.clear();
  mockedClientWidth = 1024;
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(
    () => mockedClientWidth,
  );
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

  it("keeps the native surface hidden until every overlay has closed", async () => {
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

    runtime.calls.splice(0);
    await userEvent.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    expect(await screen.findByRole("radio", { name: "Fit" })).toBeVisible();
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );

    runtime.calls.splice(0);
    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
        obscured
      />,
    );
    await userEvent.keyboard("{Escape}");
    await new Promise<void>((resolve) =>
      window.requestAnimationFrame(() => resolve()),
    );
    expect(runtime.calls).not.toContainEqual({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      action: { type: "set_visible", visible: true },
    });

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
    await waitFor(() => {
      const createAction = runtime.calls.find(({ action }) => action.type === "create");
      expect(createAction?.action).toEqual({
        type: "create",
        url: expect.stringContaining("https://example.com"),
        bounds: expect.objectContaining({ width: 840 }),
        visible: false,
      });
    });

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

  it.each([
    { label: "newly created", existing: false },
    { label: "restored", existing: true },
  ])(
    "does not reveal a $label native view after its queued reveal is unmounted",
    async ({ existing }) => {
      let releaseReady: (() => void) | undefined;
      const readyGate = new Promise<void>((resolve) => {
        releaseReady = resolve;
      });
      const queuedFrames: Array<{
        callback: FrameRequestCallback;
        id: number;
      }> = [];
      const cancelledFrames = new Set<number>();
      let nextFrame = 1;
      vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
        const id = nextFrame++;
        queuedFrames.push({ callback, id });
        return id;
      });
      vi.spyOn(window, "cancelAnimationFrame").mockImplementation((id) => {
        cancelledFrames.add(id);
      });

      const runtime = browserHost({
        existing,
        ...(existing
          ? { snapshotGate: readyGate }
          : { createGate: readyGate }),
      });
      const view = render(
        <CodeBrowserTab
          workspaceId="workspace-1"
          browserId="browser-1"
          initialUrl="https://example.com"
          host={runtime.host}
        />,
      );
      await waitFor(() =>
        expect(runtime.calls.some(({ action }) =>
          action.type === (existing ? "snapshot" : "create")
        )).toBe(true),
      );

      const framesBeforeReady = queuedFrames.length;
      await act(async () => releaseReady?.());
      await waitFor(() =>
        expect(queuedFrames.length).toBeGreaterThan(framesBeforeReady),
      );
      const revealFrame = queuedFrames.at(-1);
      expect(revealFrame).toBeDefined();
      expect(runtime.calls).not.toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      });

      view.unmount();
      await waitFor(() =>
        expect(runtime.calls).toContainEqual({
          workspaceId: "workspace-1",
          browserId: "browser-1",
          action: { type: "set_visible", visible: false },
        }),
      );
      expect(cancelledFrames).toContain(revealFrame!.id);
      const finalHideIndex = runtime.calls
        .map(({ action }) =>
          action.type === "set_visible" && action.visible === false
        )
        .lastIndexOf(true);

      await act(async () => revealFrame!.callback(16));

      expect(runtime.calls.slice(finalHideIndex + 1)).not.toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      });
    },
  );

  it("keeps a native view hidden when an overlay opens while creation is in flight", async () => {
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
    await waitFor(() => {
      const createAction = runtime.calls.find(({ action }) => action.type === "create");
      expect(createAction).toBeDefined();
      // The create payload itself must be hidden unconditionally.
      expect(createAction?.action).toEqual({
        type: "create",
        url: expect.stringContaining("https://example.com"),
        bounds: expect.objectContaining({ width: 840 }),
        visible: false,
      });
    });

    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
        obscured
      />,
    );
    releaseCreate?.();

    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );
    // No early reveal should have been issued.
    expect(runtime.calls).not.toContainEqual({
      workspaceId: "workspace-1",
      browserId: "browser-1",
      action: { type: "set_visible", visible: true },
    });
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

  it("applies the current viewport bounds when a viewport change races a pending snapshot", async () => {
    // Simulate: remount with Fit (840 px pane), fire ResizeObserver to change
    // surface to Mobile 390 while snapshot is still pending, resolve snapshot,
    // and assert the final set_bounds uses the centered 390 px rectangle, not
    // the stale 840 px pre-await capture.
    let releaseSnapshot: (() => void) | undefined;
    const snapshotGate = new Promise<void>((resolve) => {
      releaseSnapshot = resolve;
    });
    const runtime = browserHost({ existing: true, createGate: snapshotGate });
    // Modify command to gate snapshot instead of create:
    runtime.host.command = vi.fn(async (workspaceId, browserId, action) => {
      runtime.calls.push({ workspaceId, browserId, action });
      if (action.type === "snapshot") {
        await snapshotGate;
        return {
          exists: true,
          workspaceId,
          browserId,
          url: "https://example.com/restored",
          title: "Restored page",
          loadState: "ready" as const,
        };
      }
      return {
        exists: true,
        workspaceId,
        browserId,
        url: "url" in action ? (action as { url: string }).url : "https://example.com/restored",
      } satisfies BrowserHostSnapshot;
    });

    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com/one"
        host={runtime.host}
      />,
    );
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "snapshot")).toBe(true),
    );

    // Simulate ResizeObserver firing with Mobile 390 centered bounds
    // before the snapshot resolves.
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 225,
      y: 180,
      width: 390,
      height: 560,
      top: 180,
      right: 615,
      bottom: 740,
      left: 225,
      toJSON: () => ({}),
    });

    // Let ResizeObserver run one frame (it won't set_bounds because nativeReady is still false)
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    // The resize path should have skipped — no set_bounds yet
    expect(runtime.calls.filter(({ action }) => action.type === "set_bounds")).toHaveLength(0);

    // Now release the snapshot
    releaseSnapshot?.();
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "set_bounds")).toBe(true),
    );

    // The final set_bounds must use the live 390px centered surface, not the
    // 840px pre-await capture.
    const boundsCalls = runtime.calls.filter(({ action }) => action.type === "set_bounds");
    expect(boundsCalls).toHaveLength(1);
    expect(boundsCalls[0].action).toEqual({
      type: "set_bounds",
      bounds: { x: 225, y: 180, width: 390, height: 560 },
    });
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

  it("renders the viewport control in the toolbar and disables it without a URL", async () => {
    const runtime = browserHost();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        host={runtime.host}
      />,
    );
    const viewportButton = screen.getByRole("button", { name: /Viewport: Fit/i });
    expect(viewportButton).toBeDisabled();
  });

  it("enables the viewport control once a page loads", async () => {
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
    const viewportButton = await screen.findByRole("button", { name: /Viewport: Fit/i });
    expect(viewportButton).toBeEnabled();
  });

  it("updates the viewport trigger label when the preset changes", async () => {
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

    // Initially shows Fit
    expect(screen.getByRole("button", { name: /Viewport: Fit/i })).toBeEnabled();

    // Open the viewport popover and switch to mobile
    await userEvent.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    await userEvent.click(screen.getByRole("radio", { name: /Mobile/i }));

    // The trigger label should now show Mobile
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Viewport: Mobile 390/i })).toBeVisible(),
    );
  });

  it("persists the viewport preference to localStorage", async () => {
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

    await userEvent.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    await userEvent.click(screen.getByRole("radio", { name: /Tablet/i }));

    await waitFor(() => {
      const raw = window.localStorage.getItem("tidebreak.code-browser-viewport.v1");
      expect(raw).not.toBeNull();
      const parsed = JSON.parse(raw!);
      expect(parsed.preset).toBe("tablet");
    });
  });

  it("hides the viewport control trigger alongside the native surface when obscured", async () => {
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

    // The viewport control button should be present before obscuring
    expect(screen.getByRole("button", { name: /Viewport: Fit/i })).toBeInTheDocument();

    view.rerender(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
        obscured
      />,
    );

    // The set_visible false command should fire
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );
  });
});

describe("BrowserToolbar compact", () => {
  it("keeps the unshared action and essential controls keyboard-reachable at 320 px", async () => {
    mockedClientWidth = 320;
    const runtime = browserHost({
      runtime: {
        engine: inspectEngine,
        agentAccess: agentAccess(),
      },
    });
    const user = userEvent.setup();
    const { container } = render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    const share = await screen.findByRole("button", { name: "Share with agent" });
    expect(container.querySelector('[data-compact="true"]')).not.toBeNull();
    expect(screen.getByRole("button", { name: "Back" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Forward" })).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Address or search" })).toBeVisible();
    expect(screen.getByRole("button", { name: /Viewport:/i })).toBeVisible();
    expect(screen.getByRole("button", { name: "History" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Open externally" })).toBeVisible();

    share.focus();
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "share_with_agent" },
      }),
    );
  });

  it("exposes shared status and revoke through one keyboard-operated control at 320 px", async () => {
    mockedClientWidth = 320;
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
    const user = userEvent.setup();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    const access = await screen.findByRole("button", {
      name: "Shared with agent: https://example.com",
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Shared with agent: https://example.com",
    );
    access.focus();
    await user.keyboard("{Enter}");
    const revoke = screen.getByRole("menuitem", { name: "Stop sharing" });
    expect(revoke).toHaveFocus();
    await user.keyboard("{Enter}");

    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "revoke_agent_access" },
      }),
    );
  });

  it("exposes paused status, resume, and revoke through one keyboard-operated control at 390 px", async () => {
    mockedClientWidth = 390;
    const runtime = browserHost({
      runtime: {
        engine: inspectEngine,
        agentAccess: agentAccess({
          shared: true,
          paused: true,
          halted: true,
          origin: "https://accounts.example.org",
          scope: "origin",
        }),
      },
    });
    const user = userEvent.setup();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    const access = await screen.findByRole("button", {
      name: "Agent paused before https://accounts.example.org",
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Agent paused before https://accounts.example.org",
    );
    access.focus();
    await user.keyboard("{Enter}");
    const resume = screen.getByRole("menuitem", { name: "Review & resume" });
    expect(resume).toHaveFocus();
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "share_with_agent" },
      }),
    );

    access.focus();
    await user.keyboard("{Enter}");
    await user.keyboard("{ArrowDown}");
    const revoke = screen.getByRole("menuitem", { name: "Stop sharing" });
    expect(revoke).toHaveFocus();
    await user.keyboard("{Enter}");
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "revoke_agent_access" },
      }),
    );
  });

  it("closes the compact Agent access menu and restores the native view when the toolbar grows past 420 px", async () => {
    // Regression: opening the compact Agent access DropdownMenu calls the
    // parent obscuration callback true. Resizing to >=420 switches to the
    // wide branch and unmounts Radix without onOpenChange(false), leaving
    // the native WKWebView hidden. This test verifies a distinct
    // agentAccessOpen visibility source is closed explicitly on compact->wide
    // transitions and the native view is subsequently revealed.

    // Capture ResizeObserver callbacks so we can manually fire a resize event
    // after changing the mocked clientWidth. The global stub in setup.ts fires
    // once synchronously on observe; we replicate that and store the callback.
    const observerCallbacks: Array<ResizeObserverCallback> = [];
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: ResizeObserverCallback) {
        observerCallbacks.push(callback);
      }
      observe(target: Element) {
        const entry = {
          target,
          contentRect: { width: 1024, height: 768, top: 0, left: 0, right: 1024, bottom: 768, x: 0, y: 0 },
          borderBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
          contentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
          devicePixelContentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
        } as unknown as ResizeObserverEntry;
        const cb = observerCallbacks.at(-1)!;
        cb([entry], this as unknown as ResizeObserver);
      }
      unobserve() {}
      disconnect() {}
    });

    mockedClientWidth = 390;
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
    const user = userEvent.setup();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    // Wait for the native surface to be created and revealed
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      }),
    );

    // Clear calls before opening the menu so we can verify the hide command
    runtime.calls.splice(0);

    // Open the compact Agent access menu
    const access = screen.getByRole("button", {
      name: "Shared with agent: https://example.com",
    });
    await user.click(access);
    // The menu should be open
    expect(screen.getByRole("menuitem", { name: "Stop sharing" })).toBeVisible();

    // The native view should be hidden while the menu is open
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );

    // Simulate resizing from 390px to 1024px (past the 420px breakpoint).
    // Fire the ResizeObserver callback for the toolbar observer so
    // useCompactToolbar re-reads clientWidth and switches to wide mode.
    runtime.calls.splice(0);
    mockedClientWidth = 1024;
    const toolbarObserverCallback = observerCallbacks[0];
    expect(toolbarObserverCallback).toBeDefined();
    const resizeEntry = {
      target: null as unknown as Element,
      contentRect: { width: 1024, height: 768, top: 0, left: 0, right: 1024, bottom: 768, x: 0, y: 0 },
      borderBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
      contentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
      devicePixelContentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
    } as unknown as ResizeObserverEntry;
    await act(async () => {
      toolbarObserverCallback([resizeEntry], {} as ResizeObserver);
    });

    // The compact->wide effect should have closed agentAccessOpen, which
    // makes the native view visible again. Flush the reveal frame and verify.
    await act(async () => {
      await new Promise<void>((resolve) =>
        window.requestAnimationFrame(() => resolve()),
      );
    });
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      }),
    );

    // The compact menu trigger should no longer be present (wide branch renders inline)
    expect(
      screen.queryByRole("button", { name: "Shared with agent: https://example.com" }),
    ).toBeNull();
    // The wide-branch inline "Stop sharing" button should be present instead
    expect(screen.getByRole("button", { name: "Stop sharing" })).toBeVisible();

    vi.unstubAllGlobals();
  });

  it("closes the compact Agent access menu and restores the native view when agent access is revoked while the menu is open", async () => {
    // Regression: a host event revokes agent access while the compact
    // Agent access DropdownMenu is open. BrowserAgentAccessControl returns
    // null, Radix unmounts without onOpenChange(false), and agentAccessOpen
    // would stay latched true, keeping the native WKWebView hidden.

    const observerCallbacks: Array<ResizeObserverCallback> = [];
    vi.stubGlobal("ResizeObserver", class {
      constructor(callback: ResizeObserverCallback) {
        observerCallbacks.push(callback);
      }
      observe(target: Element) {
        const entry = {
          target,
          contentRect: { width: 390, height: 768, top: 0, left: 0, right: 390, bottom: 768, x: 0, y: 0 },
          borderBoxSize: [{ inlineSize: 390, blockSize: 768 }],
          contentBoxSize: [{ inlineSize: 390, blockSize: 768 }],
          devicePixelContentBoxSize: [{ inlineSize: 390, blockSize: 768 }],
        } as unknown as ResizeObserverEntry;
        const cb = observerCallbacks.at(-1)!;
        cb([entry], this as unknown as ResizeObserver);
      }
      unobserve() {}
      disconnect() {}
    });

    mockedClientWidth = 390;
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
    const user = userEvent.setup();
    render(
      <CodeBrowserTab
        workspaceId="workspace-1"
        browserId="browser-1"
        initialUrl="https://example.com"
        host={runtime.host}
      />,
    );

    // Wait for create and initial reveal
    await waitFor(() =>
      expect(runtime.calls.some(({ action }) => action.type === "create")).toBe(true),
    );
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      }),
    );

    // Open the compact Agent access menu
    const access = screen.getByRole("button", {
      name: "Shared with agent: https://example.com",
    });
    await user.click(access);
    expect(screen.getByRole("menuitem", { name: "Stop sharing" })).toBeVisible();

    // Native view should be hidden while the menu is open
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: false },
      }),
    );

    // Emit a host event that revokes agent access. The real host keeps the
    // current HTTP origin (origin stays https://example.com) while paused
    // and shared both become false. The compact Agent access dropdown is
    // mounted only while compact && (paused || shared); with both false it
    // unmounts without Radix calling onOpenChange(false). The mounted
    // predicate effect in BrowserAgentAccessControl must close
    // agentAccessOpen, restoring native visibility.
    runtime.calls.splice(0);
    await act(async () => {
      runtime.emit({
        type: "navigation_finished",
        url: "https://example.com",
        browserId: "browser-1",
        workspaceId: "workspace-1",
        agentAccess: { ...agentAccess(), shared: false, paused: false },
      });
    });

    // The compact menu trigger should no longer be present; the wide-branch
    // "Share with agent" button renders instead since neither paused nor
    // shared is true.
    expect(
      screen.queryByRole("button", { name: "Shared with agent: https://example.com" }),
    ).toBeNull();

    // The mounted-predicate effect should close agentAccessOpen, restoring
    // native visibility.
    await act(async () => {
      await new Promise<void>((resolve) =>
        window.requestAnimationFrame(() => resolve()),
      );
    });
    await waitFor(() =>
      expect(runtime.calls).toContainEqual({
        workspaceId: "workspace-1",
        browserId: "browser-1",
        action: { type: "set_visible", visible: true },
      }),
    );

    vi.unstubAllGlobals();
  });

});
