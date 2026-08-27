export const INSPECTOR_LAYOUT_STORAGE_ID = "code-workspace-inspector-v2";
export const INSPECTOR_PANEL_IDS = ["workspace", "inspector"];

export const DEFAULT_INSPECTOR_LAYOUT = {
  workspace: 70,
  inspector: 30,
};

export const MIN_WORKSPACE_SIZE = 58;
export const MIN_INSPECTOR_SIZE = 22;
export const MAX_INSPECTOR_SIZE = 42;

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
