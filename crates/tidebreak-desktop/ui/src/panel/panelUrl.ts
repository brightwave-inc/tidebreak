import { EMPTY_LAYOUT, panelKey, type LayoutState, type PanelContent } from "./panelTypes";

/**
 * Panels are addressed by the URL, so a layout can be restored, gone back to,
 * and linked at — which is what a citation needs in order to point at a place
 * inside a document rather than just at the app.
 *
 * The grammar is `{type}` or `{type}.{id}`, with one document panel taking a
 * second identifier:
 *
 *   document.{documentId}
 *   document.{documentId}.{citationId}
 *   outputs
 *   outputs.{outputId}
 *   folders
 *   permissions
 *   agents
 *   agent.{runId}
 *
 * Only the first separator picks the panel type; what follows is read by that
 * panel and nothing else. Output navigation carries the durable opaque output
 * identity rather than a display filename. A source identifier is followed by
 * the citation to open it at, when the reader arrived from one.
 *
 * The conversation has no segment: it is beside the region rather than in it,
 * so it is not something the URL has to say.
 */
export function parsePanelSegment(segment: string): PanelContent | null {
  const separator = segment.indexOf(".");
  const type = separator === -1 ? segment : segment.slice(0, separator);
  const id = separator === -1 ? "" : segment.slice(separator + 1);

  switch (type) {
    case "folders":
      return id ? null : { type: "folders" };
    case "permissions":
      return id ? null : { type: "permissions" };
    case "document":
      return parseDocumentTarget(id);
    case "sources":
      // Historical links used `sources.{document}.{citation}`. Preserve those
      // detail links while refusing the retired bare catalog.
      return id ? parseDocumentTarget(id) : null;
    case "outputs":
      return id ? { type: "outputs", outputId: id } : { type: "outputs" };
    case "agents":
      return id ? null : { type: "agents" };
    case "agent":
      // A run id is the whole address; there is no id-less run panel.
      return id ? { type: "agent", runId: id } : null;
    case "terminal":
      return id ? { type: "terminal", terminalId: id } : { type: "terminal" };
    case "file":
      return parseFileTarget(id);
    case "diff":
      return parseDiffTarget(id);
    case "browser":
      return parseBrowserTarget(id);
    default:
      // `apps` and `plugins` were panel segments before the libraries became
      // routes of their own. A bare `files` catalog is still retired.
      return null;
  }
}

function parseFileTarget(id: string): PanelContent | null {
  if (!id) return null;
  try {
    const path = decodeURIComponent(id);
    return path && !path.includes("\0") ? { type: "file", path } : null;
  } catch {
    return null;
  }
}

function parseDiffTarget(id: string): PanelContent | null {
  if (!id) return { type: "diff" };
  const parts = id.split(".");
  if (parts[0] === "t" && parts[1]) {
    const turnId = parts[1];
    if (parts.length === 2) return { type: "diff", turnId };
    if (parts[2] === "f" && parts[3]) {
      try {
        const path = decodeURIComponent(parts.slice(3).join("."));
        return path ? { type: "diff", turnId, path } : null;
      } catch {
        return null;
      }
    }
    return null;
  }
  if (parts[0] === "f" && parts[1]) {
    try {
      const path = decodeURIComponent(parts.slice(1).join("."));
      return path ? { type: "diff", path } : null;
    } catch {
      return null;
    }
  }
  return null;
}

function parseBrowserTarget(id: string): PanelContent | null {
  if (!id) return null;
  try {
    const browserId = decodeURIComponent(id);
    return /^[A-Za-z0-9_-]{1,80}$/.test(browserId)
      ? { type: "browser", browserId }
      : null;
  } catch {
    return null;
  }
}

function parseDocumentTarget(id: string): PanelContent | null {
  if (!id) return null;
  const separator = id.indexOf(".");
  if (separator === -1) return { type: "document", documentId: id };

  const documentId = id.slice(0, separator);
  const citationId = id.slice(separator + 1);
  // A citation is a position inside one document, so neither half addresses
  // anything alone, and a third segment is not part of the grammar.
  if (!documentId || !citationId || citationId.includes(".")) return null;
  return { type: "document", documentId, citationId };
}

export function encodePanelSegment(panel: PanelContent): string {
  switch (panel.type) {
    case "folders":
      return "folders";
    case "permissions":
      return "permissions";
    case "document":
      return panel.citationId
        ? `document.${panel.documentId}.${panel.citationId}`
        : `document.${panel.documentId}`;
    case "outputs":
      return panel.outputId ? `outputs.${panel.outputId}` : "outputs";
    case "agents":
      return "agents";
    case "agent":
      return `agent.${panel.runId}`;
    case "terminal":
      return panel.terminalId ? `terminal.${panel.terminalId}` : "terminal";
    case "file":
      return `file.${encodeURIComponent(panel.path)}`;
    case "diff": {
      if (panel.turnId && panel.path) {
        return `diff.t.${panel.turnId}.f.${encodeURIComponent(panel.path)}`;
      }
      if (panel.turnId) return `diff.t.${panel.turnId}`;
      if (panel.path) return `diff.f.${encodeURIComponent(panel.path)}`;
      return "diff";
    }
    case "browser":
      return `browser.${encodeURIComponent(panel.browserId)}`;
  }
}

