import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { RecentChatRow } from "./RecentChatRow";
import { useSidebarWidth } from "./primitives";

/** How many conversations the rail lists before deferring to the All chats table. */
const CHAT_LIST_LIMIT = 20;

/**
 * The rail's list of conversations, most recent first.
 *
 * This is the body of the rail rather than an aside in it: switching chats is
 * the navigation done most, so the list reads the same from home and from
 * inside a conversation, with the open one marked. It is cut at
 * {@link CHAT_LIST_LIMIT} and scrolls; anything past the cut is reached through
 * the All chats table.
 *
 * The open conversation is never cut. A chat returned to across a busy week
 * can fall past the recency cut while still being the one on screen, and a
 * list that drops it stops saying where the reader is.
 */
export function ChatsSection({ activeChatId }: { activeChatId?: string }) {
  const navigate = useNavigate();
  const { deleteChat, startRename, commitRename, cancelRename } = useApp();
  const chats = useChatListStore((state) => state.chats);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const renamingChatId = useChatListStore((state) => state.renamingChatId);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );
  const isCompact = useSidebarWidth() === "compact";

  const listed = chats.slice(0, CHAT_LIST_LIMIT);
  const active = activeChatId
    ? chats.find((chat) => chat.id === activeChatId)
    : undefined;
  if (active && !listed.some((chat) => chat.id === active.id)) listed.push(active);

  // Icons-only has no room for titles, and a column of identical glyphs says
  // nothing — the list stands down and the rail's rows carry the narrow width.
  if (isCompact || listed.length === 0) return null;

  return (
    <div
      className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto"
      aria-label="Chats"
    >
      {listed.map((chat) => (
        <RecentChatRow
          key={chat.id}
          chat={chat}
          active={chat.id === activeChatId}
          needsAttention={chatIdsWithPendingPrompts.has(chat.id)}
          renaming={renamingChatId === chat.id}
          renameDraft={renameChatDraft}
          savingTitle={savingTitle}
          mutating={deletingChatId !== null || creatingChat}
          onRenameDraftChange={setRenameDraft}
          onOpen={() => void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })}
          onStartRename={() => startRename(chat)}
          onCommitRename={() => commitRename(chat)}
          onCancelRename={cancelRename}
          onDelete={() => deleteChat(chat)}
        />
      ))}
    </div>
  );
}
