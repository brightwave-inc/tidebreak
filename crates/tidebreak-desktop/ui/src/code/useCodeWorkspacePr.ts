import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import type { ApiClient } from "../api/client";
import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import { friendlyErrorMessage } from "@/lib/utils";
import type { LiveResource } from "./useLiveContent";

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
  refreshFromHost: () => Promise<CodeWorkspacePrSnapshot | undefined>;
  runMutation: <T>(
    mutation: CodeWorkspacePrMutation,
    operation: () => Promise<T>,
  ) => Promise<T | undefined>;
};
type Client = Pick<ApiClient, "getCodeWorkspacePr"> &
  Partial<Pick<ApiClient, "refreshCodeWorkspacePr">>;
type State = Pick<
  CodeWorkspacePrResource,
  "data" | "error" | "refreshing" | "busy" | "mutationError"
>;
const resources = new WeakMap<
  Client["getCodeWorkspacePr"],
  Map<string, WorkspacePrResource>
>();

/** One read, mutation lock, and refresh timer for every view of a workspace. */
class WorkspacePrResource {
  state: State = {
    data: null,
    error: null,
    refreshing: true,
    busy: null,
    mutationError: null,
  };
  listeners = new Set<() => void>();
  generation = 0;
  hostError: string | null = null;
  inFlight: Promise<void> | null = null;
  pending = false;
  readers = 0;
  timer: ReturnType<typeof setInterval> | undefined;
  debounce: ReturnType<typeof setTimeout> | undefined;
  constructor(
    public client: Client,
    readonly id: string,
  ) {}
  snapshot = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  update(patch: Partial<State>) {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }
  retain = () => {
    if (this.readers++ === 0) {
      void this.refresh();
      this.timer = setInterval(this.onFocus, 15_000);
      window.addEventListener("focus", this.onFocus);
      document.addEventListener("visibilitychange", this.onFocus);
    }
    return () => {
      if (--this.readers === 0) {
        clearInterval(this.timer);
        clearTimeout(this.debounce);
        window.removeEventListener("focus", this.onFocus);
        document.removeEventListener("visibilitychange", this.onFocus);
      }
    };
  };
  onFocus = () => {
    if (document.visibilityState !== "hidden") void this.refresh();
  };
  invalidate = () => {
    clearTimeout(this.debounce);
    this.debounce = setTimeout(() => void this.refresh(), 250);
  };
  refresh = (): Promise<void> => {
    if (this.state.busy !== null) {
      this.pending = true;
      return Promise.resolve();
    }
    if (this.inFlight) {
      this.pending = true;
      return this.inFlight;
    }
    const generation = this.generation;
    this.update({ refreshing: true });
    this.inFlight = Promise.resolve()
      .then(() => this.client.getCodeWorkspacePr(this.id))
      .then(
        (data) => {
          if (generation === this.generation)
            this.update({ data, error: this.hostError });
        },
        (error) => {
          if (generation === this.generation)
            this.update({
              error: friendlyErrorMessage(
                error,
                "Could not load workspace status",
              ),
            });
        },
      )
      .finally(() => {
        if (generation !== this.generation) return;
        this.inFlight = null;
        this.update({ refreshing: false });
        if (this.pending) {
          this.pending = false;
          void this.refresh();
        }
      });
    return this.inFlight;
  };
  adopt = (data: CodeWorkspacePrSnapshot) => {
    this.hostError = null;
    this.generation++;
    this.inFlight = null;
    this.pending = false;
    this.update({ data, error: null, refreshing: false, mutationError: null });
  };
  setMutationError = (mutationError: string | null) =>
    this.update({ mutationError });
  runMutation = async <T>(
    busy: CodeWorkspacePrMutation,
    operation: () => Promise<T>,
  ): Promise<T | undefined> => {
    if (this.state.busy !== null) return undefined;
    this.update({ busy, mutationError: null });
    try {
      return await operation();
    } finally {
      this.update({ busy: null });
      if (this.pending) {
        this.pending = false;
        void this.refresh();
      }
    }
  };
  refreshFromHost = () =>
    this.runMutation("refresh", async () => {
      try {
        const next = await (this.client.refreshCodeWorkspacePr?.(this.id) ??
          this.client.getCodeWorkspacePr(this.id));
        this.adopt(next);
        return next;
      } catch (error) {
        this.hostError = friendlyErrorMessage(
          error,
          "Could not refresh workspace status",
        );
        this.update({ error: this.hostError });
        throw error;
      }
    });
}

export function useCodeWorkspacePr(
  client: Client,
  workspaceId: string,
  contentRevision: number,
  livePr?: PullRequestDigest,
  enabled = true,
): CodeWorkspacePrResource {
  const resource = useMemo(() => {
    let entries = resources.get(client.getCodeWorkspacePr);
    if (!entries) {
      entries = new Map();
      resources.set(client.getCodeWorkspacePr, entries);
    }
    let value = entries.get(workspaceId);
    if (!value) {
      value = new WorkspacePrResource(client, workspaceId);
      entries.set(workspaceId, value);
    }
    return value;
  }, [client.getCodeWorkspacePr, workspaceId]);
  resource.client = client;
  const state = useSyncExternalStore(
    resource.subscribe,
    resource.snapshot,
    resource.snapshot,
  );
  useEffect(
    () => (enabled ? resource.retain() : undefined),
    [enabled, resource],
  );
  const liveSignature = pullRequestDigestSignature(livePr);
  const seen = useRef({ resource, contentRevision, liveSignature });
  useEffect(() => {
    const previous = seen.current;
    seen.current = { resource, contentRevision, liveSignature };
    if (previous.resource !== resource || !enabled) return;
    if (
      previous.contentRevision !== contentRevision ||
      previous.liveSignature !== liveSignature
    )
      resource.invalidate();
  }, [resource, enabled, contentRevision, liveSignature]);
  return {
    ...state,
    refresh: resource.refresh,
    adopt: resource.adopt,
    busy: state.busy,
    setMutationError: resource.setMutationError,
    runMutation: resource.runMutation,
    refreshFromHost: resource.refreshFromHost,
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
    pr.check_counts && [
      pr.check_counts.passing,
      pr.check_counts.pending,
      pr.check_counts.failing,
      pr.check_counts.skipped,
    ],
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
