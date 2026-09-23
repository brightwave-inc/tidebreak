import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { ApiClient, Chat } from "./api";
import { useChatListStore } from "./ChatListStore";

/**
 * The conversation a chat route shows, from the list of work or the archive.
 *
 * A refresh can take the open conversation out of the list: archived from the
 * command line or another window. That must not unmount it, so this keeps the
 * row it last had and reads the conversation by id. One that still exists
 * joins the list its state says and stays open. One that is gone — deleted in
 * another window, or a stale deep link — sends the reader home rather than
 * leaving an empty frame. The gate is whether the list has been fetched, not
 * whether it has rows: an account with no chats left is exactly the case that
 * would otherwise sit on the loading frame forever.
 */
export function useOpenChat(
  client: Pick<ApiClient, "getChat">,
  chatId: string,
): Chat | null {
  const navigate = useNavigate();
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  // An archived conversation still opens: archiving keeps everything.
  const listed = useChatListStore(
    (state) =>
      state.chats.find((candidate) => candidate.id === chatId) ??
      state.archivedChats.find((candidate) => candidate.id === chatId) ??
      null,
  );
  const [lastListed, setLastListed] = useState(listed);
  if (listed !== null && listed !== lastListed) setLastListed(listed);

  const unlisted = chatsLoaded && listed === null;
  useEffect(() => {
    if (!unlisted) return;
    let cancelled = false;
    client.getChat(chatId).then(
      (found) => {
        if (!cancelled) useChatListStore.getState().adoptChat(found);
      },
      () => {
        if (!cancelled) void navigate({ to: "/", replace: true });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [unlisted, client, chatId, navigate]);

  return listed ?? lastListed;
}
