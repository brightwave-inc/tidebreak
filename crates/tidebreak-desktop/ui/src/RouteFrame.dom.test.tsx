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
import { afterEach, describe, expect, it } from "vitest";

import { RouteFrame } from "./RouteFrame";

afterEach(cleanup);

describe("RouteFrame", () => {
  it("moves focus to the page heading after an explicit navigation", async () => {
    const root = createRootRoute({
      component: () => (
        <RouteFrame sidebar={<nav>Rail</nav>}>
          <Outlet />
        </RouteFrame>
      ),
    });
    const home = createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => <h1>Home</h1>,
    });
    const inbox = createRoute({
      getParentRoute: () => root,
      path: "/inbox",
      component: () => <h1>Inbox</h1>,
    });
    const router = createRouter({
      routeTree: root.addChildren([home, inbox]),
      history: createMemoryHistory({ initialEntries: ["/"] }),
    });
    await router.load();
    render(<RouterProvider router={router} />);

    const homeHeading = screen.getByRole("heading", { name: "Home" });
    expect(document.activeElement).not.toBe(homeHeading);

    await act(async () => {
      await router.navigate({ to: "/inbox" });
    });
    const inboxHeading = await screen.findByRole("heading", { name: "Inbox" });
    await waitFor(() => {
      expect(inboxHeading).toHaveAttribute("tabindex", "-1");
    });
    expect(document.activeElement).toBe(inboxHeading);
  });
});
