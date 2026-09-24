import { useMemo } from "react";
import { create, type StoreApi, type UseBoundStore } from "zustand";

import { isRecord } from "@/lib/guards";
import type {
  CommentLineSpans,
  ReviewComment,
  ReviewCommentLine,
} from "./reviewComments";

/**
 * The comments waiting to go with a workspace's next message.
 *
 * Keyed by workspace: the diff belongs to the worktree, and whichever of the
 * workspace's conversations sends next takes them. They live in local
 * storage, so a reload or a restart keeps a half-finished review. A send
 * claims the comments it carries in one step, so two conversations sending
 * at once never both carry them; they leave the review once the server
 * accepts the message and go back to waiting when it refuses.
 */

const STORAGE_KEY = "tidebreak.code-pending-review.v1";

/** Where a comment's lines are now, as the diff showing them found them. */
export type CommentRelocation = {
  /** The same lines, numbered where they sit now. */
  lines?: readonly ReviewCommentLine[];
  span?: CommentLineSpans;
  outdated: boolean;
};

type PendingReviewState = {
  byWorkspace: Readonly<Record<string, readonly ReviewComment[]>>;
  /** Comment ids riding a send that has not answered yet. Never stored. */
  sending: Readonly<Record<string, readonly string[]>>;
  add: (workspaceId: string, comment: ReviewComment) => void;
  edit: (workspaceId: string, id: string, body: string) => void;
  remove: (workspaceId: string, id: string) => void;
  clear: (workspaceId: string) => void;
  /** Put comments back in the review, such as a deleted queued message's. */
  restore: (workspaceId: string, comments: readonly ReviewComment[]) => void;
  /**
   * Record where a comment's lines are now, or that they changed. Nothing
   * is written when that is what the review already says.
   */
  relocate: (
    workspaceId: string,
    id: string,
    change: CommentRelocation,
  ) => void;
  /**
   * Take every comment ready to send and mark it as riding this send, in
   * one step. Returns the comments as they were taken.
   */
  claim: (workspaceId: string) => readonly ReviewComment[];
  /**
   * The send answered. Accepted comments leave the review, unless one was
   * edited while it rode: the agent has the old words, so the new ones wait
   * for the next message. Refused comments wait again.
   */
  finishSend: (
    workspaceId: string,
    claimed: readonly ReviewComment[],
    accepted: boolean,
  ) => void;
};

export type PendingReviewStore = UseBoundStore<StoreApi<PendingReviewState>>;

function isCommentLine(value: unknown): value is ReviewCommentLine {
  return (
    isRecord(value) &&
    (value.kind === "add" ||
      value.kind === "del" ||
      value.kind === "context") &&
    (value.oldNo === null || typeof value.oldNo === "number") &&
    (value.newNo === null || typeof value.newNo === "number") &&
    typeof value.text === "string" &&
    (value.oldText === undefined || typeof value.oldText === "string")
  );
}

function isStringList(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function isSpanText(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

function isComment(value: unknown): value is ReviewComment {
  return (
    isRecord(value) &&
    typeof value.id === "string" &&
    isRecord(value.author) &&
    value.author.kind === "person" &&
    typeof value.path === "string" &&
    (value.turnId === undefined || typeof value.turnId === "string") &&
    Array.isArray(value.lines) &&
    value.lines.length > 0 &&
    value.lines.every(isCommentLine) &&
    (value.unquoted === undefined || typeof value.unquoted === "number") &&
    (value.span === undefined ||
      (isRecord(value.span) &&
        isSpanText(value.span.lines) &&
        isSpanText(value.span.oldLines))) &&
    (value.context === undefined ||
      (isRecord(value.context) &&
        isStringList(value.context.before) &&
        isStringList(value.context.after))) &&
    (value.outdated === undefined || typeof value.outdated === "boolean") &&
    typeof value.body === "string" &&
    typeof value.createdAt === "string"
  );
}

/** What storage holds, keeping every comment that still reads correctly. */
export function readStoredReview(
  storage: Pick<Storage, "getItem"> | null,
): Record<string, ReviewComment[]> {
  if (!storage) return {};
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || !isRecord(parsed.workspaces)) return {};
    const out: Record<string, ReviewComment[]> = {};
    for (const [workspaceId, comments] of Object.entries(parsed.workspaces)) {
      if (!Array.isArray(comments)) continue;
      const valid = comments.filter(isComment);
      if (valid.length > 0) out[workspaceId] = valid;
    }
    return out;
  } catch {
    // A review that cannot be read starts empty; it is never worth a crash.
    return {};
  }
}

