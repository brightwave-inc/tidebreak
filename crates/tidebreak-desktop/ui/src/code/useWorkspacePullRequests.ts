import { useCallback, useEffect, useRef } from "react";

import type { ApiClient } from "../api/client";
import type {
  CodeWorkspacePullRequests,
  PullRequestDigest,
} from "../api/types";
import { useLiveResource, type LiveResource } from "./useLiveContent";

/**
 * The workspace's attributed pull requests, from the durable fact store
 * (decision 77). The digest's `pr_count` is the cheap change signal: when it
 * moves, the list re-reads; the list itself never touches the host.
 */
export function useWorkspacePullRequests(
  client: Pick<ApiClient, "getCodeWorkspacePullRequests">,
  workspaceId: string,
  prCount: number | undefined,
  pr?: PullRequestDigest,
): LiveResource<CodeWorkspacePullRequests> {
  const load = useCallback(
    () => client.getCodeWorkspacePullRequests(workspaceId),
    [client, workspaceId],
  );
  const resource = useLiveResource({
    key: workspaceId,
    revision: 0,
    load,
    errorMessage: "Could not load the workspace's pull requests",
  });
  const signature = JSON.stringify([prCount, pr]);
  const seenRef = useRef({ workspaceId, signature });

  useEffect(() => {
    const seen = seenRef.current;
    if (seen.workspaceId !== workspaceId) {
      // The keyed resource is already loading the new workspace.
      seenRef.current = { workspaceId, signature };
      return;
    }
    if (seen.signature === signature) return;
    seenRef.current = { workspaceId, signature };
    void resource.refresh();
  }, [signature, resource.refresh, workspaceId]);

  return resource;
}
