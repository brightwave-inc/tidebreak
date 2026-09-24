import { create } from "zustand";

import type { RevealTarget } from "./messageSearch";

/**
 * A request to put one message or event of a conversation on screen.
 *
 * The palette and the find bar raise these; the open transcript answers
 * them. The request outlives a navigation, so the palette can raise it and
 * then open the conversation, and the transcript picks it up once its
 * history has loaded. `nonce` tells two requests for the same place apart,
 * so stepping back to a match already shown still scrolls to it.
 */
export type RevealRequest = {
  target: RevealTarget;
  /** The query's words, to highlight inside the revealed row. */
  terms: readonly string[];
  nonce: number;
};

type TranscriptRevealStore = {
  request: RevealRequest | null;
  /** Ask the transcript that holds `target` to show it. */
  reveal: (target: RevealTarget, terms: readonly string[]) => number;
  /** Drop the request once it has been answered, however it went. */
  settle: (nonce: number) => void;
};

let nextNonce = 0;

export const useTranscriptRevealStore = create<TranscriptRevealStore>(
  (set, get) => ({
    request: null,
    reveal: (target, terms) => {
      nextNonce += 1;
      set({ request: { target, terms, nonce: nextNonce } });
      return nextNonce;
    },
    settle: (nonce) => {
      if (get().request?.nonce === nonce) set({ request: null });
    },
  }),
);

/** The pending request for one conversation, or `null`. */
export function useRevealRequest(
  kind: RevealTarget["kind"],
  sessionId: string,
): RevealRequest | null {
  return useTranscriptRevealStore((state) =>
    state.request?.target.kind === kind &&
    state.request.target.sessionId === sessionId
      ? state.request
      : null,
  );
}
