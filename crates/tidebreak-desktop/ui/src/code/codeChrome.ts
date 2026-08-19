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
  splitEditors: LayoutState;
  terminal: Extract<PanelContent, { type: "terminal" }> | null;
} {
  const terminal = layout.tabs.find((tab) => tab.type === "terminal") as
    | Extract<PanelContent, { type: "terminal" }>
    | undefined;
  const active = layout.tabs[layout.activeIndex];
  const side = layout.tabs.filter((tab) => !isDrawerTab(tab) && !isEditorTab(tab));
  const editors = layout.tabs.filter(isEditorTab);
  const splitTabs = layout.editorSplit?.tabs.filter(isEditorTab) ?? [];
  const splitActive = layout.editorSplit?.tabs[layout.editorSplit.activeIndex];

  const primaryEditors = sliceLayout(editors, active, false);
  return {
    panels: sliceLayout(side, active, layout.fullscreen),
    editors:
      layout.conversationFocused === undefined
        ? primaryEditors
        : {
            ...primaryEditors,
            conversationFocused: layout.conversationFocused,
          },
    splitEditors: sliceLayout(splitTabs, splitActive, false),
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

export type CodeEditorRegion = "primary" | "secondary";

/** Show the conversation while keeping file and diff tabs open. */
export function focusConversation(layout: LayoutState): LayoutState {
  return {
    ...layout,
    conversationFocused: true,
    editorSplit: layout.editorSplit
      ? { ...layout.editorSplit, focused: undefined }
      : undefined,
  };
}

/** Bring a file or diff tab forward. `editorIndex` counts only those tabs. */
export function focusEditorTab(
  layout: LayoutState,
  editorIndex: number,
  region: CodeEditorRegion = "primary",
): LayoutState {
  if (region === "secondary") {
    const split = layout.editorSplit;
    if (!split?.tabs[editorIndex]) return layout;
    return {
      ...layout,
      editorSplit: { ...split, activeIndex: editorIndex, focused: true },
    };
  }
  const editors = layout.tabs.filter(isEditorTab);
  const target = editors[editorIndex];
  if (!target) return layout;
  const index = layout.tabs.findIndex((tab) => panelKey(tab) === panelKey(target));
  if (index < 0) return layout;
  return {
    ...layout,
    activeIndex: index,
    conversationFocused: false,
    editorSplit: layout.editorSplit
      ? { ...layout.editorSplit, focused: undefined }
      : undefined,
  };
}

/** Close a file or diff tab. `editorIndex` counts only those tabs. */
export function closeEditorTab(
  layout: LayoutState,
  editorIndex: number,
  region: CodeEditorRegion = "primary",
): LayoutState {
  if (region === "secondary") {
    const split = layout.editorSplit;
    if (!split || editorIndex < 0 || editorIndex >= split.tabs.length) {
      return layout;
    }
    const tabs = split.tabs.filter((_, index) => index !== editorIndex);
    if (tabs.length === 0) return { ...layout, editorSplit: undefined };
    let activeIndex = split.activeIndex;
    if (editorIndex < activeIndex) activeIndex -= 1;
    else if (editorIndex === activeIndex) activeIndex = editorIndex - 1;
    return {
      ...layout,
      editorSplit: {
        ...split,
        tabs,
        activeIndex: Math.min(Math.max(activeIndex, 0), tabs.length - 1),
      },
    };
  }
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
 * Close every file and diff tab in one editor group. With no region, close
 * both groups and return the center to the conversation (the Main agent menu).
 */
export function closeAllEditorTabs(
  layout: LayoutState,
  region?: CodeEditorRegion,
): LayoutState {
  if (region === "secondary") {
    return layout.editorSplit ? { ...layout, editorSplit: undefined } : layout;
  }

  const tabs = layout.tabs.filter((tab) => !isEditorTab(tab));
  const closeSplit = region === undefined;
  if (
    tabs.length === layout.tabs.length &&
    (!closeSplit || !layout.editorSplit)
  ) {
    return layout;
  }
  if (tabs.length === 0) {
    return closeSplit || !layout.editorSplit
      ? { ...EMPTY_LAYOUT, editorSplit: undefined }
      : {
          ...layout,
          tabs: [],
          activeIndex: 0,
          fullscreen: false,
          conversationFocused: undefined,
        };
  }

  const active = layout.tabs[layout.activeIndex];
  const activeIndex = active && !isEditorTab(active)
    ? Math.max(0, tabs.findIndex((tab) => panelKey(tab) === panelKey(active)))
    : 0;
  return {
    ...layout,
    tabs,
    activeIndex,
    conversationFocused: undefined,
    editorSplit: closeSplit ? undefined : layout.editorSplit,
  };
}

/** Keep one file or diff tab and close the other editor tabs around it. */
export function closeOtherEditorTabs(
  layout: LayoutState,
  editorIndex: number,
  region: CodeEditorRegion = "primary",
): LayoutState {
  if (region === "secondary") {
    const target = layout.editorSplit?.tabs[editorIndex];
    if (!target) return layout;
    return {
      ...layout,
      editorSplit: { tabs: [target], activeIndex: 0, focused: true },
    };
  }
  const editors = layout.tabs.filter(isEditorTab);
  const target = editors[editorIndex];
  if (!target || editors.length <= 1) return layout;
  const targetKey = panelKey(target);
  const tabs = layout.tabs.filter(
    (tab) => !isEditorTab(tab) || panelKey(tab) === targetKey,
  );
  return {
    ...layout,
    tabs,
    activeIndex: tabs.findIndex((tab) => panelKey(tab) === targetKey),
    conversationFocused: false,
    editorSplit: layout.editorSplit
      ? { ...layout.editorSplit, focused: undefined }
      : undefined,
  };
}

/** Close only the file and diff tabs that follow `editorIndex`. */
export function closeEditorTabsToRight(
  layout: LayoutState,
  editorIndex: number,
  region: CodeEditorRegion = "primary",
): LayoutState {
  if (region === "secondary") {
    const split = layout.editorSplit;
    if (!split || editorIndex >= split.tabs.length - 1) return layout;
    const tabs = split.tabs.slice(0, editorIndex + 1);
    return {
      ...layout,
      editorSplit: {
        ...split,
        tabs,
        activeIndex: Math.min(split.activeIndex, tabs.length - 1),
      },
    };
  }
  const editors = layout.tabs.filter(isEditorTab);
  const target = editors[editorIndex];
  const closing = editors.slice(editorIndex + 1);
  if (!target || closing.length === 0) return layout;

  const closingKeys = new Set(closing.map(panelKey));
  const tabs = layout.tabs.filter(
    (tab) => !isEditorTab(tab) || !closingKeys.has(panelKey(tab)),
  );
  const active = layout.tabs[layout.activeIndex];
  const activeKey = active ? panelKey(active) : null;
  const activeIndex = activeKey && !closingKeys.has(activeKey)
    ? tabs.findIndex((tab) => panelKey(tab) === activeKey)
    : tabs.findIndex((tab) => panelKey(tab) === panelKey(target));
  return {
    ...layout,
    tabs,
    activeIndex: Math.max(activeIndex, 0),
  };
}

/** Open one editor in the group that last had focus, unless a group is named. */
export function openCodeEditor(
  layout: LayoutState,
  panel: Extract<PanelContent, { type: "file" | "diff" }>,
  preferredRegion?: CodeEditorRegion,
): LayoutState {
  const key = panelKey(panel);
  const primaryIndex = layout.tabs
    .filter(isEditorTab)
    .findIndex((tab) => panelKey(tab) === key);
  if (primaryIndex >= 0) {
    const urlIndex = editorUrlIndex(layout, primaryIndex);
    const tabs = layout.tabs.slice();
    tabs[urlIndex] = panel;
    return focusEditorTab({ ...layout, tabs }, primaryIndex, "primary");
  }
  const splitIndex = layout.editorSplit?.tabs.findIndex(
    (tab) => panelKey(tab) === key,
  ) ?? -1;
  if (splitIndex >= 0 && layout.editorSplit) {
    const tabs = layout.editorSplit.tabs.slice();
    tabs[splitIndex] = panel;
    return focusEditorTab(
      { ...layout, editorSplit: { ...layout.editorSplit, tabs } },
      splitIndex,
      "secondary",
    );
  }

  const region =
    preferredRegion ?? (layout.editorSplit?.focused ? "secondary" : "primary");
  if (region === "secondary") {
    const split = layout.editorSplit ?? { tabs: [], activeIndex: 0 };
    return {
      ...layout,
      editorSplit: {
        tabs: [...split.tabs, panel],
        activeIndex: split.tabs.length,
        focused: true,
      },
    };
  }
  const editors = layout.tabs.filter(isEditorTab);
  return focusEditorTab(
    { ...layout, tabs: [...layout.tabs, panel] },
    editors.length,
    "primary",
  );
}

/** Move an existing file/diff tab between the two visible editor groups. */
export function moveEditorTab(
  layout: LayoutState,
  from: CodeEditorRegion,
  editorIndex: number,
  to: CodeEditorRegion,
): LayoutState {
  if (from === to) return layout;
  const target =
    from === "primary"
      ? layout.tabs.filter(isEditorTab)[editorIndex]
      : layout.editorSplit?.tabs[editorIndex];
  if (!target) return layout;

  if (from === "primary") {
    const without = closeEditorTab(layout, editorIndex, "primary");
    const split = without.editorSplit ?? { tabs: [], activeIndex: 0 };
    const existing = split.tabs.findIndex(
      (tab) => panelKey(tab) === panelKey(target),
    );
    const tabs = existing >= 0 ? split.tabs : [...split.tabs, target];
    return {
      ...without,
      editorSplit: {
        tabs,
        activeIndex: existing >= 0 ? existing : tabs.length - 1,
        focused: true,
      },
    };
  }

  const without = closeEditorTab(layout, editorIndex, "secondary");
  return openCodeEditor(
    without,
    target as Extract<PanelContent, { type: "file" | "diff" }>,
    "primary",
  );
}

/** Collapse the right group without closing its tabs. */
export function mergeEditorSplit(layout: LayoutState): LayoutState {
  const splitTabs = layout.editorSplit?.tabs.filter(isEditorTab) ?? [];
  if (splitTabs.length === 0) return { ...layout, editorSplit: undefined };
  let next: LayoutState = { ...layout, editorSplit: undefined };
  for (const tab of splitTabs) {
    next = openCodeEditor(
      next,
      tab as Extract<PanelContent, { type: "file" | "diff" }>,
      "primary",
    );
  }
  return next;
}

function editorUrlIndex(layout: LayoutState, editorIndex: number): number {
  let remaining = editorIndex;
  for (let index = 0; index < layout.tabs.length; index += 1) {
    const tab = layout.tabs[index];
    if (!tab || !isEditorTab(tab)) continue;
    if (remaining === 0) return index;
    remaining -= 1;
  }
  return -1;
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
  return combineChrome(nextPanels, editors, terminal, layout.editorSplit);
}

function combineChrome(
  panels: LayoutState,
  editors: LayoutState,
  terminal: Extract<PanelContent, { type: "terminal" }> | null,
  editorSplit?: LayoutState["editorSplit"],
): LayoutState {
  const tabs = [...editors.tabs, ...panels.tabs];
  if (terminal) tabs.push(terminal);
  if (tabs.length === 0 && !editorSplit) return EMPTY_LAYOUT;
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
    editorSplit,
  };
}

function closeLayoutTab(layout: LayoutState, index: number): LayoutState {
  if (index < 0 || index >= layout.tabs.length) return layout;
  const tabs = layout.tabs.filter((_, at) => at !== index);
  if (tabs.length === 0) {
    return {
      ...layout,
      tabs: [],
      activeIndex: 0,
      fullscreen: false,
      conversationFocused: undefined,
    };
  }
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
  if (tabs.length === 0) {
    return layout.editorSplit
      ? {
          ...layout,
          tabs: [],
          activeIndex: 0,
          fullscreen: false,
          conversationFocused: undefined,
        }
      : EMPTY_LAYOUT;
  }

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
