import { useEffect, useRef, useState, type RefObject } from "react";
import {
  ActiveTurnSteerFence,
  canBeginActiveTurnSteer,
  shouldClearAcceptedSteerDraft,
  type ActiveTurnSteerRequest,
} from "./ActiveTurnSteer";
import type { ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useChatSessionStore } from "./ChatSessionStore";
import { useOpenConversation } from "./OpenConversation";
import { useTurnLifecycle } from "./TurnLifecycleSignals";

export type TurnControls = {
  cancelPendingTurnId: string | null;
  cancelError: string | null;
  cancel: () => Promise<void>;
  steerPendingTurnId: string | null;
  steerError: string | null;
  steerStatus: string | null;
  steer: () => Promise<void>;
  clearSteerFeedback: () => void;
};

/**
 * Stopping and redirecting one conversation's active turn.
 *
 * Both controls are guarded against being pressed twice: a cancel remembers the
 * turn it was issued for, and guidance is fenced by [ActiveTurnSteerFence] so a
 * retry of unchanged text reuses its identity rather than sending twice. The
 * turn itself moves on the event stream, which arrives here as a
 * [TurnLifecycleEvent] — what to retire differs per event, which is why this
 * reads a named event and not just a revision counter.
 *
 * @param draftRef the live composer draft, read at the moment guidance is sent
 *   rather than at render, so a keystroke that has not painted yet still counts.
 * @param onDraftAccepted called when accepted guidance was the draft still in
 *   the composer, so the composer can clear it.
 * @param voiceInputUsed whether voice transcription contributed to this draft.
 */
