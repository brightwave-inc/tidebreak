import { useEffect, useMemo } from "react";

import type { DiffReview } from "./DiffView";
import {
  usePendingReview,
  usePendingReviewStore,
  type CommentRelocation,
  type PendingReviewStore,
} from "./pendingReview";
import type { ReviewComment } from "./reviewComments";

const NO_COMMENTS: readonly ReviewComment[] = [];
const NO_RENAMES: ReadonlyMap<string, string> = new Map();

/** The review of one workspace diff, file by file. */
export type WorkspaceDiffReview = {
  /** The review of one file, stable while the review is unchanged. */
  forPath: (path: string) => DiffReview;
  /** The files this diff's comments are on, by the names the diff uses. */
  paths: ReadonlySet<string>;
  /** Comments on this diff's changes as a whole, such as a review summary. */
  general: readonly ReviewComment[];
  /** Which comments are riding a send right now. */
  sending: ReadonlySet<string>;
};

/**
 * Line comments for the files of one workspace diff: the workspace against
 * its base, or one turn's changes. A comment belongs to the diff it was
 * written on, since the same code can sit on different lines in another.
 *
 * A file the diff now shows under a new name takes its comments along, and
 * the review records the new name, so the message names the file as it is.
 *
 * Returns null where there is no workspace to hold a review.
 */
export function useWorkspaceDiffReview({
  workspaceId,
  turnId,
  onDelete,
  onDismiss,
  relocate = true,
  renamed = NO_RENAMES,
  onWriting,
  store = usePendingReviewStore,
}: {
  workspaceId: string | undefined;
  turnId: string | undefined;
  /** Delete one comment; the host asks first. */
  onDelete: (id: string) => void;
  /** Dismiss a reviewer's finding; the host may offer an undo. */
  onDismiss?: (id: string) => void;
  /**
   * Record where comments' lines are now. Off while the diff is cut short,
   * where a line missing from it may only be past the cut.
   */
  relocate?: boolean;
  /** Files the diff shows under a new name: old path to new. */
  renamed?: ReadonlyMap<string, string>;
  /** Told when a new comment starts or stops being written on a file. */
  onWriting?: (path: string, writing: boolean) => void;
  store?: PendingReviewStore;
}): WorkspaceDiffReview | null {
  const { comments, sending } = usePendingReview(workspaceId, store);

  useEffect(() => {
    if (!workspaceId || renamed.size === 0) return;
    const moves = new Map<string, string[]>();
    for (const comment of comments) {
      if (comment.general) continue;
      if ((comment.turnId ?? null) !== (turnId ?? null)) continue;
      const to = renamed.get(comment.path);
      if (to) moves.set(to, [...(moves.get(to) ?? []), comment.id]);
    }
    for (const [to, ids] of moves) {
      store.getState().moveToPath(workspaceId, ids, to);
    }
  }, [workspaceId, turnId, comments, renamed, store]);

  return useMemo(() => {
    if (!workspaceId) return null;
    const byPath = new Map<string, ReviewComment[]>();
    const general: ReviewComment[] = [];
    for (const comment of comments) {
      if ((comment.turnId ?? null) !== (turnId ?? null)) continue;
      if (comment.general) {
        general.push(comment);
        continue;
      }
      const path = renamed.get(comment.path) ?? comment.path;
      byPath.set(path, [...(byPath.get(path) ?? []), comment]);
    }
    const reviews = new Map<string, DiffReview>();
    const forPath = (path: string) => {
      const known = reviews.get(path);
      if (known) return known;
      const review: DiffReview = {
        comments: byPath.get(path) ?? NO_COMMENTS,
        sending,
        onAdd: (comment, body) =>
          store.getState().add(workspaceId, {
            id: crypto.randomUUID(),
            author: { kind: "person" },
            path,
            ...(turnId ? { turnId } : {}),
            ...comment,
            body,
            createdAt: new Date().toISOString(),
          }),
        onEdit: (id, body) => store.getState().edit(workspaceId, id, body),
        onDelete,
        onKeep: (id) => store.getState().keep(workspaceId, id),
        ...(onDismiss ? { onDismiss } : {}),
        ...(relocate
          ? {
              onRelocate: (id: string, change: CommentRelocation) =>
                store.getState().relocate(workspaceId, id, change),
            }
          : {}),
        ...(onWriting
          ? { onWriting: (writing: boolean) => onWriting(path, writing) }
          : {}),
      };
      reviews.set(path, review);
      return review;
    };
    return { forPath, paths: new Set(byPath.keys()), general, sending };
  }, [
    workspaceId,
    turnId,
    comments,
    sending,
    onDelete,
    onDismiss,
    relocate,
    renamed,
    onWriting,
    store,
  ]);
}
