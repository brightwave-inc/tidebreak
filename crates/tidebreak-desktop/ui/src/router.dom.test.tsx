// @vitest-environment jsdom
import { describe, expect, it } from "vitest";

import {
  reportRouteError,
  RouteCrashScreen,
  RouteNotFound,
  RoutePaneError,
} from "./RouteFallbacks";
import { createAppRouter } from "./router";

/** The layouts that draw a rail beside their pane. */
const FRAMED_LAYOUTS = new Set(["/work-layout", "/code-layout", "/settings"]);

describe("createAppRouter fallbacks", () => {
  it("never falls to TanStack's unstyled error and not-found defaults", () => {
    const router = createAppRouter();
    expect(router.options.defaultErrorComponent).toBe(RouteCrashScreen);
    expect(router.options.defaultNotFoundComponent).toBe(RouteNotFound);
    expect(router.options.defaultOnCatch).toBe(reportRouteError);
  });

  it("keeps the rail for a crash in every route that renders inside a frame", () => {
    const router = createAppRouter();
    const framed = Object.values(router.routesById).filter((route) =>
      FRAMED_LAYOUTS.has(route.parentRoute?.id ?? ""),
    );
    // Work, code, and settings all have pages; an empty list means the
    // layout ids above no longer match the tree.
    expect(framed.length).toBeGreaterThan(20);
    const missing = framed
      .filter((route) => route.options.errorComponent !== RoutePaneError)
      .map((route) => route.id);
    expect(missing).toEqual([]);
  });
});
