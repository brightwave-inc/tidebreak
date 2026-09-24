import { useMemo } from "react";
import { create, type StoreApi, type UseBoundStore } from "zustand";

import type { HarnessKind } from "../../api/types";
import { isRecord } from "@/lib/guards";
import {
  REVIEW_SEVERITIES,
  type CommentLineSpans,
  type ReviewComment,
  type ReviewCommentLine,
} from "./reviewComments";
import { textKey } from "./textKey";

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
/**
 * Comments kept for queued messages, apart from the review: a large one that
 * fills storage must never stop the review itself from being saved.
 */
const QUEUED_STORAGE_KEY = "tidebreak.code-queued-review.v1";
/**
 * Reviews whose findings already joined a workspace's pending review, so a
 * reload never brings back findings the person dismissed.
 */
const IMPORTED_STORAGE_KEY = "tidebreak.code-imported-reviews.v1";
/** Queued messages whose comments a workspace keeps, newest first. */
export const MAX_QUEUED_REVIEWS = 10;
/** Imported reviews a workspace remembers, newest first. */
export const MAX_IMPORTED_REVIEWS = 50;

/** The comments one queued message carries, kept whole. */
type QueuedReview = {
  /** `textKey` of the review block the message carries. */
  readonly block: string;
  readonly comments: readonly ReviewComment[];
};

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
  /**
   * Comments that went with a message the server queued, kept whole. The
   * block the message carries names each comment's lines, but not the code
   * around them, the old text of a whitespace pair, or another conversation's
   * turn; deleting the message puts back these instead.
   */
  queued: Readonly<Record<string, readonly QueuedReview[]>>;
  /** Reviews whose findings already joined each workspace's review. */
  imported: Readonly<Record<string, readonly string[]>>;
  add: (workspaceId: string, comment: ReviewComment) => void;
  /**
   * Add a finished review's findings, once: a review already imported adds
   * nothing, so findings the person dismissed stay dismissed.
   */
  addReviewFindings: (
    workspaceId: string,
    reviewId: string,
    comments: readonly ReviewComment[],
  ) => void;
  /** Keep a reviewer's finding: it goes with the next message. */
  keep: (workspaceId: string, id: string) => void;
  /** Keep every finding a review proposed that is still waiting. */
  keepAll: (workspaceId: string, reviewId: string) => void;
  /** Dismiss every finding a review proposed that is still waiting. */
  dismissAll: (workspaceId: string, reviewId: string) => void;
  /** Rewrite a comment. A reviewer's finding is kept by the edit. */
  edit: (workspaceId: string, id: string, body: string) => void;
  remove: (workspaceId: string, id: string) => void;
  clear: (workspaceId: string) => void;
  /** Put comments back in the review. */
  restore: (workspaceId: string, comments: readonly ReviewComment[]) => void;
  /**
   * Keep the comments a queued message carries, found again by `block`, the
   * review block in its text. A queued message edited in the tray keeps its
   * block as sent, so the block finds them however the text changed.
   */
  keepQueued: (
    workspaceId: string,
    block: string,
    comments: readonly ReviewComment[],
  ) => void;
  /**
   * Put a deleted queued message's comments back in the review: the ones
   * kept for its block, or, when none were, `fromBlock`'s, read back from
   * the block itself.
   */
  restoreQueued: (
    workspaceId: string,
    block: string,
    fromBlock: () => readonly ReviewComment[],
  ) => void;
  /** Move comments to a file the diff now shows under a new name. */
  moveToPath: (
    workspaceId: string,
    ids: readonly string[],
    path: string,
  ) => void;
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

const HARNESS_KINDS: ReadonlySet<unknown> = new Set<HarnessKind>([
  "claude_code",
  "codex",
  "opencode",
  "grok",
  "internal",
]);

function isAuthor(value: unknown): value is ReviewComment["author"] {
  if (!isRecord(value)) return false;
  if (value.kind === "person") return true;
  return (
    value.kind === "reviewer" &&
    HARNESS_KINDS.has(value.engine) &&
    (value.model === undefined || typeof value.model === "string") &&
    (value.reviewId === undefined || typeof value.reviewId === "string")
  );
}

