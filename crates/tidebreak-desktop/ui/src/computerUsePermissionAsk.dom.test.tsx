// @vitest-environment jsdom
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatListStore } from "./ChatListStore";
import {
  ComputerUsePermissionAskHost,
  ComputerUsePermissionNoticeHost,
} from "./ComputerUsePermissionAskHost";
import {
  asksForTask,
  PERMISSION_REQUIRED_EVENT,
  permissionAskDeclined,
  type PermissionRequired,
  useComputerUsePermissionAsk,
} from "./computerUsePermissionAsk";
import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "./computerUsePermissions";

const native = vi.hoisted(() => ({
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => true,
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (event: string, handler: (event: { payload: unknown }) => void) => {
      native.handlers.set(event, handler);
      return () => native.handlers.delete(event);
    },
  ),
}));

const missing: ComputerUsePermissionStatus = {
  status: "available",
  appName: "Tidebreak",
  appIdentifier: "io.brightwave.tidebreak",
  accessibility: false,
  screenRecording: false,
};

function host(): ComputerUsePermissionHost & {
  status: ReturnType<typeof vi.fn>;
  request: ReturnType<typeof vi.fn>;
} {
  return {
    availability: () => "local",
    status: vi.fn().mockResolvedValue(missing),
    request: vi.fn().mockResolvedValue(missing),
    openSettings: vi.fn().mockResolvedValue(undefined),
    restart: vi.fn().mockResolvedValue(undefined),
  };
}

const needsScreenRecording: PermissionRequired = {
  taskId: "0d9e1c55-6d3a-4f2f-9d0e-3f1c2a7b8e41",
  permission: "screen_recording",
  browser: false,
  afterConsent: false,
};

/** What the desktop sends when a task's operation hits a missing permission. */
async function taskNeeds(need: PermissionRequired) {
  await waitFor(() =>
    expect(native.handlers.has(PERMISSION_REQUIRED_EVENT)).toBe(true),
  );
  act(() =>
    native.handlers.get(PERMISSION_REQUIRED_EVENT)?.({ payload: need }),
  );
}

function renderShell(permissionHost = host()) {
  const onOpenSettings = vi.fn();
  render(
    <>
      <ComputerUsePermissionAskHost
        host={permissionHost}
        onOpenSettings={onOpenSettings}
      />
      <ComputerUsePermissionNoticeHost onOpenSettings={onOpenSettings} />
    </>,
  );
  return { permissionHost, onOpenSettings };
}

beforeEach(() => {
  window.localStorage.clear();
  native.handlers.clear();
  useComputerUsePermissionAsk.setState({
    ask: null,
    needs: [],
    screenRecordingRequested: false,
  });
  useChatListStore.setState({ chats: [] });
});
afterEach(cleanup);

describe("asksForTask", () => {
  it("asks the first time, waits after Not now, and asks when the person acts again", () => {
    expect(asksForTask(needsScreenRecording, false)).toBe(true);
    expect(asksForTask(needsScreenRecording, true)).toBe(false);
    expect(
      asksForTask({ ...needsScreenRecording, afterConsent: true }, true),
    ).toBe(true);
  });
});

