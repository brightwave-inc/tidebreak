import { useNavigate } from "@tanstack/react-router";
import {
  ChevronLeft,
  CircleAlert,
  FolderOpen,
  Library,
  Shapes,
} from "lucide-react";

import type { Chat } from "@/api";
import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import type { PanelType } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { NewChatButton } from "./NewChatButton";
import { SidebarButton, SidebarSectionTitle } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

/**
 * The rail inside one conversation, holding only what acts on that
 * conversation.
 *
 * The chat list is deliberately absent. Every control here needs a chat to
 * mean anything, and because this rail only exists inside one, none of them
 * has a disabled state to fall back to — which was the tell that the previous
 * shared rail was in the wrong place.
 */
export function ChatSidebar({ chat }: { chat: Chat }) {
  const navigate = useNavigate();
  const { newChat } = useApp();
  const { layout, openPanel } = usePanelNav();
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );

  const openPanelTypes: Set<PanelType> =
    layout.mode === "split"
      ? new Set([layout.left.type, layout.right.type])
      : new Set<PanelType>(["chat"]);

  // The list is not here to carry a per-row marker, but an agent parking a turn
  // in another conversation still has to be noticeable from this one. The way
  // back doubles as where that is reported.
  const elsewhereNeedsAttention = [...chatIdsWithPendingPrompts].some(
    (id) => id !== chat.id,
  );

  return (
    <SidebarFrame>
      <SidebarButton onClick={() => void navigate({ to: "/" })}>
        <ChevronLeft />
        <span>All chats</span>
        {elsewhereNeedsAttention && (
          <span
            className="ml-auto shrink-0 text-amber-600 dark:text-amber-400"
            aria-label="Another chat needs attention"
            title="Another chat needs attention"
          >
            <CircleAlert aria-hidden="true" size={15} />
          </span>
        )}
      </SidebarButton>

      <NewChatButton
        onClick={newChat}
        disabled={creatingChat || deletingChatId !== null}
        creating={creatingChat}
      />

      <SidebarSectionTitle className="mt-4">
        {chat.title?.trim() || "New chat"}
      </SidebarSectionTitle>

      <ChatPanelButton
        label="Sources"
        icon={<Library />}
        active={openPanelTypes.has("sources")}
        onClick={() => openPanel({ type: "sources" })}
      />
      <ChatPanelButton
        label="Outputs"
        icon={<Shapes />}
        active={openPanelTypes.has("outputs")}
        onClick={() => openPanel({ type: "outputs" })}
      />
      <ChatPanelButton
        label="Folders"
        icon={<FolderOpen />}
        active={openPanelTypes.has("folders")}
        onClick={() => openPanel({ type: "folders" })}
      />
    </SidebarFrame>
  );
}

function ChatPanelButton({
  label,
  icon,
  active,
  onClick,
}: {
  label: string;
  icon: React.ReactNode;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <SidebarButton
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className="data-[active]:bg-muted"
      onClick={onClick}
    >
      {icon}
      <span>{label}</span>
    </SidebarButton>
  );
}