function isComment(value: unknown): value is ReviewComment {
  if (!isRecord(value)) return false;
  const general = value.general === true;
  return (
    typeof value.id === "string" &&
    isAuthor(value.author) &&
    typeof value.path === "string" &&
    (value.turnId === undefined || typeof value.turnId === "string") &&
    Array.isArray(value.lines) &&
    (general ? value.lines.length === 0 : value.lines.length > 0) &&
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
    typeof value.createdAt === "string" &&
    (value.severity === undefined ||
      (REVIEW_SEVERITIES as readonly unknown[]).includes(value.severity)) &&
    (value.title === undefined || typeof value.title === "string") &&
    (value.proposed === undefined || value.proposed === true) &&
    (value.edited === undefined || value.edited === true) &&
    (value.general === undefined || general)
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

/** The queued messages' comments storage holds, keeping what reads correctly. */
export function readStoredQueued(
  storage: Pick<Storage, "getItem"> | null,
): Record<string, QueuedReview[]> {
  if (!storage) return {};
  try {
    const raw = storage.getItem(QUEUED_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || !isRecord(parsed.workspaces)) return {};
    const out: Record<string, QueuedReview[]> = {};
    for (const [workspaceId, kept] of Object.entries(parsed.workspaces)) {
      if (!Array.isArray(kept)) continue;
      const valid = kept.flatMap((entry): QueuedReview[] =>
        isRecord(entry) &&
        typeof entry.block === "string" &&
        Array.isArray(entry.comments) &&
        entry.comments.every(isComment)
          ? [{ block: entry.block, comments: entry.comments }]
          : [],
      );
      if (valid.length > 0) {
        out[workspaceId] = valid.slice(0, MAX_QUEUED_REVIEWS);
      }
    }
    return out;
  } catch {
    return {};
  }
}

/** The reviews storage says were already imported, per workspace. */
export function readStoredImported(
  storage: Pick<Storage, "getItem"> | null,
): Record<string, string[]> {
  if (!storage) return {};
  try {
    const raw = storage.getItem(IMPORTED_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || !isRecord(parsed.workspaces)) return {};
    const out: Record<string, string[]> = {};
    for (const [workspaceId, ids] of Object.entries(parsed.workspaces)) {
      if (isStringList(ids) && ids.length > 0) {
        out[workspaceId] = ids.slice(0, MAX_IMPORTED_REVIEWS);
      }
    }
    return out;
  } catch {
    return {};
  }
}

function writeStored(
  storage: Pick<Storage, "setItem" | "removeItem"> | null,
  key: string,
  byWorkspace: Readonly<Record<string, readonly unknown[]>>,
): void {
  if (!storage) return;
  try {
    const workspaces = Object.fromEntries(
      Object.entries(byWorkspace).filter(([, items]) => items.length > 0),
    );
    if (Object.keys(workspaces).length === 0) storage.removeItem(key);
    else storage.setItem(key, JSON.stringify({ version: 1, workspaces }));
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

/** `comments` with `restored` after them, less any it already holds. */
function withRestored(
  comments: readonly ReviewComment[],
  restored: readonly ReviewComment[],
): readonly ReviewComment[] {
  const known = new Set(comments.map((comment) => comment.id));
  const fresh = restored.filter((comment) => !known.has(comment.id));
  return fresh.length > 0 ? [...comments, ...fresh] : comments;
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

/** A reviewer's finding the person kept: it goes with the next message. */
function kept(comment: ReviewComment): ReviewComment {
  if (!comment.proposed) return comment;
  const { proposed: _proposed, ...rest } = comment;
  return rest;
}

/** Whether a comment is a finding `reviewId` proposed, still waiting. */
function proposedBy(comment: ReviewComment, reviewId: string): boolean {
  return (
    comment.proposed === true &&
    comment.author.kind === "reviewer" &&
    comment.author.reviewId === reviewId
  );
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
      queued: readStoredQueued(storage),
      imported: readStoredImported(storage),
      add: (workspaceId, comment) =>
        update(workspaceId, (comments) => [...comments, comment]),
      addReviewFindings: (workspaceId, reviewId, found) =>
        set((state) => {
          const imported = state.imported[workspaceId] ?? [];
          if (imported.includes(reviewId)) return state;
          const current = state.byWorkspace[workspaceId] ?? [];
          const comments = withRestored(current, found);
          return {
            byWorkspace:
              comments === current || comments.length === 0
                ? state.byWorkspace
                : { ...state.byWorkspace, [workspaceId]: comments },
            imported: {
              ...state.imported,
              [workspaceId]: [reviewId, ...imported].slice(
                0,
                MAX_IMPORTED_REVIEWS,
              ),
            },
          };
        }),
      keep: (workspaceId, id) =>
        update(workspaceId, (comments) =>
          comments.some((comment) => comment.id === id && comment.proposed)
            ? comments.map((comment) =>
                comment.id === id ? kept(comment) : comment,
              )
            : comments,
        ),
      keepAll: (workspaceId, reviewId) =>
        update(workspaceId, (comments) =>
          comments.some((comment) => proposedBy(comment, reviewId))
            ? comments.map((comment) =>
                proposedBy(comment, reviewId) ? kept(comment) : comment,
              )
            : comments,
        ),
      dismissAll: (workspaceId, reviewId) =>
        update(workspaceId, (comments) =>
          comments.some((comment) => proposedBy(comment, reviewId))
            ? comments.filter((comment) => !proposedBy(comment, reviewId))
            : comments,
        ),
      edit: (workspaceId, id, body) =>
        update(workspaceId, (comments) =>
          comments.map((comment) =>
            comment.id === id
              ? {
                  ...kept(comment),
                  body,
                  // A reviewer's note the person rewrote is theirs now, and
                  // the message says so.
                  ...(comment.author.kind === "reviewer" &&
                  body.trim() !== comment.body.trim()
                    ? { edited: true as const }
                    : {}),
                }
              : comment,
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
          // A reviewer's finding the person has not kept was never counted
          // as going; it stays in the diff for them to decide on.
          const left = (state.byWorkspace[workspaceId] ?? []).filter(
            (comment) => riding.has(comment.id) || comment.proposed,
          );
          if (left.length > 0) next[workspaceId] = left;
          else delete next[workspaceId];
          return { byWorkspace: next };
        }),
      restore: (workspaceId, restored) =>
        update(workspaceId, (comments) => withRestored(comments, restored)),
      keepQueued: (workspaceId, block, comments) => {
        if (comments.length === 0) return;
        set((state) => ({
          queued: {
            ...state.queued,
            [workspaceId]: [
              { block: textKey([block]), comments },
              ...(state.queued[workspaceId] ?? []),
            ].slice(0, MAX_QUEUED_REVIEWS),
          },
        }));
      },
      restoreQueued: (workspaceId, block, fromBlock) =>
        set((state) => {
          const key = textKey([block]);
          const kept = state.queued[workspaceId] ?? [];
          const at = kept.findIndex((entry) => entry.block === key);
          const restored = at >= 0 ? kept[at]!.comments : fromBlock();
          const current = state.byWorkspace[workspaceId] ?? [];
          const comments = withRestored(current, restored);
          const byWorkspace =
            comments === current
              ? state.byWorkspace
              : { ...state.byWorkspace, [workspaceId]: comments };
          if (at < 0) return { byWorkspace };
          const left = kept.filter((_, index) => index !== at);
          const queued = { ...state.queued };
          if (left.length > 0) queued[workspaceId] = left;
          else delete queued[workspaceId];
          return { byWorkspace, queued };
        }),
      moveToPath: (workspaceId, ids, path) =>
        update(workspaceId, (comments) => {
          const moving = new Set(ids);
          if (
            !comments.some(
              (comment) => moving.has(comment.id) && comment.path !== path,
            )
          ) {
            return comments;
          }
          return comments.map((comment) =>
            moving.has(comment.id) ? { ...comment, path } : comment,
          );
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
          (comment) => !riding.has(comment.id) && !comment.proposed,
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
  let writtenQueued = store.getState().queued;
  let writtenImported = store.getState().imported;
  store.subscribe((state) => {
    if (state.byWorkspace !== written) {
      written = state.byWorkspace;
      writeStored(storage, STORAGE_KEY, state.byWorkspace);
    }
    if (state.queued !== writtenQueued) {
      writtenQueued = state.queued;
      writeStored(storage, QUEUED_STORAGE_KEY, state.queued);
    }
    if (state.imported !== writtenImported) {
      writtenImported = state.imported;
      writeStored(storage, IMPORTED_STORAGE_KEY, state.imported);
    }
  });
  return store;
}

export const usePendingReviewStore = createPendingReviewStore();

const NO_COMMENTS: readonly ReviewComment[] = [];
const NO_IDS: readonly string[] = [];

/**
 * The comments a send may take: pending, kept, and not already riding a
 * send. A reviewer's finding waits until the person keeps it.
 */
export function commentsReadyToSend(
  workspaceId: string,
  store: PendingReviewStore = usePendingReviewStore,
): ReviewComment[] {
  const state = store.getState();
  const riding = new Set(state.sending[workspaceId] ?? []);
  return (state.byWorkspace[workspaceId] ?? []).filter(
    (comment) => !riding.has(comment.id) && !comment.proposed,
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

/**
 * How many comments go with the next message, across how many files, how
 * many are riding a send, and how many reviewer findings wait to be kept.
 */
export function reviewSummary(
  comments: readonly ReviewComment[],
  sending: ReadonlySet<string>,
): { count: number; files: number; sending: number; proposed: number } {
  const waiting = comments.filter(
    (comment) => !sending.has(comment.id) && !comment.proposed,
  );
  const riding = comments.filter((comment) => sending.has(comment.id));
  return {
    count: waiting.length,
    files: new Set(
      waiting
        .filter((comment) => !comment.general)
        .map((comment) => comment.path),
    ).size,
    sending: riding.length,
    proposed: comments.filter((comment) => comment.proposed).length,
  };
}
