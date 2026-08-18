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
 *
 * Files and Diff live in the inspector. A stale URL tab would duplicate
 * them in the conversation region, so they are stripped here too.
 */
export function splitCodeChromeLayout(layout: LayoutState): {
  panels: LayoutState;
  terminal: Extract<PanelContent, { type: "terminal" }> | null;
} {
  const terminal = layout.tabs.find((tab) => tab.type === "terminal") as
    | Extract<PanelContent, { type: "terminal" }>
    | undefined;
  const tabs = layout.tabs.filter((tab) => !isInspectorOrDrawerTab(tab));
  if (tabs.length === 0) {
    return { panels: EMPTY_LAYOUT, terminal: terminal ?? null };
  }

  const active = layout.tabs[layout.activeIndex];
  let activeIndex = 0;
  if (active && !isInspectorOrDrawerTab(active)) {
    const found = tabs.findIndex((tab) => panelKey(tab) === panelKey(active));
    if (found >= 0) activeIndex = found;
  }

  return {
    panels: {
      tabs,
      activeIndex,
      fullscreen: layout.fullscreen,
    },
    terminal: terminal ?? null,
  };
}

function isInspectorOrDrawerTab(tab: PanelContent): boolean {
  return tab.type === "files" || tab.type === "diff" || tab.type === "terminal";
}

/**
 * URL-tab index of the side-region tab at `stripIndex`.
 *
 * The strip is the URL tabs with the terminal, files, and diff removed, so a
 * click or close on the strip has to skip those rather than treat them as
 * neighbours.
 */
function codeChromeUrlIndex(layout: LayoutState, stripIndex: number): number {
  if (stripIndex < 0) return -1;
  let remaining = stripIndex;
  for (let index = 0; index < layout.tabs.length; index += 1) {
    const tab = layout.tabs[index];
    if (!tab || isInspectorOrDrawerTab(tab)) continue;
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
    // only when a reload or a shared link actually named the terminal.
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
