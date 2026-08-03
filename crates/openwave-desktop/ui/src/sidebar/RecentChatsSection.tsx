import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { History } from "lucide-react";

import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { RecentChatRow } from "./RecentChatRow";
import { SidebarButton, useSidebarWidth } from "./primitives";

/** How many conversations the rail shows before deferring to the All chats table. */
const RECENT_CHAT_LIMIT = 8;

/**
 * The rail's short list of recent conversations, collapsible under its own
 * header row.
 *
 * Shared between the home rail and the conversation rail: switching chats is
 * the navigation done most, so the list is reachable from inside a
 * conversation too, with the open one marked. Anything beyond the cut goes
 * through the full All chats table.
 */
export function RecentChatsSection({ activeChatId }: { activeChatId?: string }) {
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
  const [recentCollapsed, setRecentCollapsed] = useState(false);

  const recentChats = chats.slice(0, RECENT_CHAT_LIMIT);
  const showRecent = !recentCollapsed && !isCompact && recentChats.length > 0;

  return (
    <>
      <SidebarButton
        aria-expanded={!recentCollapsed}
        onClick={() => setRecentCollapsed((collapsed) => !collapsed)}
      >
        <History />
        <span>Recent</span>
      </SidebarButton>
      {showRecent && (
        <div
          className="ml-4 flex flex-col gap-0.5 border-l border-border pl-2"
          aria-label="Recent chats"
        >
          {recentChats.map((chat) => (
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
      )}
    </>
  );
}