function writeStoredReview(
  storage: Pick<Storage, "setItem" | "removeItem"> | null,
  byWorkspace: Readonly<Record<string, readonly ReviewComment[]>>,
): void {
  if (!storage) return;
  try {
    const workspaces = Object.fromEntries(
      Object.entries(byWorkspace).filter(([, comments]) => comments.length > 0),
    );
    if (Object.keys(workspaces).length === 0) storage.removeItem(STORAGE_KEY);
    else
      storage.setItem(STORAGE_KEY, JSON.stringify({ version: 1, workspaces }));
  } catch {
    // Storage full or blocked: the review still lives for this session.
  }
}

function browserStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

function without(
  record: Readonly<Record<string, readonly string[]>>,
  workspaceId: string,
  ids: readonly string[],
): Record<string, readonly string[]> {
  const drop = new Set(ids);
  const left = (record[workspaceId] ?? []).filter((id) => !drop.has(id));
  const next = { ...record };
  if (left.length > 0) next[workspaceId] = left;
  else delete next[workspaceId];
  return next;
}

function sameLines(
  a: readonly ReviewCommentLine[],
  b: readonly ReviewCommentLine[],
): boolean {
  return (
    a.length === b.length &&
    a.every(
      (line, index) =>
        line.oldNo === b[index]!.oldNo && line.newNo === b[index]!.newNo,
    )
  );
}

function sameSpan(a?: CommentLineSpans, b?: CommentLineSpans): boolean {
  return a?.lines === b?.lines && a?.oldLines === b?.oldLines;
}

