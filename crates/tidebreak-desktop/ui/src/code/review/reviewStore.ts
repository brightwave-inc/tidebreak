import { create, type StoreApi, type UseBoundStore } from "zustand";

import type { ApiClient } from "../../api/client";
import type { CodeReviewSnapshot, StartCodeReviewBody } from "../../api/types";
import {
  usePendingReviewStore,
  type PendingReviewStore,
} from "../diff/pendingReview";
import { commentsFromReview } from "../diff/reviewFindings";

/**
 * Reviews of a workspace's changes by another engine, as the desktop follows
 * them: the latest review per workspace, polled while it runs, and its
 * findings added to the pending review once, when it completes.
 *
 * The store is module-level, not a component's, so a review started from the
 * diff keeps being followed while the person works elsewhere, and a reload
 * finds it again through `refresh`.
 */

export type ReviewClient = Pick<
  ApiClient,
  "startCodeReview" | "getCodeReview" | "cancelCodeReview" | "listCodeReviews"
>;

/** How often a running review is read. The server is local. */
export const REVIEW_POLL_MS = 1_000;

type ReviewState = {
  /** The latest review of each workspace. */
  byWorkspace: Readonly<Record<string, CodeReviewSnapshot>>;
  /** Reviews whose status the person closed, by workspace. */
  closed: Readonly<Record<string, string>>;
  /** Workspaces with a start request in flight. */
  starting: Readonly<Record<string, true>>;
  /**
   * Start a review. Throws the server's refusal, such as an engine that is
   * signed out, for the form to show.
   */
  start: (
    client: ReviewClient,
    workspaceId: string,
    body: StartCodeReviewBody,
  ) => Promise<CodeReviewSnapshot>;
  /** Stop the workspace's running review. */
  cancel: (client: ReviewClient, workspaceId: string) => Promise<void>;
  /** Read the workspace's latest review again, and follow it if it runs. */
  refresh: (client: ReviewClient, workspaceId: string) => Promise<void>;
  /** Hide the status of the workspace's latest review. */
  close: (workspaceId: string) => void;
};

export type CodeReviewStore = UseBoundStore<StoreApi<ReviewState>>;

export function createCodeReviewStore(
  options: { pendingReview?: PendingReviewStore; pollMs?: number } = {},
): CodeReviewStore {
  const pendingReview = options.pendingReview ?? usePendingReviewStore;
  const pollMs = options.pollMs ?? REVIEW_POLL_MS;
  /** The review each workspace's poll follows, so one poll runs at a time. */
  const polling = new Map<string, string>();

  const store = create<ReviewState>()((set, get) => {
    function adopt(review: CodeReviewSnapshot) {
      const current = get().byWorkspace[review.workspace_id];
      // A read that answers late never replaces a newer review.
      if (
        current &&
        current.id !== review.id &&
        current.started_at > review.started_at
      ) {
        return;
      }
      set((state) => ({
        byWorkspace: { ...state.byWorkspace, [review.workspace_id]: review },
      }));
      if (review.status === "completed") {
        pendingReview
          .getState()
          .addReviewFindings(
            review.workspace_id,
            review.id,
            commentsFromReview(review),
          );
      }
    }

    function follow(client: ReviewClient, review: CodeReviewSnapshot) {
      if (review.status !== "running") return;
      if (polling.get(review.workspace_id) === review.id) return;
      polling.set(review.workspace_id, review.id);
      const tick = () => {
        if (polling.get(review.workspace_id) !== review.id) return;
        client
          .getCodeReview(review.workspace_id, review.id)
          .then((next) => {
            adopt(next);
            if (next.status === "running") {
              setTimeout(tick, pollMs);
            } else if (polling.get(review.workspace_id) === review.id) {
              polling.delete(review.workspace_id);
            }
          })
          .catch(() => {
            // The server may be restarting; try again on the next beat. A
            // review it no longer knows reads as gone on the next refresh.
            if (polling.get(review.workspace_id) === review.id) {
              setTimeout(tick, pollMs * 3);
            }
          });
      };
      setTimeout(tick, pollMs);
    }

    return {
      byWorkspace: {},
      closed: {},
      starting: {},
      start: async (client, workspaceId, body) => {
        set((state) => ({
          starting: { ...state.starting, [workspaceId]: true },
        }));
        try {
          const review = await client.startCodeReview(workspaceId, body);
          adopt(review);
          follow(client, review);
          return review;
        } finally {
          set((state) => {
            const starting = { ...state.starting };
            delete starting[workspaceId];
            return { starting };
          });
        }
      },
      cancel: async (client, workspaceId) => {
        const review = get().byWorkspace[workspaceId];
        if (!review || review.status !== "running") return;
        const answer = await client.cancelCodeReview(workspaceId, review.id);
        adopt(answer);
      },
      refresh: async (client, workspaceId) => {
        const [latest] = await client.listCodeReviews(workspaceId);
        if (!latest) return;
        adopt(latest);
        follow(client, latest);
      },
      close: (workspaceId) => {
        const review = get().byWorkspace[workspaceId];
        if (!review) return;
        set((state) => ({
          closed: { ...state.closed, [workspaceId]: review.id },
        }));
      },
    };
  });
  return store;
}

export const useCodeReviewStore = createCodeReviewStore();

/** The workspace's latest review, unless the person closed its status. */
export function useWorkspaceReview(
  workspaceId: string | undefined,
  store: CodeReviewStore = useCodeReviewStore,
): CodeReviewSnapshot | null {
  return store((state) => {
    if (!workspaceId) return null;
    const review = state.byWorkspace[workspaceId];
    if (!review) return null;
    return state.closed[workspaceId] === review.id ? null : review;
  });
}
