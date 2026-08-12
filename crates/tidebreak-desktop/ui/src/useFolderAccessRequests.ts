import { useEffect, useState } from "react";
import type { ApiClient, PendingFolderAccessRequest } from "./api";
import {
  hasNativeHost,
  resolveFolderAccessRequest,
  type FolderAccessDecision,
} from "./host";
import {
  PICKER_BUSY_MESSAGE,
  useNativePickerLatch,
} from "./NativePickerLatch";
import { useOpenConversation } from "./OpenConversation";
import { usePendingPrompts } from "./PendingPrompts";

export type FolderAccessRequests = {
  requests: PendingFolderAccessRequest[];
  resolving: Set<string>;
  errors: Record<string, string>;
  decide: (callId: string, decision: FolderAccessDecision) => void;
  cancel: (callId: string, turnId: string) => void;
};

/**
 * Deciding the folder-access requests the agent is waiting on.
 *
 * The requests themselves are watched by the shell rather than read here, so
 * the agent asking for a folder is noticed whatever screen is open — see
 * [useChatPromptWatcher].
 *
 * A decision opens a native dialog, so at most one is in flight at a time —
 * a second prompt while the first is open would be answering a question the
 * reader cannot see. That latch is held app-wide rather than here, because the
 * picker outlives this conversation: see [FolderDecisionLatch].
 */
export function useFolderAccessRequests(
  client: ApiClient | null,
  chatId: string | null,
): FolderAccessRequests {
  const requests = usePendingPrompts((state) => state.folderAccess);
  const refresh = usePendingPrompts((state) => state.refresh);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const pickerHolder = useNativePickerLatch((state) => state.holder);
  const resolving = new Set(pickerHolder === null ? [] : [pickerHolder]);
  const stillOpen = useOpenConversation(chatId);

  // The pane is keyed on the conversation, so this hook is normally replaced
  // rather than reused. Reset anyway: nothing held here belongs to a different
  // conversation, and leaving the keying to do it makes removing that key a
  // silent bug rather than a loud one.
  // The decision latch is deliberately absent: it is the host picker's, not this
  // conversation's, and releasing it on a chat switch is the bug #481 fixed.
  useEffect(() => () => setErrors({}), [chatId]);

  /** Takes the app-wide picker latch, or reports that another surface has it. */
  function beginResolving(callId: string): boolean {
    if (!useNativePickerLatch.getState().claim(callId)) {
      setErrors((current) => ({ ...current, [callId]: PICKER_BUSY_MESSAGE }));
      return false;
    }
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
    return true;
  }

  /** The latch is released unconditionally — it is the host's, not the chat's. */
  function finishResolving(callId: string, startedChatId: string) {
    useNativePickerLatch.getState().release(callId);
    if (stillOpen(startedChatId)) refresh();
  }

  async function decide(callId: string, decision: FolderAccessDecision) {
    if (!chatId || !hasNativeHost()) return;
    const startedChatId = chatId;
    if (!beginResolving(callId)) return;
    try {
      await resolveFolderAccessRequest(startedChatId, callId, decision);
    } catch (err) {
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: String(err) }));
      }
    } finally {
      finishResolving(callId, startedChatId);
    }
  }

  async function cancel(callId: string, turnId: string) {
    if (!client || !chatId) return;
    const startedChatId = chatId;
    if (!beginResolving(callId)) return;
    try {
      await client.cancel(startedChatId, turnId);
    } catch (err) {
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: String(err) }));
      }
    } finally {
      finishResolving(callId, startedChatId);
    }
  }

  return {
    requests,
    resolving,
    errors,
    decide: (callId, decision) => void decide(callId, decision),
    cancel: (callId, turnId) => void cancel(callId, turnId),
  };
}