describe("the macOS permission ask", () => {
  it("asks nothing at launch, even with both permissions missing", async () => {
    const { permissionHost } = renderShell();
    await waitFor(() =>
      expect(native.handlers.has(PERMISSION_REQUIRED_EVENT)).toBe(true),
    );

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("complementary")).toBeNull();
    // Not even a status read: the helper stays idle until a task needs it.
    expect(permissionHost.status).not.toHaveBeenCalled();
    expect(permissionHost.request).not.toHaveBeenCalled();
  });

  it("asks the first time a task needs a permission", async () => {
    const { permissionHost } = renderShell();

    await taskNeeds(needsScreenRecording);

    expect(
      await screen.findByRole("dialog", {
        name: "Allow Tidebreak to use apps on this Mac",
      }),
    ).toBeInTheDocument();
    await waitFor(() => expect(permissionHost.status).toHaveBeenCalled());
    expect(permissionHost.request).not.toHaveBeenCalled();
  });

  it("does not ask again after Not now until the person acts again", async () => {
    renderShell();
    await taskNeeds(needsScreenRecording);
    await userEvent.click(
      await screen.findByRole("button", { name: "Not now" }),
    );
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(permissionAskDeclined()).toBe(true);

    // The task says what is missing and how to allow it instead.
    const notice = await screen.findByRole("complementary", {
      name: "Computer use needs Screen Recording",
    });
    expect(notice).toHaveTextContent("take screenshots of other apps");

    // The task tries again: no second ask.
    await taskNeeds({ ...needsScreenRecording, permission: "accessibility" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(
      await screen.findByRole("complementary", {
        name: "Computer use needs Accessibility",
      }),
    ).toBeInTheDocument();

    // The person acts: Allow on the notice opens the ask again.
    await userEvent.click(screen.getByRole("button", { name: "Allow…" }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(permissionAskDeclined()).toBe(false);
  });

  it("asks again after Not now when the person allows a task to use an app", async () => {
    renderShell();
    await taskNeeds(needsScreenRecording);
    await userEvent.click(
      await screen.findByRole("button", { name: "Not now" }),
    );

    // Allowing a task to control a browser turns browser control on.
    await taskNeeds({
      ...needsScreenRecording,
      browser: true,
      afterConsent: true,
    });

    expect(
      await screen.findByRole("dialog", {
        name: "Allow Tidebreak to control your browser",
      }),
    ).toBeInTheDocument();
  });

  it("remembers Not now across launches", async () => {
    renderShell();
    await taskNeeds(needsScreenRecording);
    await userEvent.click(
      await screen.findByRole("button", { name: "Not now" }),
    );
    cleanup();
    useComputerUsePermissionAsk.setState({ ask: null, needs: [] });

    const { permissionHost } = renderShell();
    await taskNeeds(needsScreenRecording);

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(permissionHost.status).not.toHaveBeenCalled();
    expect(
      await screen.findByRole("complementary", {
        name: "Computer use needs Screen Recording",
      }),
    ).toBeInTheDocument();
  });

  it("leaves one notice per task, named for the task", async () => {
    useChatListStore.setState({
      chats: [
        {
          id: needsScreenRecording.taskId,
          title: "Tidy the Notes sidebar",
        } as never,
      ],
    });
    window.localStorage.setItem(
      "tidebreak.computer-use-permissions-not-now",
      "yes",
    );
    const { onOpenSettings } = renderShell();
    const other = "7b0c7f1e-51a4-4f0e-bb4c-8f2d9a1c3e55";

    await taskNeeds(needsScreenRecording);
    await taskNeeds({ ...needsScreenRecording, taskId: other });
    // The first task tries again: its notice is replaced, not doubled.
    await taskNeeds({ ...needsScreenRecording, permission: "accessibility" });

    const notices = screen.getAllByRole("complementary");
    expect(notices).toHaveLength(2);
    expect(notices[0]).toHaveTextContent(
      "A task stopped because macOS has not allowed Tidebreak to take screenshots",
    );
    expect(notices[1]).toHaveAccessibleName("Computer use needs Accessibility");
    expect(notices[1]).toHaveTextContent(
      "“Tidy the Notes sidebar” stopped because macOS has not allowed Tidebreak to click and type",
    );

    // Each notice goes its own way.
    await userEvent.click(
      within(notices[1]).getByRole("button", {
        name: "Open Settings → Permissions",
      }),
    );
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
    expect(screen.getAllByRole("complementary")).toHaveLength(1);
    await userEvent.click(
      screen.getByRole("button", { name: "Dismiss notice" }),
    );
    expect(screen.queryByRole("complementary")).toBeNull();
  });

  it("ignores reports it cannot read and windows attached elsewhere", async () => {
    const remote = { ...host(), availability: () => "remote" as const };
    renderShell(remote);
    await taskNeeds(needsScreenRecording);
    act(() =>
      native.handlers.get(PERMISSION_REQUIRED_EVENT)?.({
        payload: { taskId: 7 },
      }),
    );

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("complementary")).toBeNull();
  });
});
