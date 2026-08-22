import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import { useLiveResource, type LiveResource } from "./useLiveContent";

export type CodeWorkspacePrMutation =
  | "refresh"
  | "commit"
  | "push"
  | "create_pr"
  | "mark_ready"
  | "merge"
  | "auto_merge"
  | "watch"
  | "stop_watch";

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
  const busyRef = useRef<{
    generation: number;
    mutation: CodeWorkspacePrMutation;
  } | null>(null);
  const workspaceRef = useRef(workspaceId);
  const workspaceGenerationRef = useRef(0);
  if (workspaceRef.current !== workspaceId) {
    workspaceRef.current = workspaceId;
    workspaceGenerationRef.current += 1;
  }
  const workspaceGeneration = workspaceGenerationRef.current;
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
  const livePrSignature = pullRequestDigestSignature(livePr);
  const seenLivePrRef = useRef({ workspaceId, signature: livePrSignature });

  useEffect(() => {
    busyRef.current = null;
    setBusy(null);
    setMutationError(null);
  }, [workspaceId]);

  useEffect(() => {
    if (!resource.refreshing && resource.error === null && resource.data) {
      setMutationError(null);
    }
  }, [resource.data, resource.error, resource.refreshing]);

  useEffect(() => {
    const seen = seenLivePrRef.current;
    if (seen.workspaceId !== workspaceId) {
      // The keyed resource is already loading the new workspace.
      seenLivePrRef.current = { workspaceId, signature: livePrSignature };
      return;
    }
    if (seen.signature === livePrSignature) return;
    seenLivePrRef.current = { workspaceId, signature: livePrSignature };
    // A digest is a cheap signal that persisted PR state changed elsewhere.
    // Re-read the complete snapshot so local Git fields and hosted PR fields
    // continue to come from one source of truth.
    void resource.refresh();
  }, [livePrSignature, resource.refresh, workspaceId]);

  const adopt = useCallback(
    (value: CodeWorkspacePrSnapshot) => {
      if (workspaceGeneration !== workspaceGenerationRef.current) return;
      resource.adopt(value);
      setMutationError(null);
    },
    [resource.adopt, workspaceGeneration],
  );
  const setBoundMutationError = useCallback(
    (error: string | null) => {
      if (workspaceGeneration !== workspaceGenerationRef.current) return;
      setMutationError(error);
    },
    [workspaceGeneration],
  );

  const runMutation = useCallback(
    async <T>(
      mutation: CodeWorkspacePrMutation,
      operation: () => Promise<T>,
    ): Promise<T | undefined> => {
      if (workspaceGeneration !== workspaceGenerationRef.current) {
        return undefined;
      }
      if (busyRef.current?.generation === workspaceGeneration) {
        return undefined;
      }
      busyRef.current = { generation: workspaceGeneration, mutation };
      setBusy(mutation);
      setMutationError(null);
      try {
        return await operation();
      } finally {
        if (
          workspaceGeneration === workspaceGenerationRef.current &&
          busyRef.current?.generation === workspaceGeneration
        ) {
          busyRef.current = null;
          setBusy(null);
        }
      }
    },
    [workspaceGeneration],
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
        adopt(next);
        return next;
      }),
    [adopt, client, runMutation, workspaceId],
  );

  return {
    ...resource,
    adopt,
    busy,
    mutationError,
    setMutationError: setBoundMutationError,
    refreshFromHost,
    runMutation,
  };
}

function pullRequestDigestSignature(pr: PullRequestDigest | undefined): string {
  if (!pr) return "";
  return JSON.stringify([
    pr.number,
    pr.url,
    pr.state,
    pr.title,
    pr.checks_summary,
    pr.checks?.map((check) => [
      check.name,
      check.bucket,
      check.detail,
      check.url,
    ]),
    pr.draft,
    pr.merged,
    pr.review_decision,
    pr.mergeable,
    pr.merge_state_status,
    pr.head_branch,
    pr.base_branch,
    pr.head_sha,
    pr.auto_merge_enabled,
    pr.in_merge_queue,
  ]);
}
