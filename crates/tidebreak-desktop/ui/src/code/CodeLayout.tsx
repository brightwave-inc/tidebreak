import { Outlet } from "@tanstack/react-router";

import { RouteFrame } from "@/RouteFrame";
import { CodeSidebar } from "./CodeSidebar";

/**
 * The frame every `/code` route shares: one rail, one main pane.
 *
 * Mounted once on the pathless code layout route, so navigating between
 * workspaces, workspace-less conversations, and the library pages swaps only
 * the pane. The rail keeps its scroll position, selection, and live update
 * subscription across every code navigation — remounting it on each route
 * used to reload the whole view when a conversation row was opened.
 */
export function CodeLayout() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <Outlet />
    </RouteFrame>
  );
}
