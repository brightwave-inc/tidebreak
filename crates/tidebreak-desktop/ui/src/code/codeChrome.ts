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
  editors: LayoutState;
  terminal: Extract<PanelContent, { type: "terminal" }> | null;
} {
  const terminal = layout.tabs.find((tab) => tab.type === "terminal") as
    | Extract<PanelContent, { type: "terminal" }>
    | undefined;
  const active = layout.tabs[layout.activeIndex];
  const side = layout.tabs.filter((tab) => !isDrawerTab(tab) && !isEditorTab(tab));
  const editors = layout.tabs.filter(isEditorTab);

  return {
    panels: sliceLayout(side, active, layout.fullscreen),
    editors: {
      ...sliceLayout(editors, active, false),
      conversationFocused: layout.conversationFocused,
    },
    terminal: terminal ?? null,
  };
}

function sliceLayout(
  tabs: PanelContent[],
  active: PanelContent | undefined,
  fullscreen: boolean,
): LayoutState {
  if (tabs.length === 0) return EMPTY_LAYOUT;
  let activeIndex = 0;
  if (active) {
    const found = tabs.findIndex((tab) => panelKey(tab) === panelKey(active));
    if (found >= 0) activeIndex = found;
  }
  return { tabs, activeIndex, fullscreen };
}

function isDrawerTab(tab: PanelContent): boolean {
  return tab.type === "terminal";
}

export function isEditorTab(tab: PanelContent): boolean {
  return tab.type === "file" || tab.type === "diff";
}

/** Show the conversation while keeping file and diff tabs open. */
export function focusConversation(layout: LayoutState): LayoutState {
  return { ...layout, conversationFocused: true };
}

/** Bring a file or diff tab forward. `editorIndex` counts only those tabs. */
export function focusEditorTab(layout: LayoutState, editorIndex: number): LayoutState {
  const editors = layout.tabs.filter(isEditorTab);
  const target = editors[editorIndex];
  if (!target) return layout;
  const index = layout.tabs.findIndex((tab) => panelKey(tab) === panelKey(target));
  if (index < 0) return layout;
  return { ...layout, activeIndex: index, conversationFocused: false };
}

/** Close a file or diff tab. `editorIndex` counts only those tabs. */
export function closeEditorTab(layout: LayoutState, editorIndex: number): LayoutState {
  const editors = layout.tabs.filter(isEditorTab);
  const target = editors[editorIndex];
  if (!target) return layout;
  const index = layout.tabs.findIndex((tab) => panelKey(tab) === panelKey(target));
  if (index < 0) return layout;
  const next = closeLayoutTab(layout, index);
  if (next.tabs.filter(isEditorTab).length === 0) {
    return { ...next, conversationFocused: undefined };
  }
  return next;
}

/**
 * URL-tab index of the side-region tab at `stripIndex`.
 *
 * The strip is the URL tabs with the terminal removed, so a click or close on
 * the strip has to skip it rather than treat it as a neighbour.
 */
function codeChromeUrlIndex(layout: LayoutState, stripIndex: number): number {
  if (stripIndex < 0) return -1;
  let remaining = stripIndex;
  for (let index = 0; index < layout.tabs.length; index += 1) {
    const tab = layout.tabs[index];
    if (!tab || isDrawerTab(tab) || isEditorTab(tab)) continue;
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
  const { panels, editors, terminal } = splitCodeChromeLayout(layout);
  const nextPanels = closeLayoutTab(panels, stripIndex);
  return combineChrome(nextPanels, editors, terminal);
}

function combineChrome(
  panels: LayoutState,
  editors: LayoutState,
  terminal: Extract<PanelContent, { type: "terminal" }> | null,
): LayoutState {
  const tabs = [...editors.tabs, ...panels.tabs];
  if (terminal) tabs.push(terminal);
  if (tabs.length === 0) return EMPTY_LAYOUT;
  const focused =
    !editors.conversationFocused && editors.tabs[editors.activeIndex]
      ? editors.tabs[editors.activeIndex]
      : panels.tabs[panels.activeIndex];
  const activeIndex = focused
    ? Math.max(0, tabs.findIndex((tab) => panelKey(tab) === panelKey(focused)))
    : 0;
  return {
    tabs,
    activeIndex,
    fullscreen: panels.fullscreen && panels.tabs.length > 0,
    conversationFocused: editors.conversationFocused,
  };
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
