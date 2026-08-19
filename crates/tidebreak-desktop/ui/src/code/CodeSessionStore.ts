import { create } from "zustand";
import type { SequencedCodeEventFrame } from "../api/types";
import type { CodeConnectionState } from "./CodeSessionController";
import {
  initialCodeSessionState,
  markCodeSessionHydrated,
  reduceCodeSessionEvent,
  type CodeSessionDeps,
  type CodeSessionEffect,
  type CodeSessionState,
} from "./CodeSessionReducer";

/**
 * One code session's state, held outside React so socket callbacks always see
 * current truth without ref mirrors.
 *
 * This is the chat session store factory generalized from one pinned instance
 * to N: the registry constructs one per open session, and nothing here is
 * app-global.
 *
 * `connectionState` sits beside the reducer so a reconnect does not replay as
 * a journal event, and so the first paint can stay `live` instead of flashing
 * a reconnect bar before the socket opens.
 */
export type CodeSessionStore = CodeSessionState & {
  connectionState: CodeConnectionState;
  setConnectionState: (connectionState: CodeConnectionState) => void;
  /**
   * Turn id from the last `turn_began` effect. The pane watches this to drop
   * a queued follow-up once the worker promotes it.
   */
  lastTurnBeganId: string | null;
  applyEvent: (
    framed: SequencedCodeEventFrame,
    deps: CodeSessionDeps,
  ) => CodeSessionEffect[];
  /**
   * Reduce a replay chunk and publish it as one store update.
   *
   * A reopened session can contain hundreds of journal frames. Publishing
   * every frame separately forces React's external-store subscribers to
   * synchronously re-render the whole transcript for every historical token.
   * `settleInitialView` lowers the hydration skeleton in that same update.
   */
  applyEvents: (
    framed: readonly SequencedCodeEventFrame[],
    deps: CodeSessionDeps,
    settleInitialView?: boolean,
  ) => CodeSessionEffect[];
  update: (change: (session: CodeSessionState) => CodeSessionState) => void;
  reset: () => void;
};

export function createCodeSessionStore() {
  return create<CodeSessionStore>()((set, get) => {
    const applyEvents = (
      framed: readonly SequencedCodeEventFrame[],
      deps: CodeSessionDeps,
      settleInitialView = false,
    ): CodeSessionEffect[] => {
      const current = sessionOf(get());
      let state = current;
      const effects: CodeSessionEffect[] = [];
      for (const frame of framed) {
        const transition = reduceCodeSessionEvent(state, frame, deps);
        state = transition.state;
        effects.push(...transition.effects);
      }
      if (settleInitialView) state = markCodeSessionHydrated(state);

      // The reducer returns its input for duplicate/stale frames. Do not turn
      // that no-op into a fresh Zustand snapshot and wake every subscriber.
      if (state !== current) {
        const began = [...effects]
          .reverse()
          .find((effect) => effect.type === "turn_began");
        set(began ? { ...state, lastTurnBeganId: began.turnId } : state);
      }
      return effects;
    };

    return {
      ...initialCodeSessionState(),
      connectionState: "live",
      lastTurnBeganId: null,
      setConnectionState: (connectionState) => {
        if (get().connectionState !== connectionState) set({ connectionState });
      },
      applyEvent: (framed, deps) => applyEvents([framed], deps),
      applyEvents,
      update: (change) => {
        const current = sessionOf(get());
        const next = change(current);
        if (next !== current) set(next);
      },
      reset: () => {
        set({
          ...initialCodeSessionState(),
          connectionState: "live",
          lastTurnBeganId: null,
        });
      },
    };
  });
}

function sessionOf(store: CodeSessionStore): CodeSessionState {
  const {
    applyEvent,
    applyEvents,
    update,
    reset,
    connectionState,
    setConnectionState,
    lastTurnBeganId,
    ...session
  } = store;
  void applyEvent;
  void applyEvents;
  void update;
  void reset;
  void connectionState;
  void setConnectionState;
  void lastTurnBeganId;
  return session;
}
