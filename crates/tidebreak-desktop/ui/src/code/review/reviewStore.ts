import { create, type StoreApi, type UseBoundStore } from "zustand";

import type { ApiClient } from "../../api/client";
import { HttpError } from "../../api/client/http";
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
 *
 * The server keeps reviews in memory. When it restarts, a review that was
 * running is gone: it answers 404 for it and leaves it out of the list. The
 * store then stops following it and marks it as stopped by the restart, so
 * the diff does not show a review running that nothing runs.
 */

export type ReviewClient = Pick<
  ApiClient,
  "startCodeReview" | "getCodeReview" | "cancelCodeReview" | "listCodeReviews"
>;

/** How often a running review is read. The server is local. */
export const REVIEW_POLL_MS = 1_000;

/** Why a review the server no longer knows stopped. */
export const LOST_REVIEW_MESSAGE =
  "The review stopped when Tidebreak restarted. Nothing was added to the diff.";

type ReviewState = {
  /** The latest review of each workspace. */
  byWorkspace: Readonly<Record<string, CodeReviewSnapshot>>;
  /** Reviews whose status the person closed, by workspace. */
  closed: Readonly<Record<string, string>>;
  /** Workspaces with a start request in flight. */
  starting: Readonly<Record<string, true>>;
  /**
   * Reviews asked to stop that still run, by workspace. The server gives a
   * stopped engine a few seconds to wind down before the review ends.
   */
  stopping: Readonly<Record<string, string>>;
  /**
   * Start a review. Throws the server's refusal, such as an engine that is
   * signed out, for the form to show.
   */
  start: (
    client: ReviewClient,
    workspaceId: string,
    body: StartCodeReviewBody,
  ) => Promise<CodeReviewSnapshot>;
  /**
   * Stop the workspace's running review. It reads as stopping until the
   * server says it ended.
   */
  cancel: (client: ReviewClient, workspaceId: string) => Promise<void>;
  /** Read the workspace's latest review again, and follow it if it runs. */
  refresh: (client: ReviewClient, workspaceId: string) => Promise<void>;
  /** Hide the status of the workspace's latest review. */
  close: (workspaceId: string) => void;
};

export type CodeReviewStore = UseBoundStore<StoreApi<ReviewState>>;

/** Whether an answer says the server does not know the review. */
function isGone(error: unknown): boolean {
  return error instanceof HttpError && error.status === 404;
}

/**
 * A running review the server no longer knows, as ended: it stopped when
 * Tidebreak restarted, and nothing came of it.
 */
export function lostReview(
  review: CodeReviewSnapshot,
  now: () => string = () => new Date().toISOString(),
): CodeReviewSnapshot {
  const { activity: _activity, ...progress } = review.progress;
  return {
    ...review,
    status: "failed",
    finished_at: now(),
    progress,
    failure: { kind: "failed", message: LOST_REVIEW_MESSAGE },
  };
}

export function createCodeReviewStore(
  options: { pendingReview?: PendingReviewStore; pollMs?: number } = {},
): CodeReviewStore {
  const pendingReview = options.pendingReview ?? usePendingReviewStore;
  const pollMs = options.pollMs ?? REVIEW_POLL_MS;
  /** The review each workspace's poll follows, so one poll runs at a time. */
  const polling = new Map<string, string>();

  const store = create<ReviewState>()((set, get) => {
    function settleStopping(review: CodeReviewSnapshot) {
      if (review.status === "running") return;
      if (get().stopping[review.workspace_id] !== review.id) return;
      set((state) => {
        const stopping = { ...state.stopping };
        delete stopping[review.workspace_id];
        return { stopping };
      });
    }

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
      settleStopping(review);
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

    /** The server no longer knows `review`: stop following it, and say so. */
    function lose(review: CodeReviewSnapshot) {
      if (polling.get(review.workspace_id) === review.id) {
        polling.delete(review.workspace_id);
      }
      const current = get().byWorkspace[review.workspace_id];
      if (current?.id !== review.id || current.status !== "running") return;
      adopt(lostReview(current));
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
          .catch((error: unknown) => {
            if (isGone(error)) {
              lose(review);
              return;
            }
            // The server may be restarting; try again a little later. Once
            // it is back, a review it no longer knows answers 404.
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
      stopping: {},
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
        set((state) => ({
          stopping: { ...state.stopping, [workspaceId]: review.id },
        }));
        try {
          const answer = await client.cancelCodeReview(workspaceId, review.id);
          adopt(answer);
          follow(client, answer);
        } catch (error) {
          set((state) => {
            const stopping = { ...state.stopping };
            delete stopping[workspaceId];
            return { stopping };
          });
          if (isGone(error)) {
            lose(review);
            return;
          }
          throw error;
        }
      },
      refresh: async (client, workspaceId) => {
        // Only a review the store knew before asking can be missing from the
        // answer: one started while the list was on its way is not in it yet.
        const asked = get().byWorkspace[workspaceId];
        const reviews = await client.listCodeReviews(workspaceId);
        const current = get().byWorkspace[workspaceId];
        if (
          current?.status === "running" &&
          current.id === asked?.id &&
          !reviews.some((review) => review.id === current.id)
        ) {
          lose(current);
        }
        const [latest] = reviews;
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

/** Whether the workspace's running review was asked to stop. */
export function useReviewStopping(
  workspaceId: string | undefined,
  reviewId: string | undefined,
  store: CodeReviewStore = useCodeReviewStore,
): boolean {
  return store((state) =>
    Boolean(
      workspaceId && reviewId && state.stopping[workspaceId] === reviewId,
    ),
  );
}
