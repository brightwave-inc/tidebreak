import { useNavigate } from "@tanstack/react-router";

import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { NewChatButton } from "./NewChatButton";
import { RecentChatRow } from "./RecentChatRow";
import { SidebarSectionTitle, useSidebarWidth } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

/** How many conversations the rail shows before deferring to the home list. */
const RECENT_CHAT_LIMIT = 8;

/**
 * The rail outside any conversation: starting one, and returning to one.
 *
 * Nothing here is scoped to a chat, so nothing here is ever disabled for want
 * of one. The conversation's own controls — its sources, outputs and folders —
 * live on {@link ChatSidebar}, which only exists once there is a conversation
 * for them to act on.
 */
export function HomeSidebar() {
  const navigate = useNavigate();
  const { newChat, deleteChat, startRename, commitRename, cancelRename } = useApp();
  const chats = useChatListStore((state) => state.chats);
  const chatsError = useChatListStore((state) => state.chatsError);
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

  const recentChats = chats.slice(0, RECENT_CHAT_LIMIT);

  return (
    <SidebarFrame>
      <NewChatButton
        onClick={newChat}
        disabled={creatingChat || deletingChatId !== null}
        creating={creatingChat}
      />

      {recentChats.length > 0 && (
        <SidebarSectionTitle className="mt-4">Recent</SidebarSectionTitle>
      )}
      <div className={isCompact ? "hidden" : "flex flex-col gap-0.5"} aria-label="Chats">
        {recentChats.map((chat) => (
          <RecentChatRow
            key={chat.id}
            chat={chat}
            active={false}
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
        {chatsError && <p className="px-2 py-1 text-xs text-critical">{chatsError}</p>}
      </div>
    </SidebarFrame>
  );
}
