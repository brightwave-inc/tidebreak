import { type ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { CircleAlert, LayoutGrid, MessagesSquare, Puzzle } from "lucide-react";

import type { Chat } from "@/api";
import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import type { PanelType } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { ChatsSection } from "./ChatsSection";
import { InboxButton } from "./InboxButton";
import { NewChatButton } from "./NewChatButton";
import { SidebarButton, SidebarSectionTitle } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

/**
 * The one navigation rail, used by every route that is not settings.
 *
 * Its subject is the chat list: a slim block of install-wide destinations at
 * the top, and the conversations filling everything below it. Home and a
 * conversation see the same rail so that moving between them does not
 * rearrange the furniture. Everything that describes one conversation —
 * outputs, folders, agents — lives in the chat header's status chip instead,
 * beside the conversation it describes.
 *
 * `chat` is the conversation the route is showing, when it is showing one.
 */
export function AppSidebar({ chat }: { chat?: Chat }) {
  const navigate = useNavigate();
  const { newChat, refreshChats } = useApp();
  const { layout, openPanel } = usePanelNav();
  const chatsError = useChatListStore((state) => state.chatsError);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );

  const openPanelTypes: Set<PanelType> = new Set(layout.tabs.map((tab) => tab.type));

  // The list carries per-row markers, but a conversation past the cut can
  // still park a turn on a question. The way to the full table doubles as
  // where that is reported.
  const elsewhereNeedsAttention =
    chat !== undefined &&
    [...chatIdsWithPendingPrompts].some((id) => id !== chat.id);

  return (
    <SidebarFrame>
      <NewChatButton
        onClick={newChat}
        disabled={creatingChat || deletingChatId !== null}
        creating={creatingChat}
      />

      <div className="mt-2 flex shrink-0 flex-col gap-0.5">
        <InboxButton />

        {/* Apps and plugins are install-wide — they outlive every conversation
            — so they open the same way from anywhere: as a panel beside the
            conversation, or beside home's composer when there is none. */}
        <PanelButton
          label="Apps"
          icon={<LayoutGrid />}
          active={openPanelTypes.has("apps")}
          onClick={() => openPanel({ type: "apps" })}
        />
        <PanelButton
          label="Plugins"
          icon={<Puzzle />}
          active={openPanelTypes.has("plugins")}
          onClick={() => openPanel({ type: "plugins" })}
        />
      </div>

      <SidebarSectionTitle className="mt-4">Chats</SidebarSectionTitle>
      <ChatsSection activeChatId={chat?.id} />
      <div className="flex shrink-0 flex-col gap-0.5">
        <SidebarButton
          aria-current={pathname === "/chats" ? "page" : undefined}
          data-active={pathname === "/chats" || undefined}
          className="data-[active]:bg-muted"
          onClick={() => void navigate({ to: "/chats" })}
        >
          <MessagesSquare />
          <span>All chats</span>
          {elsewhereNeedsAttention && (
            <span
              className="text-warning ml-auto shrink-0"
              aria-label="Another chat needs attention"
              title="Another chat needs attention"
            >
              <CircleAlert aria-hidden="true" size={15} />
            </span>
          )}
        </SidebarButton>
        {chatsError && (
          <div className="flex flex-col gap-1 px-2 py-1">
            <p className="text-xs text-critical">{chatsError}</p>
            <button
              type="button"
              className="self-start text-xs text-muted-foreground underline-offset-2 hover:underline"
              onClick={() => void refreshChats()}
            >
              Retry
            </button>
          </div>
        )}
      </div>
    </SidebarFrame>
  );
}

function PanelButton({
  label,
  icon,
  active,
  onClick,
}: {
  label: string;
  icon: ReactNode;
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
