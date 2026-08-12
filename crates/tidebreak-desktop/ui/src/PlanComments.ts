import { create } from "zustand";

const KEY_PREFIX = "plan_comments_";

/** A note the reader attached to one block of a proposed plan. */
export type PlanComment = {
  /** The block's raw markdown source, which is both its identity and its quote. */
  blockText: string;
  comment: string;
};

type PlanCommentsState = {
  byCall: Record<string, PlanComment[]>;
  hydrate: (callId: string) => void;
  setComment: (callId: string, blockText: string, comment: string) => void;
  removeComment: (callId: string, blockText: string) => void;
  clear: (callId: string) => void;
};

function persist(callId: string, comments: PlanComment[]): void {
  try {
    if (comments.length === 0) {
      window.localStorage.removeItem(`${KEY_PREFIX}${callId}`);
      return;
    }
    window.localStorage.setItem(
      `${KEY_PREFIX}${callId}`,
      JSON.stringify(comments),
    );
  } catch {
    // Comments that outlive a reload are a convenience; an unwritable store is
    // not a reason to refuse the edit.
  }
}

function readStored(callId: string): PlanComment[] | undefined {
  try {
    const raw = window.localStorage.getItem(`${KEY_PREFIX}${callId}`);
    if (!raw) return undefined;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return undefined;
    return parsed.filter(
      (entry): entry is PlanComment =>
        typeof entry === "object" &&
        entry !== null &&
        typeof (entry as PlanComment).blockText === "string" &&
        typeof (entry as PlanComment).comment === "string",
    );
  } catch {
    return undefined;
  }
}

/**
 * Block-level comments on a proposed plan, keyed by the parked call.
 *
 * They live on the client until the reader sends them: a plan the agent has
 * not been asked to revise yet is a private draft. Persisting to localStorage
 * is what keeps a half-written round of edits from evaporating on a reload.
 * One comment per block — a second note on the same block replaces the first.
 */
export const usePlanComments = create<PlanCommentsState>((set, get) => ({
  byCall: {},

  hydrate: (callId) => {
    if (get().byCall[callId]) return;
    const stored = readStored(callId);
    if (!stored?.length) return;
    set((state) => ({ byCall: { ...state.byCall, [callId]: stored } }));
  },

  setComment: (callId, blockText, comment) =>
    set((state) => {
      const existing = state.byCall[callId] ?? [];
      const next = existing.some((entry) => entry.blockText === blockText)
        ? existing.map((entry) =>
            entry.blockText === blockText ? { ...entry, comment } : entry,
          )
        : [...existing, { blockText, comment }];
      persist(callId, next);
      return { byCall: { ...state.byCall, [callId]: next } };
    }),

  removeComment: (callId, blockText) =>
    set((state) => {
      const next = (state.byCall[callId] ?? []).filter(
        (entry) => entry.blockText !== blockText,
      );
      persist(callId, next);
      return { byCall: { ...state.byCall, [callId]: next } };
    }),

  clear: (callId) =>
    set((state) => {
      persist(callId, []);
      const { [callId]: _dropped, ...rest } = state.byCall;
      return { byCall: rest };
    }),
}));

/**
 * Render the collected comments as the one feedback string the server takes.
 *
 * The revision request is a single message, so each note becomes a markdown
 * blockquote of the block it addresses followed by what the reader asked for —
 * the same pairing they saw on screen, in a form the agent reads as prose.
 */
export function serializePlanComments(comments: PlanComment[]): string {
  return comments
    .map(
      (entry) =>
        `${entry.blockText
          .split("\n")
          .map((line) => `> ${line}`)
          .join("\n")}\n\n${entry.comment}`,
    )
    .join("\n\n---\n\n");
}
