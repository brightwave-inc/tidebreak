import { useEffect, useState, type ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  CircleAlert,
  FolderOpen,
  LayoutGrid,
  MessagesSquare,
  Puzzle,
  Shapes,
} from "lucide-react";

import type { Chat } from "@/api";
import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { Badge } from "@/components/ui/badge";
import { listDeliverables, type DeliverablesCatalog } from "@/deliverables";
import type { PanelType } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { useRefreshSignals } from "@/RefreshSignals";
import { ChatsSection } from "./ChatsSection";
import { InboxButton } from "./InboxButton";
import { NewChatButton } from "./NewChatButton";
import { SidebarButton, SidebarSectionTitle } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

// Module scope, not an inline arrow: the count effect keys on the extractor's
// identity, and this rail re-renders on every composer keystroke.
const countDeliverables = (catalog: DeliverablesCatalog) =>
  catalog.deliverables.length;

/**
 * The one navigation rail, used by every route that is not settings.
 *
 * Its subject is the chat list: a slim block of actions at the top, and the
 * conversations filling everything below it. Home and a conversation see the
 * same rail so that moving between them does not rearrange the furniture —
 * what a conversation adds is the two panels that only mean something when
 * there is one to act on, Outputs and Folders.
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

        {/* Outputs and Folders describe one conversation, so they exist only
            where there is one. Offering them on home is what let the rail
            navigate into whichever chat happened to have been open last. */}
        {chat && (
          <>
            <PanelButton
              label="Outputs"
              icon={<Shapes />}
              active={openPanelTypes.has("outputs")}
              onClick={() => openPanel({ type: "outputs" })}
              badge={<OutputCountBadge chatId={chat.id} />}
            />
            <PanelButton
              label="Folders"
              icon={<FolderOpen />}
              active={openPanelTypes.has("folders")}
              onClick={() => openPanel({ type: "folders" })}
            />
          </>
        )}

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
  badge,
  onClick,
}: {
  label: string;
  icon: ReactNode;
  active: boolean;
  badge?: ReactNode;
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
      {badge}
    </SidebarButton>
  );
}

/** How many outputs this conversation has produced, or nothing while it has none. */
function OutputCountBadge({ chatId }: { chatId: string }) {
  const count = useSidebarCount(chatId, listDeliverables, countDeliverables);
  if (count === 0) return null;
  return (
    <Badge variant="outline" className="-my-0.5">
      {count}
    </Badge>
  );
}

/**
 * Fetch a count on mount and whenever the refresh-signal store ticks any of the
 * targets that could change the number (folder access for sources, output
 * writebacks for deliverables). Errors are swallowed — the badge simply stays at
 * its last known value or zero.
 */
function useSidebarCount<T>(
  chatId: string,
  fetcher: (chatId: string) => Promise<T>,
  extract: (result: T) => number,
): number {
  const [count, setCount] = useState(0);
  const folderAccess = useRefreshSignals((s) => s.folderAccess);
  const outputWritebacks = useRefreshSignals((s) => s.outputWritebacks);

  useEffect(() => {
    let cancelled = false;
    fetcher(chatId).then(
      (result) => { if (!cancelled) setCount(extract(result)); },
      () => { /* swallow — stale count is acceptable */ },
    );
    return () => { cancelled = true; };
  }, [chatId, folderAccess, outputWritebacks, fetcher, extract]);

  return count;
}
