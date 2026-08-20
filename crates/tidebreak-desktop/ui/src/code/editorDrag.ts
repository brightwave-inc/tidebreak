import {
  panelKey,
  type LayoutState,
  type PanelContent,
} from "@/panel/panelTypes";

import {
  isEditorTab,
  moveEditorTab,
  reorderEditorTab,
  splitCodeChromeLayout,
  type CodeEditorRegion,
} from "./codeChrome";

/**
 * Identity and drop rules for dragging editor tabs.
 *
 * dnd-kit addresses everything by a single string id, so the ids are the whole
 * protocol: one per tab, one per strip, and one for the split zone. A tab's id
 * carries its region and its panel key rather than its position, because a
 * position changes under the drag and a key does not.
 *
 * Nothing here touches `dataTransfer`. Native HTML5 drag never started reliably
 * in the desktop webview — the pointer path replaces it — so a drop no longer
 * has to survive a serialization round trip.
 */

const TAB_PREFIX = "editor-tab:";
const STRIP_PREFIX = "editor-strip:";

/** The zone that appears mid-drag, offering to open the tab beside the chat. */
export const EDITOR_SPLIT_DROP_ID = "editor-split-zone";

/** The id of a tab, stable while it moves. Panel keys may hold colons. */
export function editorTabDragId(
  region: CodeEditorRegion,
  panel: PanelContent,
): string {
  return `${TAB_PREFIX}${region}:${panelKey(panel)}`;
}

/** The id of a whole strip, so a drop on its empty space still lands. */
export function editorStripDropId(region: CodeEditorRegion): string {
  return `${STRIP_PREFIX}${region}`;
}

/** Whether an id names a whole strip rather than one tab or the split zone. */
export function isEditorStripDropId(id: string): boolean {
  return id.startsWith(STRIP_PREFIX);
}

/** The region an id belongs to, or null when the id is not one of ours. */
export function editorDragRegion(id: string): CodeEditorRegion | null {
  const rest = id.startsWith(TAB_PREFIX)
    ? id.slice(TAB_PREFIX.length)
    : id.startsWith(STRIP_PREFIX)
      ? id.slice(STRIP_PREFIX.length)
      : null;
  if (rest === null) return null;
  if (rest === "primary" || rest.startsWith("primary:")) return "primary";
  if (rest === "secondary" || rest.startsWith("secondary:")) return "secondary";
  return null;
}

/** The panel key inside a tab id, or null for a strip or the split zone. */
function editorDragKey(id: string): string | null {
  if (!id.startsWith(TAB_PREFIX)) return null;
  const rest = id.slice(TAB_PREFIX.length);
  const at = rest.indexOf(":");
  return at < 0 ? null : rest.slice(at + 1);
}

/** Which tabs a region draws, in the order the strip draws them. */
function regionTabs(
  layout: LayoutState,
  region: CodeEditorRegion,
): PanelContent[] {
  return region === "primary"
    ? layout.tabs.filter(isEditorTab)
    : [...(layout.editorSplit?.tabs ?? [])];
}

function indexOfKey(tabs: readonly PanelContent[], key: string): number {
  return tabs.findIndex((tab) => panelKey(tab) === key);
}

/**
 * The layout a finished drag produces, or null when it changes nothing.
 *
 * Null covers every way a drag can end without a move: released over open air,
 * dropped back where it started, or aimed at a tab that closed underneath it.
 * The caller treats null as "leave the layout alone" rather than as an error,
 * because from the reader's side those are all the same gesture.
 */
export function dropEditorTab(
  layout: LayoutState,
  activeId: string,
  overId: string | null,
): LayoutState | null {
  if (!overId || activeId === overId) return null;
  const from = editorDragRegion(activeId);
  const key = editorDragKey(activeId);
  if (!from || key === null) return null;
  const fromIndex = indexOfKey(regionTabs(layout, from), key);
  if (fromIndex < 0) return null;

  if (overId === EDITOR_SPLIT_DROP_ID) {
    if (from === "secondary") return null;
    return moveEditorTab(layout, from, fromIndex, "secondary");
  }

  const to = editorDragRegion(overId);
  if (!to) return null;

  if (to !== from) {
    // Across groups the tab joins the end of the other strip. dnd-kit knows
    // which tab the pointer is over, but a cross-group drop reads as "put this
    // over there" rather than as an ordering, and the reader can drag again to
    // place it.
    return moveEditorTab(layout, from, fromIndex, to);
  }

  const tabs = regionTabs(layout, to);
  const overKey = editorDragKey(overId);
  // A drop with no tab under it landed on the strip itself, which within a
  // group means the open space past the last tab.
  const toIndex =
    overKey === null ? tabs.length - 1 : indexOfKey(tabs, overKey);
  if (toIndex < 0 || toIndex === fromIndex) return null;
  return reorderEditorTab(layout, from, fromIndex, toIndex);
}

/**
 * Whether the split zone should be offered for the tab being dragged.
 *
 * Only a left-group tab can create the split, and only while there is no split
 * to create — once the right group exists, that group is itself the target.
 */
export function offersSplitDrop(
  layout: LayoutState,
  activeId: string,
): boolean {
  if (editorDragRegion(activeId) !== "primary") return false;
  return splitCodeChromeLayout(layout).splitEditors.tabs.length === 0;
}

/**
 * The tab an id names, or null once that tab has closed under the drag.
 *
 * The overlay needs the panel itself, not its position, so that the ghost keeps
 * drawing the right label while the tabs beneath it shuffle.
 */
export function findEditorPanel(
  layout: LayoutState,
  id: string,
): PanelContent | null {
  const region = editorDragRegion(id);
  const key = editorDragKey(id);
  if (!region || key === null) return null;
  const at = indexOfKey(regionTabs(layout, region), key);
  return at < 0 ? null : (regionTabs(layout, region)[at] ?? null);
}
