export const INSPECTOR_LAYOUT_STORAGE_ID = "code-workspace-inspector-v2";
export const INSPECTOR_PANEL_IDS = ["workspace", "inspector"];

export const DEFAULT_INSPECTOR_LAYOUT = {
  workspace: 70,
  inspector: 30,
};

export const MIN_WORKSPACE_SIZE = 58;
export const MIN_INSPECTOR_SIZE = 22;
export const MAX_INSPECTOR_SIZE = 42;

/**
 * The narrowest workspace column that still carries its own furniture: the
 * center tab strip with Source control and Pull request on it, the journal,
 * and the composer with a readable model pill.
 */
export const MIN_WORKSPACE_WIDTH_PX = 480;

/**
 * The narrowest pane worth splitting at all.
 *
 * The bounds above are percentages, so a narrow window shrinks both panels
 * together and no drag can win the workspace its floor back. Below this the
 * inspector stands down and the workspace takes the whole pane; Source
 * control and Pull request stay reachable as center tabs.
 */
export const MIN_INSPECTOR_PANE_WIDTH_PX = Math.ceil(
  (MIN_WORKSPACE_WIDTH_PX * 100) / MIN_WORKSPACE_SIZE,
);

/**
 * Whether a pane of this width can carry the split.
 *
 * An unmeasured pane counts as roomy, so the first paint on a wide window
 * never flashes the inspector away before the observer reports.
 */
export function fitsInspectorSplit(paneWidth: number | null): boolean {
  return paneWidth === null || paneWidth >= MIN_INSPECTOR_PANE_WIDTH_PX;
}

export type InspectorLayout = {
  workspace: number;
  inspector: number;
};

/**
 * Accept only a complete, bounded split that leaves the workspace dominant.
 *
 * The panel library stores arbitrary JSON in local storage. Older payloads,
 * interrupted writes, and layouts saved before the inspector had a maximum
 * size must not be allowed to hide the journal on the next open.
 */
export function usableInspectorLayout(
  value: unknown,
): InspectorLayout | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined;
  }

  const layout = value as Record<string, unknown>;
  const keys = Object.keys(layout);
  if (
    keys.length !== INSPECTOR_PANEL_IDS.length ||
    !INSPECTOR_PANEL_IDS.every((id) => keys.includes(id))
  ) {
    return undefined;
  }

  const workspace = layout.workspace;
  const inspector = layout.inspector;
  if (
    typeof workspace !== "number" ||
    typeof inspector !== "number" ||
    !Number.isFinite(workspace) ||
    !Number.isFinite(inspector) ||
    Math.abs(workspace + inspector - 100) > 0.01 ||
    workspace < MIN_WORKSPACE_SIZE ||
    inspector < MIN_INSPECTOR_SIZE ||
    inspector > MAX_INSPECTOR_SIZE
  ) {
    return undefined;
  }

  return { workspace, inspector };
}
