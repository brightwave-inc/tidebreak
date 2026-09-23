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

import { headingMayTakeFocus, RouteFrame } from "./RouteFrame";

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

  it("leaves the caret in a field that stays mounted across the navigation", async () => {
    const root = createRootRoute({
      component: () => (
        <RouteFrame sidebar={<textarea aria-label="Draft" />}>
          <Outlet />
        </RouteFrame>
      ),
    });
    const home = createRoute({
      getParentRoute: () => root,
      path: "/",
      component: () => <h1>Home</h1>,
    });
    const code = createRoute({
      getParentRoute: () => root,
      path: "/code",
      component: () => <h1>Code</h1>,
    });
    const router = createRouter({
      routeTree: root.addChildren([home, code]),
      history: createMemoryHistory({ initialEntries: ["/"] }),
    });
    await router.load();
    render(<RouterProvider router={router} />);

    const draft = screen.getByRole("textbox", { name: "Draft" });
    draft.focus();
    await act(async () => {
      await router.navigate({ to: "/code" });
    });
    const codeHeading = await screen.findByRole("heading", { name: "Code" });
    await waitFor(() => {
      expect(codeHeading).toHaveAttribute("tabindex", "-1");
    });
    expect(document.activeElement).toBe(draft);
  });
});

describe("headingMayTakeFocus", () => {
  it("keeps focus in rich text and dialogs", () => {
    document.body.innerHTML = `
      <div contenteditable="true"><p id="rich">draft</p></div>
      <div role="dialog"><button id="keep">Keep</button></div>
      <nav><a id="link" href="/x">Chats</a></nav>`;
    expect(headingMayTakeFocus(document.getElementById("rich"))).toBe(false);
    expect(headingMayTakeFocus(document.getElementById("keep"))).toBe(false);
    expect(headingMayTakeFocus(document.getElementById("link"))).toBe(true);
    expect(headingMayTakeFocus(document.body)).toBe(true);
    document.body.innerHTML = "";
  });
});