export function useTurnControls(
  client: ApiClient | null,
  chatId: string | null,
  draftRef: RefObject<string>,
  onDraftAccepted: () => void,
  voiceInputUsed = false,
): TurnControls {
  const [cancelPendingTurnId, setCancelPendingTurnId] = useState<string | null>(
    null,
  );
  const [cancelError, setCancelError] = useState<string | null>(null);
  const [steerPendingTurnId, setSteerPendingTurnId] = useState<string | null>(
    null,
  );
  const [steerError, setSteerError] = useState<string | null>(null);
  const [steerStatus, setSteerStatus] = useState<string | null>(null);
  const cancelRequestTurnRef = useRef<string | null>(null);
  const steerFenceRef = useRef(new ActiveTurnSteerFence());
  const busy = useChatSessionStore((session) => session.busy);
  const activeTurnId = useChatSessionStore((session) => session.activeTurnId);
  const stillOpen = useOpenConversation(chatId);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const revision = useTurnLifecycle((state) => state.revision);
  const lifecycleEvent = useTurnLifecycle((state) => state.last);

  function clearCancelRequestState() {
    cancelRequestTurnRef.current = null;
    setCancelPendingTurnId(null);
    setCancelError(null);
  }

  function clearSteerRequestState() {
    steerFenceRef.current.invalidate();
    setSteerPendingTurnId(null);
    setSteerError(null);
    setSteerStatus(null);
  }

  // Only a signal raised after this hook mounted means anything to it; the
  // counter is app-wide and may already be well past zero on arrival.
  const lastRevisionRef = useRef(revision);
  useEffect(() => {
    if (lastRevisionRef.current === revision) return;
    lastRevisionRef.current = revision;
    switch (lifecycleEvent) {
      case "began":
        clearCancelRequestState();
        clearSteerRequestState();
        return;
      case "began_same_turn":
        // The turn the reader is guiding is still the one running, so their
        // guidance — and the notice that it was sent — still stands.
        clearCancelRequestState();
        return;
      case "resolved":
        clearCancelRequestState();
        clearSteerRequestState();
        return;
      case "submitted":
        // A local submission clears only what the reader can see. The
        // cancel-request turn is deliberately left standing: it fences a second
        // stop for the turn it named, and the `began` that confirms the new
        // turn is what retires it.
        setCancelPendingTurnId(null);
        setCancelError(null);
        return;
    }
  }, [revision, lifecycleEvent]);

  // Deleting this conversation retires the guidance aimed at it: the fence goes
  // stale so a reply still in flight cannot land, and the notice goes with the
  // conversation rather than following the reader to its replacement.
  useEffect(() => {
    if (!chatId || deletingChatId !== chatId) return;
    clearSteerRequestState();
  }, [chatId, deletingChatId]);

  // The pane is keyed on the conversation, so this hook is normally replaced
  // rather than reused. Reset anyway: nothing held here belongs to a different
  // conversation, and leaving the keying to do it makes removing that key a
  // silent bug rather than a loud one.
  useEffect(
    () => () => {
      clearSteerRequestState();
      clearCancelRequestState();
    },
    [chatId],
  );

  /**
   * Whether a steer reply may still be applied. The current chat is named only
   * while this conversation is genuinely open — a chat being deleted keeps its
   * id for the whole round trip, so naming it here would let a reply paint onto
   * a conversation on its way out.
   */
  function canApplySteerResponse(request: ActiveTurnSteerRequest): boolean {
    return steerFenceRef.current.canApplyResponse(request, {
      chatId: stillOpen(request.chatId) ? request.chatId : "",
      turnId: useChatSessionStore.getState().activeTurnId ?? "",
    });
  }

  async function cancel(): Promise<void> {
    const turnId = activeTurnId;
    if (
      !client ||
      !chatId ||
      !busy ||
      !turnId ||
      cancelRequestTurnRef.current === turnId
    ) {
      return;
    }
    const startedChatId = chatId;

    cancelRequestTurnRef.current = turnId;
    setCancelPendingTurnId(turnId);
    setCancelError(null);
    try {
      await client.cancel(startedChatId, turnId);
    } catch (err) {
      if (
        stillOpen(startedChatId) &&
        cancelRequestTurnRef.current === turnId
      ) {
        cancelRequestTurnRef.current = null;
        setCancelPendingTurnId(null);
        setCancelError(String(err));
      }
    }
  }

  /**
   * Sends the composer draft as guidance. The returned promise settles when the
   * request does, not when it is submitted: the composer awaits this to time
   * restoring focus, and resolving early would hand focus back while the request
   * is still open and put the caret in a field the reader may retype into.
   */
  async function steer(): Promise<void> {
    const admission = {
      busy,
      turnId: useChatSessionStore.getState().activeTurnId,
      cancelRequestTurnId: cancelRequestTurnRef.current,
      deletionInFlight: deletingChatId !== null,
    };
    if (!client || !chatId || !canBeginActiveTurnSteer(admission)) return;
    const turnId = admission.turnId;

    const request = steerFenceRef.current.begin(
      { chatId, turnId },
      draftRef.current,
      () => crypto.randomUUID(),
      voiceInputUsed,
    );
    if (!request) return;

    setSteerPendingTurnId(turnId);
    setSteerError(null);
    setSteerStatus("Sending guidance…");
    setCancelError(null);
    try {
      await client.steer(
        request.chatId,
        request.turnId,
        request.steerId,
        request.content,
        true,
        request.voiceInputUsed,
      );
      if (!canApplySteerResponse(request)) return;

      steerFenceRef.current.finish(request);
      setSteerPendingTurnId(null);
      if (shouldClearAcceptedSteerDraft(request, draftRef.current)) {
        onDraftAccepted();
      }
      setSteerStatus("Guidance sent");
    } catch (err) {
      if (!canApplySteerResponse(request)) return;

      steerFenceRef.current.fail(request);
      setSteerPendingTurnId(null);
      setSteerStatus(null);
      setSteerError(String(err));
    }
  }

  return {
    cancelPendingTurnId,
    cancelError,
    cancel,
    steerPendingTurnId,
    steerError,
    steerStatus,
    steer,
    // Typing is the reader answering the last attempt; the verdict on it goes
    // stale as soon as the text it was about changes.
    clearSteerFeedback: () => {
      setSteerError(null);
      setSteerStatus(null);
    },
  };
}
