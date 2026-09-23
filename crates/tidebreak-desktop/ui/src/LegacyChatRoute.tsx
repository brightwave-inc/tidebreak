import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useApp } from "./AppContext";
import { useChatListStore } from "./ChatListStore";
import { ChatRoute } from "./ChatRoute";
import { codeClientGeneration } from "./code/CodeClientGeneration";

/**
 * `/c/$chatId`: a Work conversation, or an older link that names something
 * else.
 *
 * A conversation the local list holds opens at once. Only an id the list does
 * not hold is looked up: older Slack buttons address `/c` for a session
 * another principal owns, or one that runs in code mode, and those go to the
 * code session page. A Work conversation of the reader's that the list does
 * not hold — an archived one opened from a link — is added to the store and
 * opened. Once the conversation is open it stays open: a refresh that drops it
 * from the list, such as an archive from the command line, is the chat route's
 * to settle, and must not unmount it.
 */
export function LegacyChatRoute({ chatId }: { chatId: string }) {
  const { client } = useApp();
  return (
    <ResolveChatRoute
      key={`${codeClientGeneration(client)}:${chatId}`}
      chatId={chatId}
    />
  );
}

function ResolveChatRoute({ chatId }: { chatId: string }) {
  const { client } = useApp();
  const navigate = useNavigate();
  const known = useChatListStore(
    (state) =>
      state.chats.some((chat) => chat.id === chatId) ||
      state.archivedChats.some((chat) => chat.id === chatId),
  );
  const chatsLoaded = useChatListStore((state) => state.chatsLoaded);
  const [settled, setSettled] = useState(false);
  const [opened, setOpened] = useState(false);
  if ((known || settled) && !opened) setOpened(true);

  useEffect(() => {
    if (known || opened || !chatsLoaded || settled) return;
    let cancelled = false;
    void (async () => {
      const session = await client.getCodeSession(chatId).catch(() => null);
      if (cancelled) return;
      if (
        session &&
        (session.is_owner === false ||
          session.workspace_id !== null ||
          session.harness_kind !== "internal")
      ) {
        void navigate({
          to: "/code/s/$sessionId",
          params: { sessionId: chatId },
          replace: true,
        });
        return;
      }
      const chat = await client.getChat(chatId).catch(() => null);
      if (cancelled) return;
      if (chat) useChatListStore.getState().adoptChat(chat);
      // A conversation that is gone still reaches the chat route, which sends
      // a stale link home rather than leaving it on this placeholder.
      setSettled(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [client, chatId, known, opened, chatsLoaded, settled, navigate]);

  if (known || settled || opened) return <ChatRoute chatId={chatId} />;
  // Inside the shared frame, so the rail stays put while this settles.
  return (
    <p role="status" className="text-muted-foreground p-6 text-sm">
      Opening conversation…
    </p>
  );
}
