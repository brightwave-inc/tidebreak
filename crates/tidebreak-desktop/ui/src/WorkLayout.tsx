import { Outlet } from "@tanstack/react-router";

import { RouteFrame } from "./RouteFrame";
import { AppSidebar } from "./sidebar/AppSidebar";

/**
 * The frame every Work route shares: one rail, one main pane.
 *
 * Mounted once on the pathless Work layout route, so moving between home,
 * the libraries, the inbox, projects, conversations, and the archive swaps
 * only the pane. The rail keeps its scroll position and its row state across
 * every one of those moves; remounting it per route used to snap the list
 * back to the top each time a conversation opened.
 */
export function WorkLayout() {
  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <Outlet />
    </RouteFrame>
  );
}
