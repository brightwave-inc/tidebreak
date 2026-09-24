// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

const reportRendererError = vi.hoisted(() => vi.fn());
vi.mock("./rendererErrors", () => ({ reportRendererError }));
// The Work rail needs the whole app's context; the frame is what is under test.
vi.mock("./sidebar/AppSidebar", () => ({
  AppSidebar: () => <nav aria-label="Work rail">Work rail</nav>,
}));

import {
  reportRouteError,
  RouteCrashScreen,
  RouteNotFound,
  RoutePaneError,
} from "./RouteFallbacks";
import { RouteFrame } from "./RouteFrame";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  reportRendererError.mockReset();
});

/**
 * The app's shape in miniature: a shell with no rail of its own, a layout
 * that draws a rail beside its pane, and the router's fallbacks configured
 * the way `createAppRouter` configures them.
 */
function appRouter(
  path: string,
  options: { crashingPage?: () => never; crashingShell?: boolean } = {},
) {
  const root = createRootRoute({
    component: () => {
      if (options.crashingShell) throw new Error("the shell exploded");
      return (
        <div data-testid="shell">
          <Outlet />
        </div>
      );
    },
  });
  const settings = createRoute({
    getParentRoute: () => root,
    path: "/settings",
    component: () => (
      <RouteFrame sidebar={<nav aria-label="Settings rail">Settings</nav>}>
        <Outlet />
      </RouteFrame>
    ),
  });
  const settingsSection = createRoute({
    getParentRoute: () => settings,
    path: "/general",
    errorComponent: RoutePaneError,
    component: () => <h1>General</h1>,
  });
  const work = createRoute({
    getParentRoute: () => root,
    id: "work-layout",
    component: () => (
      <RouteFrame sidebar={<nav aria-label="Work rail">Work rail</nav>}>
        <Outlet />
      </RouteFrame>
    ),
  });
  const home = createRoute({
    getParentRoute: () => work,
    path: "/",
    errorComponent: RoutePaneError,
    component: () => <h1>Home</h1>,
  });
  const page = createRoute({
    getParentRoute: () => work,
    path: "/page",
    errorComponent: RoutePaneError,
    component: () => {
      options.crashingPage?.();
      return <h1>The page</h1>;
    },
  });
  return createRouter({
    routeTree: root.addChildren([
      settings.addChildren([settingsSection]),
      work.addChildren([home, page]),
    ]),
    history: createMemoryHistory({ initialEntries: [path] }),
    defaultErrorComponent: RouteCrashScreen,
    defaultNotFoundComponent: RouteNotFound,
    defaultOnCatch: reportRouteError,
  });
}

async function renderRouter(router: ReturnType<typeof appRouter>) {
  await act(async () => {
    await router.load();
  });
  render(<RouterProvider router={router} />);
}

describe("RoutePaneError", () => {
  it("shows a page's crash in its pane and keeps the rail beside it", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    await renderRouter(
      appRouter("/page", {
        crashingPage: () => {
          throw new Error("the page exploded");
        },
      }),
    );

    const pane = screen.getByRole("main");
    const alert = await within(pane).findByRole("alert");
    expect(alert).toHaveTextContent("This page hit an unexpected error");
    expect(alert).toHaveTextContent("the page exploded");
    expect(screen.getByRole("navigation", { name: "Work rail" })).toBeTruthy();
    expect(reportRendererError).toHaveBeenCalledWith(
      "render",
      expect.objectContaining({ message: "the page exploded" }),
      expect.anything(),
    );
  });

  it("renders the page again when the reader tries again", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    let broken = true;
    await renderRouter(
      appRouter("/page", {
        crashingPage: () => {
          if (broken) throw new Error("a passing fault");
          return undefined as never;
        },
      }),
    );
    const retry = await screen.findByRole("button", { name: "Try again" });

    // Whatever broke the page has cleared by the time the reader retries.
    broken = false;
    await userEvent.click(retry);

    expect(
      await screen.findByRole("heading", { name: "The page" }),
    ).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("goes home from the crashed page without reloading the window", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    await renderRouter(
      appRouter("/page", {
        crashingPage: () => {
          throw new Error("still broken");
        },
      }),
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Go home" }),
    );

    expect(await screen.findByRole("heading", { name: "Home" })).toBeTruthy();
  });
});

describe("RouteCrashScreen", () => {
  it("takes the window for a crash no frame survived, with every recovery", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
    await renderRouter(appRouter("/", { crashingShell: true }));

    const screenAlert = await screen.findByRole("alert");
    expect(screenAlert).toHaveTextContent("Tidebreak hit an unexpected error.");
    expect(screenAlert).toHaveTextContent("the shell exploded");
    for (const name of ["Reload", "Go home", "Copy debug info"]) {
      expect(within(screenAlert).getByRole("button", { name })).toBeTruthy();
    }
    expect(screen.queryByRole("navigation")).toBeNull();
  });
});

describe("RouteNotFound", () => {
  it("brings the Work rail to an unknown path, with a way home", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    await renderRouter(appRouter("/no/such/page"));

    const pane = await screen.findByRole("main");
    expect(
      within(pane).getByRole("heading", { name: "This page does not exist" }),
    ).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "Work rail" })).toBeTruthy();

    await userEvent.click(
      within(pane).getByRole("button", { name: "Go home" }),
    );
    expect(await screen.findByRole("heading", { name: "Home" })).toBeTruthy();
  });

  it("fills the pane of a frame that already drew its rail, and adds no second one", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    await renderRouter(appRouter("/settings/no-such-section"));

    const pane = await screen.findByRole("main");
    expect(
      within(pane).getByRole("heading", { name: "This page does not exist" }),
    ).toBeTruthy();
    expect(
      screen.getByRole("navigation", { name: "Settings rail" }),
    ).toBeTruthy();
    expect(screen.queryByRole("navigation", { name: "Work rail" })).toBeNull();
    expect(screen.getAllByRole("main")).toHaveLength(1);
  });
});
