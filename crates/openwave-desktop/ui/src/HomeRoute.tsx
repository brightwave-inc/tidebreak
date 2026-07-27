import { useEffect, useRef } from "react";
import { useNavigate } from "@tanstack/react-router";

import { useApp } from "./AppContext";
import { useChatListStore } from "./ChatListStore";

const chatListActions = useChatListStore.getState();

/**
 * The app opens on a conversation, so the root route resolves one and steps
 * out of the way. A first run with nothing to open makes a chat rather than
 * showing an empty frame.
 *
 * This is where the home page will go once there is more to show than the
 * conversation you had open last.
 */
export function HomeRoute() {
  const navigate = useNavigate();
  const { client } = useApp();
  const chats = useChatListStore((state) => state.chats);
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const creatingRef = useRef(false);

  useEffect(() => {
    if (!chatsLoaded) return;
    const existing = chats[0];
    if (existing) {
      void navigate({ to: "/c/$chatId", params: { chatId: existing.id }, replace: true });
      return;
    }
    // Guarded because the effect reruns while the request is in flight, and a
    // second create would leave an orphan chat behind on every cold start.
    if (creatingRef.current) return;
    creatingRef.current = true;
    void (async () => {
      try {
        const created = await client.createChat();
        chatListActions.prependChat(created);
        await navigate({ to: "/c/$chatId", params: { chatId: created.id }, replace: true });
      } catch (err) {
        chatListActions.setChatsError(`Could not create a chat: ${String(err)}`);
      } finally {
        creatingRef.current = false;
      }
    })();
  }, [chatsLoaded, chats, client, navigate]);

  return <div className="routed-surface-loading" />;
}
