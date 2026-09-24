// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";

import type { CliCommandChange } from "@/cliCommand";
import { CLI_STATUS } from "@/stories/cliCommandFixtures";

const host = vi.hoisted(() => ({
  status: vi.fn(),
  install: vi.fn(),
  uninstall: vi.fn(),
}));
vi.mock("@/cliCommand", () => ({
  cliCommandHost: {
    available: () => true,
    status: host.status,
    install: host.install,
    uninstall: host.uninstall,
  },
}));

import { SETTINGS_SECTIONS } from "./sections";

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

/** Settings → Command line and one other page, in a router of their own. */
async function renderCommandLine() {
  // The router restores scroll on navigation, which jsdom does not do.
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  const section = SETTINGS_SECTIONS.find(
    (entry) => entry.path === "command-line",
  );
  if (!section) throw new Error("Settings has no Command line section");
  const root = createRootRoute({ component: Outlet });
  const commandLine = createRoute({
    getParentRoute: () => root,
    path: "/settings/command-line",
    component: section.Component,
    validateSearch: section.validateSearch,
  });
  const elsewhere = createRoute({
    getParentRoute: () => root,
    path: "/elsewhere",
    component: () => <p>Somewhere else</p>,
  });
  const router = createRouter({
    routeTree: root.addChildren([commandLine, elsewhere]),
    history: createMemoryHistory({
      initialEntries: ["/settings/command-line"],
    }),
    defaultPreload: false,
  });
  await router.load();
  render(<RouterProvider router={router} />);
  // The app registers its own route tree with the router's types; this
  // throwaway tree does not need to satisfy them.
  const navigate = (options: object) =>
    act(() => router.navigate(options as never));
  return { router, navigate };
}

it("installs once when the menu asks on an open page, and Back never asks again", async () => {
  const installed: CliCommandChange = {
    location: "user",
    outcome: "created",
    folderCreated: false,
    status: CLI_STATUS.installed,
  };
  host.status.mockResolvedValue(CLI_STATUS.notInstalled);
  host.install.mockResolvedValue(installed);
  const { router, navigate } = await renderCommandLine();
  await screen.findByText("Not installed");

  // The app menu, with Settings → Command line already open.
  await navigate({
    to: "/settings/command-line",
    search: { install: "user" },
  });

  expect(await screen.findByText("Installed")).toBeInTheDocument();
  expect(host.install).toHaveBeenCalledTimes(1);
  expect(host.install).toHaveBeenCalledWith("user");
  // The request left the history entry that carried it.
  await waitFor(() => expect(router.state.location.search).toEqual({}));

  host.status.mockResolvedValue(CLI_STATUS.installed);
  await navigate({ to: "/elsewhere" });
  await screen.findByText("Somewhere else");
  act(() => router.history.back());

  await screen.findByText("Installed");
  expect(router.state.location.pathname).toBe("/settings/command-line");
  expect(router.state.location.search).toEqual({});
  expect(host.install).toHaveBeenCalledTimes(1);
});
