// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CodeConnectPage } from "./api";

const mocks = vi.hoisted(() => ({
  client: {
    getCodeConnectPage: vi.fn(),
    approveCodeConnect: vi.fn(),
  },
}));
vi.mock("./AppContext", () => ({
  useApp: () => ({ client: mocks.client }),
}));

import { ConnectApprovalRoute } from "./ConnectApprovalRoute";

function routerAt(nonce: string) {
  window.history.replaceState(null, "", "/#/connect/" + nonce);
  const root = createRootRoute({ component: Outlet });
  const approval = createRoute({
    getParentRoute: () => root,
    path: "/connect/$nonce",
    component: ConnectApprovalRoute,
  });
  return createRouter({
    routeTree: root.addChildren([approval]),
    history: createHashHistory(),
    defaultPreload: false,
  });
}

function pendingApproval() {
  let resolve!: () => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<void>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  mocks.client.approveCodeConnect.mockReturnValue(promise);
  return { resolve, reject };
}

beforeEach(() => {
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  mocks.client.getCodeConnectPage.mockImplementation(
    async (nonce: string): Promise<CodeConnectPage> => ({
      channel_kind: "slack",
      display_name: nonce,
      workspace_name: "Test workspace",
      state: "pending",
      csrf: "csrf-" + nonce,
      expires_at: "2026-09-06T00:00:00Z",
    }),
  );
});

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
  vi.resetAllMocks();
  vi.restoreAllMocks();
});

describe.each([
  { path: "A to B", intermediate: false, nonce: "nonce-b" },
  { path: "A to B to A", intermediate: true, nonce: "nonce-a" },
])("ConnectApprovalRoute navigating $path", ({ intermediate, nonce }) => {
  it.each(["resolve", "reject"] as const)(
    "keeps a replacement link ready when the previous approval later %ss",
    async (outcome) => {
      const pending = pendingApproval();
      const router = routerAt("nonce-a");
      await router.load();
      render(<RouterProvider router={router} />);
      await screen.findByRole("heading", { name: "nonce-a" });
      await userEvent
        .setup()
        .click(screen.getByRole("button", { name: "Yes, this is me" }));
      expect(mocks.client.approveCodeConnect).toHaveBeenCalledWith(
        "nonce-a",
        "csrf-nonce-a",
      );

      // Connect links differ only in their fragment when the tab is reused.
      await act(async () => {
        window.location.hash = "/connect/nonce-b";
      });
      await screen.findByRole("heading", { name: "nonce-b" });
      if (intermediate) {
        await act(async () => {
          window.location.hash = "/connect/nonce-a";
        });
        await screen.findByRole("heading", { name: "nonce-a" });
      }
      await act(async () => {
        if (outcome === "resolve") pending.resolve();
        else pending.reject(new Error("The earlier approval failed"));
      });

      expect(screen.queryByText(/Approved\. To finish connecting/)).toBeNull();
      expect(screen.queryByRole("alert")).toBeNull();
      mocks.client.approveCodeConnect.mockResolvedValue(undefined);
      await userEvent
        .setup()
        .click(screen.getByRole("button", { name: "Yes, this is me" }));
      expect(mocks.client.approveCodeConnect).toHaveBeenLastCalledWith(
        nonce,
        "csrf-" + nonce,
      );
      expect(
        await screen.findByText(/Approved\. To finish connecting/),
      ).toBeTruthy();
    },
  );
});
