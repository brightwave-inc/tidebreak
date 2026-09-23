import { type ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { LayoutGrid, Puzzle } from "lucide-react";

import { useApp } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { CodeModeSwitch } from "@/code/CodeModeSwitch";
import { ChatsSection } from "./ChatsSection";
import { InboxButton } from "./InboxButton";
import { NotificationBellButton } from "@/NotificationBellButton";
import { ProjectsSection } from "./ProjectsSection";
import { SidebarButton } from "./primitives";
import { SidebarFrame } from "./SidebarFrame";
import { useActiveChatId } from "@/useActiveChatId";

/**
 * The one navigation rail, used by every route that is not settings.
 *
 * Its subject is the list of work: a slim block of install-wide destinations
 * at the top, and the conversations filling everything below it. It is
 * mounted once, by the Work layout route, so moving between home, the
 * libraries, and conversations swaps only the pane and the list keeps its
 * scroll. Everything that describes one conversation — outputs, folders,
 * agents — lives in the chat header's status chip instead, beside the
 * conversation it describes.
 *
 * The conversation on screen comes from the URL, the one record of it.
 */
export function AppSidebar() {
  const activeChatId = useActiveChatId() ?? undefined;
  const navigate = useNavigate();
  const { refreshChats } = useApp();
  const chatsError = useChatListStore((state) => state.chatsError);
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const appsActive = pathname === "/apps" || pathname.startsWith("/apps/");
  const pluginsActive =
    pathname === "/plugins" || pathname.startsWith("/plugins/");

  return (
    <SidebarFrame>
      <CodeModeSwitch />

      <nav aria-label="Work destinations" className="sidebar-primary-nav">
        <InboxButton />
        <NotificationBellButton />

        {/* Apps and plugins are install-wide — they outlive every conversation
            — so each is a full page of its own rather than a tab beside one. */}
        <RouteButton
          label="Apps"
          icon={
            <LayoutGrid className={appsActive ? "text-icon-blue" : undefined} />
          }
          active={appsActive}
          onClick={() => void navigate({ to: "/apps" })}
        />
        <RouteButton
          label="Plugins"
          icon={
            <Puzzle
              className={pluginsActive ? "text-icon-violet" : undefined}
            />
          }
          active={pluginsActive}
          onClick={() => void navigate({ to: "/plugins" })}
        />
      </nav>

      <ProjectsSection activeChatId={activeChatId} />
      <ChatsSection activeChatId={activeChatId} />
      {chatsError && (
        <div className="flex shrink-0 flex-col gap-1 px-2 py-1">
          <p className="text-xs text-critical">{chatsError}</p>
          <button
            type="button"
            className="self-start text-xs text-muted-foreground underline-offset-2 hover:underline"
            onClick={() => void refreshChats()}
          >
            Try again
          </button>
        </div>
      )}
    </SidebarFrame>
  );
}

function RouteButton({
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
