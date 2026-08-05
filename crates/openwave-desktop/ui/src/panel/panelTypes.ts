/**
 * The workspace is the conversation and, beside it, a region of open panels.
 * The conversation is always there — it is the frame, not a panel — and
 * everything else opens as a tab in the region to its right.
 */
export type PanelContent =
  /**
   * `citationId` is where in the document to open: the citation it names
   * carries the cited span and page. It only means anything alongside the
   * document it points into, so it is never set without `documentId`.
   */
  | { type: "document"; documentId: string; citationId?: string }
  | { type: "outputs"; outputId?: string }
  | { type: "folders" }
  /** The Apps library; an app id turns the list into that app's detail. */
  | { type: "apps"; appId?: string }
  /**
   * The Plugins library; a plugin slug turns the list into that bundle's
   * detail, with its member skills and their own switches.
   */
  | { type: "plugins"; pluginId?: string }
  /**
   * One background agent run, opened from its row in the transcript's agent
   * list. There is no bare agent catalog — the transcript is the list — so the
   * run id is always present.
   */
  | { type: "agent"; runId: string };

export type PanelType = PanelContent["type"];

/**
 * The open panels, which one is showing, and whether the region has taken the
 * whole window. No tabs is the bare URL: the conversation alone.
 */
export type LayoutState = {
  tabs: PanelContent[];
  activeIndex: number;
  fullscreen: boolean;
};

export const EMPTY_LAYOUT: LayoutState = { tabs: [], activeIndex: 0, fullscreen: false };

/**
 * What makes two panels the same tab.
 *
 * A library and the item drilled into from it are one panel showing something
 * else, not two — the Apps list and an app's detail share a tab, and clicking
 * Folders again lands on the tab already open. Documents and agent runs are
 * addressed by identity instead, so two documents are two tabs while
 * re-opening one at a different citation moves within the tab it already has.
 */
export function panelKey(panel: PanelContent): string {
  switch (panel.type) {
    case "document":
      return `document:${panel.documentId}`;
    case "agent":
      return `agent:${panel.runId}`;
    default:
      return panel.type;
  }
}

/** The panel currently showing in the region, or `null` when nothing is open. */
export function activePanel(layout: LayoutState): PanelContent | null {
  return layout.tabs[layout.activeIndex] ?? null;
}
