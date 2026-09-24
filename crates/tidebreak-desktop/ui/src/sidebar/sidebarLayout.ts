/** Below this width the rail leaves the layout and opens as an overlay. */
export const SIDEBAR_OVERLAY_MAX_WIDTH = 899;

/** True when the window is too narrow to keep a 280px rail beside the pane. */
export function sidebarUsesOverlay(viewportWidth: number): boolean {
  return viewportWidth <= SIDEBAR_OVERLAY_MAX_WIDTH;
}
