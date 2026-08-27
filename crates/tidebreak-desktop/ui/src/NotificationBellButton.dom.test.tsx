// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AgentNotification, ApiClient } from "./api";
import { AppContextProvider, type AppContextValue } from "./AppContext";
import { NotificationBellButton } from "./NotificationBellButton";
import { useNotifications } from "./NotificationStore";
import { renderWithRouter } from "./test/router";

const notification: AgentNotification = {
  id: "notification-1",
  kind: "agent_completed",
  title: "Workspace finished",
  context: {
    surface: "code",
    sessionId: "session-1",
    workspaceId: "workspace-1",
  },
  createdAt: "2026-08-26T18:00:00Z",
  readAt: null,
};

function renderBell(client: ApiClient) {
  return renderWithRouter(
    <AppContextProvider value={{ client } as AppContextValue}>
      <NotificationBellButton defaultOpen />
    </AppContextProvider>,
    { initialUrl: "/" },
  );
}

afterEach(() => {
  cleanup();
  useNotifications.getState().clear();
});

describe("NotificationBellButton", () => {
  it("opens the target before the read request settles", async () => {
    let resolveMark: ((marked: number) => void) | undefined;
    const markNotificationsRead = vi.fn(
      () =>
        new Promise<number>((resolve) => {
          resolveMark = resolve;
        }),
    );
    const client = {
      markNotificationsRead,
    } as unknown as ApiClient;
    useNotifications.setState({
      notifications: [notification],
      unread: 1,
      loaded: true,
    });

    const { router } = await renderBell(client);
    await userEvent.click(
      await screen.findByRole("button", { name: /Workspace finished/ }),
    );

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/workspace-1"),
    );
    expect(markNotificationsRead).toHaveBeenCalledWith(["notification-1"]);
    expect(useNotifications.getState().unread).toBe(0);
    resolveMark?.(1);
  });

  it("renders invalid timestamps without crashing", async () => {
    useNotifications.setState({
      notifications: [{ ...notification, createdAt: "not-a-timestamp" }],
      unread: 1,
      loaded: true,
    });

    await renderBell({} as ApiClient);

    expect(await screen.findByText("Unknown time")).toBeInTheDocument();
  });

  it("caps the popover width to the viewport", async () => {
    useNotifications.setState({
      notifications: [notification],
      unread: 1,
      loaded: true,
    });

    await renderBell({} as ApiClient);

    expect(
      await screen.findByRole("dialog", { name: "Notifications" }),
    ).toHaveClass("w-80", "max-w-[calc(100vw-1rem)]");
  });
});
