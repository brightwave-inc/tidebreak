import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import { useLiveResource, type LiveResource } from "./useLiveContent";

export type CodeWorkspacePrMutation =
  | "refresh"
  | "commit"
  | "push"
  | "create_pr"
  | "merge"
  | "auto_merge";

export type CodeWorkspacePrResource = LiveResource<CodeWorkspacePrSnapshot> & {
  busy: CodeWorkspacePrMutation | null;
  mutationError: string | null;
  setMutationError: (error: string | null) => void;
  /** Force a fresh host read through the same lock and generation as writes. */
  refreshFromHost: () => Promise<CodeWorkspacePrSnapshot | undefined>;
  runMutation: <T>(
    mutation: CodeWorkspacePrMutation,
    operation: () => Promise<T>,
  ) => Promise<T | undefined>;
};

/** One shareable live Git/PR snapshot for the header and inspector. */
export function useCodeWorkspacePr(
  client: Pick<ApiClient, "getCodeWorkspacePr"> &
    Partial<Pick<ApiClient, "refreshCodeWorkspacePr">>,
  workspaceId: string,
  contentRevision: number,
  livePr?: PullRequestDigest,
): CodeWorkspacePrResource {
  const [busy, setBusy] = useState<CodeWorkspacePrMutation | null>(null);
  const [mutationError, setMutationError] = useState<string | null>(null);
  const busyRef = useRef<CodeWorkspacePrMutation | null>(null);
  const load = useCallback(
    () => client.getCodeWorkspacePr(workspaceId),
    [client, workspaceId],
  );
  const resource = useLiveResource({
    key: workspaceId,
    revision: contentRevision,
    load,
    errorMessage: "Could not load workspace status",
  });
  const seenLivePrRef = useRef({ workspaceId, pr: livePr });

  useEffect(() => {
    setMutationError(null);
  }, [workspaceId]);

  useEffect(() => {
    const seen = seenLivePrRef.current;
    if (seen.workspaceId !== workspaceId) {
      // The keyed resource is already loading the new workspace.
      seenLivePrRef.current = { workspaceId, pr: livePr };
      return;
    }
    if (seen.pr === livePr) return;
    seenLivePrRef.current = { workspaceId, pr: livePr };
    // A digest is a cheap signal that persisted PR state changed elsewhere.
    // Re-read the complete snapshot so local Git fields and hosted PR fields
    // continue to come from one source of truth.
    void resource.refresh();
  }, [livePr, resource.refresh, workspaceId]);

  const runMutation = useCallback(
    async <T>(
      mutation: CodeWorkspacePrMutation,
      operation: () => Promise<T>,
    ): Promise<T | undefined> => {
      if (busyRef.current !== null) return undefined;
      busyRef.current = mutation;
      setBusy(mutation);
      setMutationError(null);
      try {
        return await operation();
      } finally {
        busyRef.current = null;
        setBusy(null);
      }
    },
    [],
  );

  const refreshFromHost = useCallback(
    () =>
      runMutation("refresh", async () => {
        const next = client.refreshCodeWorkspacePr
          ? await client.refreshCodeWorkspacePr(workspaceId)
          : await client.getCodeWorkspacePr(workspaceId);
        // Adopt inside the serialized operation. Besides keeping the lock held
        // until the state is visible, adopt retires any older passive read so
        // it cannot land after this fresh host snapshot.
        resource.adopt(next);
        return next;
      }),
    [client, resource.adopt, runMutation, workspaceId],
  );

  return {
    ...resource,
    busy,
    mutationError,
    setMutationError,
    refreshFromHost,
    runMutation,
  };
}
