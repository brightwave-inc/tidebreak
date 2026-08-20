/**
 * The workspace is the conversation and, beside it, a region of open panels.
 * The conversation is always there — it is the frame, not a panel — and
 * everything else opens as a tab in the region to its right.
 *
 * Every panel here is scoped to the conversation beside it. Install-wide
 * surfaces — the Apps and Plugins libraries — are routes with the rail, not
 * tabs: they outlive every conversation, so they take the whole pane rather
 * than a slot beside one.
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
  /** Standing approvals that reach this conversation. */
  | { type: "permissions" }
  /** This conversation's background agents, as a table of runs. */
  | { type: "agents" }
  /** One background agent run, opened from the agents table or the transcript. */
  | { type: "agent"; runId: string }
  /** Auxiliary workspace shell; `terminalId` pins a live PTY when known. */
  | { type: "terminal"; terminalId?: string }
  /** One worktree file in the workspace center. */
  | { type: "file"; path: string }
  /** A colored diff in the workspace center. */
  | { type: "diff"; path?: string; turnId?: string }
  /** One native browser session in the workspace center. */
  | { type: "browser"; browserId: string }
  /** The change index plus commit, push, and PR creation, as a peer tab. */
  | { type: "source_control" }
  /** The pull request's own life: status, checks, and review comments. */
  | { type: "pr" };

export type PanelType = PanelContent["type"];

/**
 * The open panels, which one is showing, and whether the region has taken the
 * whole window. No tabs is the bare URL: the conversation alone.
 */
export type LayoutState = {
  tabs: PanelContent[];
  activeIndex: number;
  fullscreen: boolean;
  /**
   * Code workspace: the conversation tab is selected while editor tabs
   * stay open. Absent everywhere else.
   */
  conversationFocused?: boolean;
  /**
   * Code workspace: a second editor group to the right of the main-agent
   * group. The transcript itself never moves or mounts twice; only editor
   * tabs are stored here.
   */
  editorSplit?: {
    tabs: PanelContent[];
    activeIndex: number;
    /** The second group received the most recent explicit focus. */
    focused?: boolean;
  };
};

export const EMPTY_LAYOUT: LayoutState = { tabs: [], activeIndex: 0, fullscreen: false };

/**
 * What makes two panels the same tab.
 *
 * A library and the item drilled into from it are one panel showing something
 * else, not two — the agents table and clicking Folders again land on the tab
 * already open. Documents and agent runs are addressed by identity instead, so
 * two documents are two tabs while re-opening one at a different citation
 * moves within the tab it already has.
 */
export function panelKey(panel: PanelContent): string {
  switch (panel.type) {
    case "document":
      return `document:${panel.documentId}`;
    case "agent":
      return `agent:${panel.runId}`;
    case "file":
      return `file:${panel.path}`;
    case "diff":
      return `diff:${panel.turnId ?? ""}:${panel.path ?? ""}`;
    case "browser":
      return `browser:${panel.browserId}`;
    default:
      return panel.type;
  }
}

/** The panel currently showing in the region, or `null` when nothing is open. */
export function activePanel(layout: LayoutState): PanelContent | null {
  return layout.tabs[layout.activeIndex] ?? null;
}
