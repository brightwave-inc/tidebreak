import { useEffect } from "react";
import { useCodeUiStore } from "../CodeUiStore";
import { useCodeUpdatesStore } from "../CodeUpdatesStore";

/**
 * Mark a workspace as the one on screen while its page is mounted. When the
 * reader leaves, reset the inspector's turn scope and release any composer
 * action the workspace still holds.
 */
export function useViewedWorkspace(workspaceId: string) {
  const setViewedWorkspace = useCodeUpdatesStore(
    (state) => state.setViewedWorkspace,
  );

  useEffect(() => {
    setViewedWorkspace(workspaceId);
    return () => setViewedWorkspace(null);
  }, [setViewedWorkspace, workspaceId]);

  useEffect(() => {
    return () => {
      useCodeUiStore.getState().setInspectorScope(null);
      useCodeUiStore.getState().finishComposerAction(workspaceId);
    };
  }, [workspaceId]);
}
