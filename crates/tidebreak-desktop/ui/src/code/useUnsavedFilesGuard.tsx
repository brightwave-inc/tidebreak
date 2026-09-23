import { useCallback, useEffect, type ReactElement } from "react";
import { useBlocker } from "@tanstack/react-router";

import { useConfirm } from "@/components/ConfirmDialog";
import type { LayoutState } from "@/panel/panelTypes";
import { layoutFromSearch, panelSearchFrom } from "@/panel/panelUrl";
import {
  dirtyCodeFilePaths,
  discardUnsavedTitle,
  useCodeFileDraftStore,
  useDirtyCodeFilePaths,
} from "./CodeFileDraftStore";
import { codeWorkspaceIdFromPath } from "./routes";

/** Every file open as a tab, in either editor group. */
export function openFilePaths(layout: LayoutState): Set<string> {
  const paths = new Set<string>();
  for (const tab of [...layout.tabs, ...(layout.editorSplit?.tabs ?? [])]) {
    if (tab.type === "file") paths.add(tab.path);
  }
  return paths;
}

/**
 * Which files with unsaved changes a navigation would close: all of them
 * when it leaves the workspace, and otherwise the ones whose tabs the next
 * layout no longer has open. The layout lives in the URL, so every way a tab
 * closes — its close button, Cmd+W, the tab menu, back — is a navigation.
 */
export function unsavedFilesClosedBy(
  workspaceId: string,
  dirtyPaths: readonly string[],
  next: { pathname: string; search: unknown },
): string[] {
  if (dirtyPaths.length === 0) return [];
  if (codeWorkspaceIdFromPath(next.pathname) !== workspaceId) {
    return [...dirtyPaths];
  }
  const search =
    typeof next.search === "object" && next.search !== null
      ? (next.search as Record<string, unknown>)
      : {};
  const open = openFilePaths(layoutFromSearch(panelSearchFrom(search)));
  return dirtyPaths.filter((path) => !open.has(path));
}

/**
 * Ask before a navigation throws unsaved file changes away, and before a
 * browser reloads or closes the page with some.
 *
 * The blocker only exists while the workspace has unsaved changes, so every
 * other navigation stays as fast as it was. Returns the confirmation dialog
 * for the page to render.
 */
export function useUnsavedFilesGuard(
  workspaceId: string,
  layout: LayoutState,
): ReactElement {
  const { confirm, dialog } = useConfirm();
  const dirty = useDirtyCodeFilePaths(workspaceId);

  const shouldBlockFn = useCallback(
    async ({ next }: { next: { pathname: string; search: unknown } }) => {
      const closing = unsavedFilesClosedBy(
        workspaceId,
        dirtyCodeFilePaths(useCodeFileDraftStore.getState(), workspaceId),
        next,
      );
      if (closing.length === 0) return false;
      const discard = await confirm({
        title: discardUnsavedTitle(closing),
        description: "Your edits since the last save are lost.",
        confirmLabel: "Discard",
        destructive: true,
      });
      if (!discard) return true;
      useCodeFileDraftStore.getState().discard(workspaceId, closing);
      return false;
    },
    [confirm, workspaceId],
  );
  // Read when the page is about to unload, not when this rendered: a reload
  // you already confirmed has dropped the drafts by then.
  const enableBeforeUnload = useCallback(
    () =>
      dirtyCodeFilePaths(useCodeFileDraftStore.getState(), workspaceId).length >
      0,
    [workspaceId],
  );
  useBlocker({
    shouldBlockFn,
    enableBeforeUnload,
    disabled: dirty.size === 0,
  });

  // A draft with nothing unsaved whose tab closed is only edit mode left
  // behind; drop it so the file opens read-only next time.
  const openKey = [...openFilePaths(layout)].sort().join("\u0000");
  useEffect(() => {
    useCodeFileDraftStore
      .getState()
      .pruneClean(workspaceId, new Set(openKey ? openKey.split("\u0000") : []));
  }, [workspaceId, openKey]);
  useEffect(
    () => () => {
      useCodeFileDraftStore.getState().pruneClean(workspaceId, new Set());
    },
    [workspaceId],
  );

  return dialog;
}
