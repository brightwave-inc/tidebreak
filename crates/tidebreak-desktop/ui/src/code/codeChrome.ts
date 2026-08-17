import {
  EMPTY_LAYOUT,
  panelKey,
  type LayoutState,
  type PanelContent,
} from "@/panel/panelTypes";

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
    // The URL named the drawer. Pick a remaining side tab rather than
    // leaving the strip pointing at a hole.
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

/**
 * URL-tab index of the side-region tab at `stripIndex`.
 *
 * The strip is the URL tabs with the terminal removed, so a click or close
 * on the strip has to skip over the drawer rather than treat it as a neighbour.
 */
function codeChromeUrlIndex(layout: LayoutState, stripIndex: number): number {
  if (stripIndex < 0) return -1;
  let remaining = stripIndex;
  for (let index = 0; index < layout.tabs.length; index += 1) {
    if (layout.tabs[index]?.type === "terminal") continue;
    if (remaining === 0) return index;
    remaining -= 1;
  }
  return -1;
}

/** Bring the side-region tab at `stripIndex` forward, leaving the drawer in place. */
export function focusCodeChromeTab(layout: LayoutState, stripIndex: number): LayoutState {
  const urlIndex = codeChromeUrlIndex(layout, stripIndex);
  if (urlIndex < 0 || urlIndex === layout.activeIndex) return layout;
  return { ...layout, activeIndex: urlIndex };
}

/** Close the side-region tab at `stripIndex` and keep the terminal in the URL. */
export function closeCodeChromeTab(layout: LayoutState, stripIndex: number): LayoutState {
  const { panels, terminal } = splitCodeChromeLayout(layout);
  const nextPanels = closeLayoutTab(panels, stripIndex);
  return mergeTerminalLayout(nextPanels, terminal);
}

function closeLayoutTab(layout: LayoutState, index: number): LayoutState {
  if (index < 0 || index >= layout.tabs.length) return layout;
  const tabs = layout.tabs.filter((_, at) => at !== index);
  if (tabs.length === 0) return EMPTY_LAYOUT;
  let activeIndex = layout.activeIndex;
  if (index < activeIndex) activeIndex -= 1;
  else if (index === activeIndex) activeIndex = index - 1;
  return {
    ...layout,
    tabs,
    activeIndex: Math.min(Math.max(activeIndex, 0), tabs.length - 1),
  };
}

function mergeTerminalLayout(
  panels: LayoutState,
  terminal: Extract<PanelContent, { type: "terminal" }> | null,
): LayoutState {
  if (!terminal) return panels;
  if (panels.tabs.length === 0) {
    return { tabs: [terminal], activeIndex: 0, fullscreen: false };
  }
  const tabs = [...panels.tabs, terminal];
  const active = panels.tabs[panels.activeIndex];
  const activeIndex = active
    ? tabs.findIndex((tab) => panelKey(tab) === panelKey(active))
    : 0;
  return {
    ...panels,
    tabs,
    activeIndex: Math.max(0, activeIndex),
  };
}

/** Open the terminal drawer, or close it if it is already in the layout. */
export function toggleTerminalLayout(layout: LayoutState): LayoutState {
  const index = layout.tabs.findIndex((tab) => tab.type === "terminal");
  if (index === -1) {
    // Leave the still-visible side tab selected. splitCodeChromeLayout remaps
    // only when a reload or shared link actually named the terminal.
    return {
      ...layout,
      tabs: [...layout.tabs, { type: "terminal" }],
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
