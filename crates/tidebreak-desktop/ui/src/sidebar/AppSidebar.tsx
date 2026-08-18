import { type ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { LayoutGrid, Puzzle, SquarePen } from "lucide-react";

import type { Chat } from "@/api";
import { useApp } from "@/AppContext";
import { useChatListStore } from "@/ChatListStore";
import { CodeModeSwitch } from "@/code/CodeModeSwitch";
import { useExperimentalFlags } from "@/experimental";
import { ChatsSection } from "./ChatsSection";
import { InboxButton } from "./InboxButton";
import { ProjectsSection } from "./ProjectsSection";
import { SidebarButton, useSidebarWidth } from "./primitives";
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
  const chatsError = useChatListStore((state) => state.chatsError);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const isCompact = useSidebarWidth() === "compact";
  const codeModeEnabled = useExperimentalFlags((state) => state.codeModeEnabled);

  return (
    <SidebarFrame>
      {/* Expanded, starting a chat is the + on the Chats section header. The
          compact rail hides that section, so it keeps a row of its own. */}
      {isCompact && (
        <SidebarButton
          aria-label={creatingChat ? "Starting…" : "New chat"}
          onClick={newChat}
          disabled={creatingChat || deletingChatId !== null}
        >
          <SquarePen />
          <span>New chat</span>
        </SidebarButton>
      )}

      {codeModeEnabled && <CodeModeSwitch />}

      <div className="flex shrink-0 flex-col gap-0.5">
        <InboxButton />

        {/* Apps and plugins are install-wide — they outlive every conversation
            — so each is a full page of its own rather than a tab beside one. */}
        <RouteButton
          label="Apps"
          icon={<LayoutGrid />}
          active={pathname === "/apps" || pathname.startsWith("/apps/")}
          onClick={() => void navigate({ to: "/apps" })}
        />
        <RouteButton
          label="Plugins"
          icon={<Puzzle />}
          active={pathname === "/plugins" || pathname.startsWith("/plugins/")}
          onClick={() => void navigate({ to: "/plugins" })}
        />
      </div>

      <ProjectsSection activeChatId={chat?.id} />
      <ChatsSection activeChatId={chat?.id} />
      {chatsError && (
        <div className="flex shrink-0 flex-col gap-1 px-2 py-1">
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