/** A store over `storage`. Tests pass their own to stand in for a reload. */
export function createPendingReviewStore(
  storage: Storage | null = browserStorage(),
): PendingReviewStore {
  const store = create<PendingReviewState>()((set, get) => {
    const update = (
      workspaceId: string,
      change: (comments: readonly ReviewComment[]) => readonly ReviewComment[],
    ) =>
      set((state) => {
        const current = state.byWorkspace[workspaceId] ?? [];
        const comments = change(current);
        if (comments === current) return state;
        const next = { ...state.byWorkspace };
        if (comments.length > 0) next[workspaceId] = comments;
        else delete next[workspaceId];
        return { byWorkspace: next };
      });
    return {
      byWorkspace: readStoredReview(storage),
      sending: {},
      add: (workspaceId, comment) =>
        update(workspaceId, (comments) => [...comments, comment]),
      edit: (workspaceId, id, body) =>
        update(workspaceId, (comments) =>
          comments.map((comment) =>
            comment.id === id ? { ...comment, body } : comment,
          ),
        ),
      remove: (workspaceId, id) =>
        update(workspaceId, (comments) =>
          comments.filter((comment) => comment.id !== id),
        ),
      clear: (workspaceId) =>
        set((state) => {
          const riding = new Set(state.sending[workspaceId] ?? []);
          const next = { ...state.byWorkspace };
          const left = (state.byWorkspace[workspaceId] ?? []).filter(
            (comment) => riding.has(comment.id),
          );
          if (left.length > 0) next[workspaceId] = left;
          else delete next[workspaceId];
          return { byWorkspace: next };
        }),
      restore: (workspaceId, restored) =>
        update(workspaceId, (comments) => {
          const known = new Set(comments.map((comment) => comment.id));
          const fresh = restored.filter((comment) => !known.has(comment.id));
          return fresh.length > 0 ? [...comments, ...fresh] : comments;
        }),
      relocate: (workspaceId, id, change) =>
        update(workspaceId, (comments) => {
          let changed = false;
          const next = comments.map((comment) => {
            if (comment.id !== id) return comment;
            const lines =
              change.lines && !sameLines(change.lines, comment.lines)
                ? change.lines
                : comment.lines;
            const span =
              change.span && !sameSpan(change.span, comment.span)
                ? change.span
                : comment.span;
            const outdated = change.outdated || undefined;
            if (
              lines === comment.lines &&
              span === comment.span &&
              outdated === comment.outdated
            ) {
              return comment;
            }
            changed = true;
            const { outdated: _was, ...rest } = comment;
            return {
              ...rest,
              lines,
              ...(span ? { span } : {}),
              ...(outdated ? { outdated } : {}),
            };
          });
          return changed ? next : comments;
        }),
      claim: (workspaceId) => {
        const state = get();
        const riding = new Set(state.sending[workspaceId] ?? []);
        const ready = (state.byWorkspace[workspaceId] ?? []).filter(
          (comment) => !riding.has(comment.id),
        );
        if (ready.length === 0) return [];
        set({
          sending: {
            ...state.sending,
            [workspaceId]: [
              ...(state.sending[workspaceId] ?? []),
              ...ready.map((comment) => comment.id),
            ],
          },
        });
        return ready;
      },
      finishSend: (workspaceId, claimed, accepted) =>
        set((state) => {
          const ids = claimed.map((comment) => comment.id);
          const sending = without(state.sending, workspaceId, ids);
          if (!accepted) return { sending };
          const sentBody = new Map(
            claimed.map((comment) => [comment.id, comment.body]),
          );
          const next = { ...state.byWorkspace };
          const left = (state.byWorkspace[workspaceId] ?? []).filter(
            (comment) =>
              !sentBody.has(comment.id) ||
              sentBody.get(comment.id) !== comment.body,
          );
          if (left.length > 0) next[workspaceId] = left;
          else delete next[workspaceId];
          return { sending, byWorkspace: next };
        }),
    };
  });
  let written = store.getState().byWorkspace;
  store.subscribe((state) => {
    if (state.byWorkspace === written) return;
    written = state.byWorkspace;
    writeStoredReview(storage, state.byWorkspace);
  });
  return store;
}

export const usePendingReviewStore = createPendingReviewStore();

const NO_COMMENTS: readonly ReviewComment[] = [];
const NO_IDS: readonly string[] = [];

/** The comments a send may take: pending, and not already riding a send. */
export function commentsReadyToSend(
  workspaceId: string,
  store: PendingReviewStore = usePendingReviewStore,
): ReviewComment[] {
  const state = store.getState();
  const riding = new Set(state.sending[workspaceId] ?? []);
  return (state.byWorkspace[workspaceId] ?? []).filter(
    (comment) => !riding.has(comment.id),
  );
}

/** Every pending comment on a workspace, and which of them are sending. */
export function usePendingReview(
  workspaceId: string | undefined,
  store: PendingReviewStore = usePendingReviewStore,
): { comments: readonly ReviewComment[]; sending: ReadonlySet<string> } {
  const comments = store((state) =>
    workspaceId ? (state.byWorkspace[workspaceId] ?? NO_COMMENTS) : NO_COMMENTS,
  );
  const sendingIds = store((state) =>
    workspaceId ? (state.sending[workspaceId] ?? NO_IDS) : NO_IDS,
  );
  const sending = useMemo(() => new Set(sendingIds), [sendingIds]);
  return { comments, sending };
}

/** How many comments go with the next message, and across how many files. */
export function reviewSummary(
  comments: readonly ReviewComment[],
  sending: ReadonlySet<string>,
): { count: number; files: number; sending: number } {
  const waiting = comments.filter((comment) => !sending.has(comment.id));
  return {
    count: waiting.length,
    files: new Set(waiting.map((comment) => comment.path)).size,
    sending: comments.length - waiting.length,
  };
}
