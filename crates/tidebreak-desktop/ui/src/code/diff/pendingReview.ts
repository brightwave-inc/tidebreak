import { useMemo } from "react";
import { create, type StoreApi, type UseBoundStore } from "zustand";

import { isRecord } from "@/lib/guards";
import type { ReviewComment, ReviewCommentLine } from "./reviewComments";

/**
 * The comments waiting to go with a workspace's next message.
 *
 * Keyed by workspace: the diff belongs to the worktree, and whichever of the
 * workspace's conversations sends next takes them. They live in local
 * storage, so a reload or a restart keeps a half-finished review. A send
 * marks the comments it carries; they leave the review once the server
 * accepts the message and stay, unmarked, when it refuses.
 */

const STORAGE_KEY = "tidebreak.code-pending-review.v1";

type PendingReviewState = {
  byWorkspace: Readonly<Record<string, readonly ReviewComment[]>>;
  /** Comment ids riding a send that has not answered yet. Never stored. */
  sending: Readonly<Record<string, readonly string[]>>;
  add: (workspaceId: string, comment: ReviewComment) => void;
  edit: (workspaceId: string, id: string, body: string) => void;
  remove: (workspaceId: string, id: string) => void;
  clear: (workspaceId: string) => void;
  /** Mark comments as riding a send, so they are neither edited nor sent twice. */
  beginSend: (workspaceId: string, ids: readonly string[]) => void;
  /** The send answered: accepted comments leave, refused ones stay. */
  finishSend: (
    workspaceId: string,
    ids: readonly string[],
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
    typeof value.text === "string"
  );
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

/** A store over `storage`. Tests pass their own to stand in for a reload. */
export function createPendingReviewStore(
  storage: Storage | null = browserStorage(),
): PendingReviewStore {
  const store = create<PendingReviewState>()((set) => {
    const update = (
      workspaceId: string,
      change: (comments: readonly ReviewComment[]) => readonly ReviewComment[],
    ) =>
      set((state) => {
        const next = { ...state.byWorkspace };
        const comments = change(state.byWorkspace[workspaceId] ?? []);
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
      beginSend: (workspaceId, ids) =>
        set((state) => ({
          sending: {
            ...state.sending,
            [workspaceId]: [...(state.sending[workspaceId] ?? []), ...ids],
          },
        })),
      finishSend: (workspaceId, ids, accepted) =>
        set((state) => {
          const sending = without(state.sending, workspaceId, ids);
          if (!accepted) return { sending };
          const sent = new Set(ids);
          const next = { ...state.byWorkspace };
          const left = (state.byWorkspace[workspaceId] ?? []).filter(
            (comment) => !sent.has(comment.id),
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
