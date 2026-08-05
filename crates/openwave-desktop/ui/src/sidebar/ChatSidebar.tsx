import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  ChevronLeft,
  CircleAlert,
  FolderOpen,
  LayoutGrid,
  Shapes,
} from "lucide-react";

import type { Chat } from "@/api";
import { useChatAttention } from "@/ChatAttention";
import { Badge } from "@/components/ui/badge";
import { listDeliverables, type DeliverablesCatalog } from "@/deliverables";
import type { PanelType } from "@/panel/panelTypes";
import { usePanelNav } from "@/panel/usePanelNav";
import { useRefreshSignals } from "@/RefreshSignals";
import { InboxButton } from "./InboxButton";
import { RecentChatsSection } from "./RecentChatsSection";
import { SidebarButton, SidebarSectionTitle } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";

// Module scope, not an inline arrow: the count effect keys on the extractor's
// identity, and this rail re-renders on every composer keystroke.
const countDeliverables = (catalog: DeliverablesCatalog) =>
  catalog.deliverables.length;

/**
 * The rail inside one conversation: the controls that act on it, and the way
 * to the others.
 *
 * The recent-chats list rides along even though nothing in it is scoped to
 * this conversation, because switching conversations is the one navigation
 * done most, and routing it through home made the common path the longest
 * one. The open chat is marked so the list also says where the reader is.
 */
export function ChatSidebar({ chat }: { chat: Chat }) {
  const navigate = useNavigate();
  const { layout, openPanel } = usePanelNav();
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );

  const openPanelTypes: Set<PanelType> = new Set(layout.tabs.map((tab) => tab.type));

  // The recent list carries per-row markers, but a chat past its cut can still
  // park a turn on a question. The way back doubles as where that is reported.
  const elsewhereNeedsAttention = [...chatIdsWithPendingPrompts].some(
    (id) => id !== chat.id,
  );

  const outputCount = useSidebarCount(chat.id, listDeliverables, countDeliverables);

  return (
    <SidebarFrame>
      <SidebarButton onClick={() => void navigate({ to: "/chats" })}>
        <ChevronLeft />
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

      <InboxButton />

      <ChatPanelButton
        label="Outputs"
        icon={<Shapes />}
        active={openPanelTypes.has("outputs")}
        onClick={() => openPanel({ type: "outputs" })}
        badge={outputCount > 0 ? (
          <Badge variant="outline" className="-my-0.5">{outputCount}</Badge>
        ) : undefined}
      />
      <ChatPanelButton
        label="Folders"
        icon={<FolderOpen />}
        active={openPanelTypes.has("folders")}
        onClick={() => openPanel({ type: "folders" })}
      />

      <SidebarSectionTitle className="mt-4">Chats</SidebarSectionTitle>
      <div className="flex flex-col gap-0.5">
        <RecentChatsSection activeChatId={chat.id} />
      </div>

      {/* Apps outlive every conversation, but reaching one from inside a chat
          used to mean a trip through home. The library opens beside the
          conversation like the other navigation panels do. */}
      <SidebarSectionTitle className="mt-4">Library</SidebarSectionTitle>
      <ChatPanelButton
        label="Apps"
        icon={<LayoutGrid />}
        active={openPanelTypes.has("apps")}
        onClick={() => openPanel({ type: "apps" })}
      />
    </SidebarFrame>
  );
}

function ChatPanelButton({
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
