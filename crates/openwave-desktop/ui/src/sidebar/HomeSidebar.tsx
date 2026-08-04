import { useNavigate, useRouterState, useSearch } from "@tanstack/react-router";
import { LayoutGrid, MessagesSquare } from "lucide-react";

import { useApp } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { InboxButton } from "./InboxButton";
import { NewChatButton } from "./NewChatButton";
import { RecentChatsSection } from "./RecentChatsSection";
import { SidebarButton, SidebarSectionTitle } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

/**
 * The rail outside any conversation: starting one, and returning to one.
 *
 * Nothing here is scoped to a chat, so nothing here is ever disabled for want
 * of one. The conversation's own controls — its sources, outputs and folders —
 * live on {@link ChatSidebar}, which only exists once there is a conversation
 * for them to act on.
 *
 * "Recent" is the rail's own short list; "All chats" opens the full searchable
 * table in the main area.
 */
export function HomeSidebar() {
  const navigate = useNavigate();
  const { newChat, refreshChats } = useApp();
  const chatsError = useChatListStore((state) => state.chatsError);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const search = useSearch({ strict: false }) as { left?: string; right?: string };

  // The Apps library lives on home; the entry is "current" when either slot
  // holds it, list or detail alike.
  const holdsApps = (segment: string | undefined) =>
    segment === "apps" || segment?.startsWith("apps.") === true;
  const appsOpen =
    pathname === "/" && (holdsApps(search.left) || holdsApps(search.right));

  return (
    <SidebarFrame>
      <NewChatButton
        onClick={newChat}
        disabled={creatingChat || deletingChatId !== null}
        creating={creatingChat}
      />

      <div className="mt-2">
        <InboxButton />
      </div>

      <SidebarSectionTitle className="mt-4">Chats</SidebarSectionTitle>
      <div className="flex flex-col gap-0.5">
        <RecentChatsSection />
        <SidebarButton
          aria-current={pathname === "/chats" ? "page" : undefined}
          data-active={pathname === "/chats" || undefined}
          className="data-[active]:bg-muted"
          onClick={() => void navigate({ to: "/chats" })}
        >
          <MessagesSquare />
          <span>All chats</span>
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

      {/* Apps are profile-scoped — they outlive every conversation — so their
          library belongs on the chat-free rail, opening as a panel on home. */}
      <SidebarSectionTitle className="mt-4">Library</SidebarSectionTitle>
      <SidebarButton
        aria-current={appsOpen ? "page" : undefined}
        data-active={appsOpen || undefined}
        className="data-[active]:bg-muted"
        onClick={() => void navigate({ to: "/", search: { left: "apps" } })}
      >
        <LayoutGrid />
        <span>Apps</span>
      </SidebarButton>
    </SidebarFrame>
  );
}