export type PanelSearch = {
  /** The open tabs, in strip order, as a comma-separated list of segments. */
  tabs?: string;
  /** The segment of the tab showing; absent means the first one. */
  active?: string;
  /** `"1"` when the region has taken the whole window. */
  fullscreen?: string;
  /** Code workspace: editor tabs in the right editor group. */
  split?: string;
  /** Code workspace: the tab showing in the right editor group. */
  splitActive?: string;
  /** `"1"` when the right editor group most recently received focus. */
  splitFocused?: string;
  /**
   * The retired pair-of-slots grammar. Read on the way in so older links and
   * already-open windows still land somewhere; never written back out.
   */
  left?: string;
  right?: string;
};

const TAB_SEPARATOR = ",";

/**
 * Read a layout out of the URL, falling back to the conversation alone
 * whenever the search params do not describe a usable one. A hand-edited or
 * stale URL should land the reader somewhere sensible rather than on an error.
 */
export function layoutFromSearch(search: PanelSearch): LayoutState {
  const segments =
    search.tabs !== undefined
      ? search.tabs.split(TAB_SEPARATOR)
      : // The old grammar named one panel per side; left came first on screen,
        // so it comes first in the strip. `chat` was a slot filler and parses
        // as nothing, which is what drops it here.
        [search.left, search.right].filter((value): value is string => Boolean(value));

  const tabs: PanelContent[] = [];
  const seen = new Set<string>();
  for (const segment of segments) {
    const panel = parsePanelSegment(segment.trim());
    if (!panel) continue;
    const key = panelKey(panel);
    // A URL naming the same panel twice describes one tab, not two.
    if (seen.has(key)) continue;
    seen.add(key);
    tabs.push(panel);
  }

  const splitTabs: PanelContent[] = [];
  for (const segment of search.split?.split(TAB_SEPARATOR) ?? []) {
    const panel = parsePanelSegment(segment.trim());
    if (
      !panel ||
      (panel.type !== "file" &&
        panel.type !== "diff" &&
        panel.type !== "browser")
    ) {
      continue;
    }
    const key = panelKey(panel);
    if (seen.has(key)) continue;
    seen.add(key);
    splitTabs.push(panel);
  }
  if (tabs.length === 0 && splitTabs.length === 0) return EMPTY_LAYOUT;

  return {
    tabs,
    activeIndex: tabs.length > 0 ? activeIndexFromSearch(search, tabs) : 0,
    fullscreen: isFullscreenParam(search),
    conversationFocused: search.active === "chat" || undefined,
    editorSplit:
      splitTabs.length > 0
        ? {
            tabs: splitTabs,
            activeIndex: namedIndex(search.splitActive, splitTabs),
            focused: search.splitFocused === "1" || undefined,
          }
        : undefined,
  };
}

function namedIndex(segment: string | undefined, tabs: PanelContent[]): number {
  const named = segment ? parsePanelSegment(segment) : null;
  if (!named) return 0;
  const index = tabs.findIndex((tab) => panelKey(tab) === panelKey(named));
  return index < 0 ? 0 : index;
}

function activeIndexFromSearch(search: PanelSearch, tabs: PanelContent[]): number {
  const named = search.active ? parsePanelSegment(search.active) : null;
  if (named) {
    const index = tabs.findIndex((tab) => panelKey(tab) === panelKey(named));
    if (index !== -1) return index;
  }
  // In a legacy link the expanded side is the one the reader was looking at.
  if (search.tabs === undefined && search.fullscreen === "right" && tabs.length > 1) {
    return tabs.length - 1;
  }
  return 0;
}

function isFullscreenParam(search: PanelSearch): boolean {
  if (search.tabs !== undefined) return search.fullscreen === "1";
  // The old grammar named the expanded side. Only a side that actually held a
  // panel survives as fullscreen — an expanded conversation is now just the
  // conversation with nothing beside it.
  const expanded = search.fullscreen === "left" ? search.left : search.right;
  if (search.fullscreen !== "left" && search.fullscreen !== "right") return false;
  return Boolean(expanded && parsePanelSegment(expanded));
}

/**
 * The inverse of {@link layoutFromSearch}. No tabs clears the params entirely,
 * and the retired slot params are always cleared, so a legacy URL is rewritten
 * the first time the layout changes.
 */
export function searchFromLayout(layout: LayoutState): PanelSearch {
  const cleared = { left: undefined, right: undefined };
  const split = layout.editorSplit;
  const splitTabs = split?.tabs ?? [];
  if (layout.tabs.length === 0 && splitTabs.length === 0) {
    return {
      ...cleared,
      tabs: undefined,
      active: undefined,
      fullscreen: undefined,
      split: undefined,
      splitActive: undefined,
      splitFocused: undefined,
    };
  }
  const active = layout.tabs[layout.activeIndex];
  const splitActive = splitTabs[split?.activeIndex ?? 0];
  return {
    ...cleared,
    tabs:
      layout.tabs.length > 0
        ? layout.tabs.map(encodePanelSegment).join(TAB_SEPARATOR)
        : undefined,
    // The first tab is the default, so naming it would only lengthen the URL.
    // `chat` keeps editor tabs open while the conversation is showing.
    active: layout.conversationFocused
      ? "chat"
      : layout.activeIndex > 0 && active
        ? encodePanelSegment(active)
        : undefined,
    fullscreen: layout.fullscreen ? "1" : undefined,
    split:
      splitTabs.length > 0
        ? splitTabs.map(encodePanelSegment).join(TAB_SEPARATOR)
        : undefined,
    splitActive:
      split && split.activeIndex > 0 && splitActive
        ? encodePanelSegment(splitActive)
        : undefined,
    splitFocused: split?.focused ? "1" : undefined,
  };
}
