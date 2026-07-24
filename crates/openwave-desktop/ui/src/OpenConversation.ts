import { useEffect, useRef } from "react";
import { useChatListStore } from "./ChatListStore";

/**
 * Whether a response that started under `startedChatId` may still be applied.
 *
 * The surfaces that own a conversation's requests are keyed on the chat, so a
 * response that outlives a chat *switch* lands on an unmounted hook and is
 * discarded for free. Deletion is the case that keying misses: the root marks
 * the chat as deleting and only selects a replacement once the delete request
 * and the chat-list reload have both come back, so the doomed conversation
 * stays mounted — with a matching id — for the whole round trip.
 *
 * Returns a stable predicate that reads current truth when it is called, so a
 * handler can capture it before an await and still ask an up-to-date question
 * afterwards.
 *
 * The predicate also has to answer for a hook that is already gone. Its inputs
 * are only written while rendering, so without the unmount cleanup below it
 * would keep reporting the last conversation it saw as open forever — which is
 * the opposite of what an unmounted hook means, and would make it disagree with
 * the self-resets the hooks using it perform on the same event.
 */
export function useOpenConversation(
  chatId: string | null,
): (startedChatId: string) => boolean {
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const current = useRef<{ chatId: string | null; deletingChatId: string | null }>({
    chatId,
    deletingChatId,
  });
  current.current = { chatId, deletingChatId };

  useEffect(
    () => () => {
      current.current = { chatId: null, deletingChatId: null };
    },
    [],
  );

  return useRef(
    (startedChatId: string) =>
      current.current.chatId === startedChatId &&
      current.current.deletingChatId !== startedChatId,
  ).current;
}
