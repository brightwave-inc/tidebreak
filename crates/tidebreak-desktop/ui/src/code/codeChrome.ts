import { EMPTY_LAYOUT, type LayoutState, type PanelContent } from "@/panel/panelTypes";

/**
 * Split a workspace layout into the panels that sit beside the conversation
 * and the terminal, which the workspace draws as a bottom drawer instead.
 *
 * The terminal stays in the URL so a reload or a shared link still opens it.
 * Only the rendering changes: it is not a tab in the side region.
 */
export function splitCodeChromeLayout(layout: LayoutState): {
  panels: LayoutState;
  terminal: Extract<PanelContent, { type: "terminal" }> | null;
} {
  const terminalIndex = layout.tabs.findIndex((tab) => tab.type === "terminal");
  if (terminalIndex === -1) {
    return { panels: layout, terminal: null };
  }

  const terminal = layout.tabs[terminalIndex] as Extract<
    PanelContent,
    { type: "terminal" }
  >;
  const tabs = layout.tabs.filter((_, index) => index !== terminalIndex);
  if (tabs.length === 0) {
    return { panels: EMPTY_LAYOUT, terminal };
  }

  let activeIndex = layout.activeIndex;
  if (terminalIndex < activeIndex) activeIndex -= 1;
  else if (terminalIndex === activeIndex) {
    activeIndex = Math.min(terminalIndex, tabs.length - 1);
  }

  return {
    panels: {
      tabs,
      activeIndex: Math.max(0, Math.min(activeIndex, tabs.length - 1)),
      fullscreen: layout.fullscreen,
    },
    terminal,
  };
}

/** Open the terminal drawer, or close it if it is already in the layout. */
export function toggleTerminalLayout(layout: LayoutState): LayoutState {
  const index = layout.tabs.findIndex((tab) => tab.type === "terminal");
  if (index === -1) {
    return {
      ...layout,
      tabs: [...layout.tabs, { type: "terminal" }],
      activeIndex: layout.tabs.length,
    };
  }

  const tabs = layout.tabs.filter((_, at) => at !== index);
  if (tabs.length === 0) return EMPTY_LAYOUT;

  let activeIndex = layout.activeIndex;
  if (index < activeIndex) activeIndex -= 1;
  else if (index === activeIndex) {
    activeIndex = Math.min(index, tabs.length - 1);
  }
  return {
    ...layout,
    tabs,
    activeIndex: Math.max(0, Math.min(activeIndex, tabs.length - 1)),
    fullscreen: layout.fullscreen && tabs.length > 0,
  };
}
