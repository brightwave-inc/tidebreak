import { useMemo } from "react";

import type { DiffReview } from "./DiffView";
import {
  usePendingReview,
  usePendingReviewStore,
  type PendingReviewStore,
} from "./pendingReview";
import type { ReviewComment } from "./reviewComments";

const NO_COMMENTS: readonly ReviewComment[] = [];

/**
 * Line comments for the files of one workspace diff: the workspace against
 * its base, or one turn's changes. A comment belongs to the diff it was
 * written on, since the same line number names different text in another.
 *
 * Returns a lookup by path, stable while the review is unchanged, or null
 * where there is no workspace to hold a review.
 */
export function useWorkspaceDiffReview({
  workspaceId,
  turnId,
  onDelete,
  store = usePendingReviewStore,
}: {
  workspaceId: string | undefined;
  turnId: string | undefined;
  /** Delete one comment; the host asks first. */
  onDelete: (id: string) => void;
  store?: PendingReviewStore;
}): ((path: string) => DiffReview) | null {
  const { comments, sending } = usePendingReview(workspaceId, store);
  return useMemo(() => {
    if (!workspaceId) return null;
    const byPath = new Map<string, ReviewComment[]>();
    for (const comment of comments) {
      if ((comment.turnId ?? null) !== (turnId ?? null)) continue;
      byPath.set(comment.path, [...(byPath.get(comment.path) ?? []), comment]);
    }
    const reviews = new Map<string, DiffReview>();
    return (path: string) => {
      const known = reviews.get(path);
      if (known) return known;
      const review: DiffReview = {
        comments: byPath.get(path) ?? NO_COMMENTS,
        sending,
        onAdd: (lines, body) =>
          store.getState().add(workspaceId, {
            id: crypto.randomUUID(),
            author: { kind: "person" },
            path,
            ...(turnId ? { turnId } : {}),
            lines,
            body,
            createdAt: new Date().toISOString(),
          }),
        onEdit: (id, body) => store.getState().edit(workspaceId, id, body),
        onDelete,
      };
      reviews.set(path, review);
      return review;
    };
  }, [workspaceId, turnId, comments, sending, onDelete, store]);
}
