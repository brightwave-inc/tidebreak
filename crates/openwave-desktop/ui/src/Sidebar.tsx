import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Ellipsis,
  FolderOpen,
  Library,
  MessageCircleMore,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Pencil,
  RotateCw,
  Settings,
  Shapes,
  SquarePen,
  Sun,
  Trash2,
} from "lucide-react";

import type { Chat } from "./api";
import { useChatListStore } from "./ChatListStore";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import { Logomark } from "./Logomark";
import type { PanelContent, PanelType } from "./panel/panelTypes";
import { usePanelNav } from "./panel/usePanelNav";
import {
  Sidebar as SidebarRail,
  SidebarButton,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarSectionTitle,
  useSidebarWidth,
} from "./sidebar/primitives";
import type { ThemeMode } from "./theme";
import { useUiStore } from "./UiStore";

/** How many conversations the rail shows before deferring to the Chats panel. */
export const RECENT_CHAT_LIMIT = 8;

export type SidebarProps = {
  themeMode: ThemeMode;
  updateReady: boolean;
  updateVersion: string | null;
  onCycleTheme: () => void;
  onNewChat: () => void;
  onStartRename: (chat: Chat) => void;
  onCommitRename: (chat: Chat) => void;
  onCancelRename: () => void;
  onDeleteChat: (chat: Chat) => void;
  onRestartForUpdate: () => void;
};

/**
 * The navigation rail: the way home, the panels of the open conversation, the
 * conversations themselves, and the app's own controls.
 *
 * List state comes straight from the chat-list store; the callback props are
 * the mutations whose orchestration (fences, confirm dialog, chat lifecycle)
 * lives with the shell. Where the workspace is pointed comes from the URL, so
 * selecting a chat or a panel here is navigation rather than a store write.
 */
export function Sidebar({
  themeMode,
  updateReady,
  updateVersion,
  onCycleTheme,
  onNewChat,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onDeleteChat,
  onRestartForUpdate,
}: SidebarProps) {
  const navigate = useNavigate();
  const chats = useChatListStore((state) => state.chats);
  const activeChatId = useChatListStore((state) => state.selected?.id ?? null);
  const chatsError = useChatListStore((state) => state.chatsError);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const renamingChatId = useChatListStore((state) => state.renamingChatId);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const isCompact = useSidebarWidth() === "compact";
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { layout, openPanel } = usePanelNav();

  const onChatRoute = pathname.startsWith("/c/");
  const openPanelTypes: Set<PanelType> =
    onChatRoute && layout.mode === "split"
      ? new Set([layout.left.type, layout.right.type])
      : new Set<PanelType>(["chat"]);

  function showPanel(panel: PanelContent) {
    // The panels belong to a conversation, so reaching one from home or from
    // settings has to arrive at the conversation first.
    if (!onChatRoute && activeChatId) {
      void navigate({
        to: "/c/$chatId",
        params: { chatId: activeChatId },
        search: { left: panel.type, right: "chat" },
      });
      return;
    }
    openPanel(panel);
  }

  // Sources, outputs and folders belong to a conversation. With none open —
  // a first run, before anything has been started — there is nothing for them
  // to show, so they read as unavailable rather than as a dead click.
  const hasConversation = activeChatId !== null;

  const recentChats = chats.slice(0, RECENT_CHAT_LIMIT);

  return (
    <SidebarRail>
      <SidebarHeader>
        <button
          type="button"
          className="inline-flex min-w-0 cursor-pointer items-center gap-2 rounded-md p-1 transition-colors hover:bg-muted"
          aria-label="Home"
          onClick={() => void navigate({ to: "/" })}
        >
          <Logomark />
          {!isCompact && <span className="truncate text-sm font-medium">OpenWave</span>}
        </button>
        <span className="grow" />
        {!isCompact && (
          <WithTooltip label="Collapse sidebar" side="bottom">
            <button
              type="button"
              className="cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              aria-label="Collapse sidebar"
              onClick={toggleSidebar}
            >
              <PanelLeftClose size={15} />
            </button>
          </WithTooltip>
        )}
      </SidebarHeader>

      <SidebarContent className="gap-1 overflow-y-auto px-2">
        {isCompact && (
          <SidebarButton aria-label="Expand sidebar" onClick={toggleSidebar}>
            <PanelLeftOpen />
            <span>Expand sidebar</span>
          </SidebarButton>
        )}

        <SidebarButton onClick={onNewChat} disabled={creatingChat || deletingChatId !== null}>
          <SquarePen />
          <span>{creatingChat ? "Starting…" : "New chat"}</span>
        </SidebarButton>

        <WorkspacePanelButton
          label="Sources"
          icon={<Library />}
          active={openPanelTypes.has("sources")}
          disabled={!hasConversation}
          onClick={() => showPanel({ type: "sources" })}
        />
        <WorkspacePanelButton
          label="Outputs"
          icon={<Shapes />}
          active={openPanelTypes.has("outputs")}
          disabled={!hasConversation}
          onClick={() => showPanel({ type: "outputs" })}
        />
        <WorkspacePanelButton
          label="Folders"
          icon={<FolderOpen />}
          active={openPanelTypes.has("folders")}
          disabled={!hasConversation}
          onClick={() => showPanel({ type: "folders" })}
        />
        <WorkspacePanelButton
          label="All chats"
          icon={<MessageCircleMore />}
          active={openPanelTypes.has("chats")}
          disabled={!hasConversation}
          onClick={() => showPanel({ type: "chats" })}
        />

        <SidebarSectionTitle className="mt-4">Recent</SidebarSectionTitle>
        <div className={isCompact ? "hidden" : "flex flex-col gap-0.5"} aria-label="Chats">
          {recentChats.map((chat) => (
            <RecentChatRow
              key={chat.id}
              chat={chat}
              active={onChatRoute && chat.id === activeChatId}
              renaming={renamingChatId === chat.id}
              renameDraft={renameChatDraft}
              savingTitle={savingTitle}
              mutating={deletingChatId !== null || creatingChat}
              onRenameDraftChange={setRenameDraft}
              onOpen={() => void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })}
              onStartRename={() => onStartRename(chat)}
              onCommitRename={() => onCommitRename(chat)}
              onCancelRename={onCancelRename}
              onDelete={() => onDeleteChat(chat)}
            />
          ))}
          {chats.length > RECENT_CHAT_LIMIT && (
            <button
              type="button"
              className="cursor-pointer rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              onClick={() => showPanel({ type: "chats" })}
            >
              More…
            </button>
          )}
          {chatsError && <p className="px-2 py-1 text-xs text-critical">{chatsError}</p>}
        </div>
      </SidebarContent>

      <SidebarFooter className="flex flex-col gap-0.5">
        {updateReady && (
          <SidebarButton onClick={onRestartForUpdate}>
            <RotateCw />
            <span>Restart to update</span>
            {updateVersion && (
              <span className="ml-auto text-xs text-muted-foreground">v{updateVersion}</span>
            )}
          </SidebarButton>
        )}
        <SidebarButton aria-label={`Theme: ${themeMode}. Click to change.`} onClick={onCycleTheme}>
          {themeMode === "light" ? <Sun /> : themeMode === "dark" ? <Moon /> : <Monitor />}
          <span>Theme</span>
        </SidebarButton>
        <SidebarButton
          aria-current={pathname === "/settings" ? "page" : undefined}
          data-active={pathname === "/settings" || undefined}
          className="data-[active]:bg-muted"
          onClick={() => void navigate({ to: "/settings" })}
        >
          <Settings />
          <span>Settings</span>
        </SidebarButton>
      </SidebarFooter>
    </SidebarRail>
  );
}

function WorkspacePanelButton({
  label,
  icon,
  active,
  disabled,
  onClick,
}: {
  label: string;
  icon: React.ReactNode;
  active: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <SidebarButton
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className="data-[active]:bg-muted"
      disabled={disabled}
      onClick={onClick}
    >
      {icon}
      <span>{label}</span>
    </SidebarButton>
  );
}

function RecentChatRow({
  chat,
  active,
  renaming,
  renameDraft,
  savingTitle,
  mutating,
  onRenameDraftChange,
  onOpen,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onDelete,
}: {
  chat: Chat;
  active: boolean;
  renaming: boolean;
  renameDraft: string;
  savingTitle: boolean;
  mutating: boolean;
  onRenameDraftChange: (draft: string) => void;
  onOpen: () => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onDelete: () => void;
}) {
  const title = chat.title?.trim() || "New chat";

  if (renaming) {
    return (
      <input
        className="w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
        autoFocus
        aria-label="Chat title"
        value={renameDraft}
        disabled={savingTitle}
        onChange={(event) => onRenameDraftChange(event.target.value)}
        onBlur={onCommitRename}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            event.currentTarget.blur();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            onCancelRename();
          }
        }}
      />
    );
  }

  return (
    <div
      className={`group flex items-center rounded-md transition-colors hover:bg-muted ${
        active ? "bg-muted" : ""
      }`}
    >
      <button
        type="button"
        className="min-w-0 flex-1 cursor-pointer truncate px-2 py-1.5 text-left text-sm disabled:pointer-events-none disabled:opacity-50"
        aria-current={active ? "page" : undefined}
        disabled={mutating}
        onClick={onOpen}
      >
        {title}
      </button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            // Revealed on hover, but kept in the layout so the row does not
            // reflow under the cursor.
            className="mr-1 cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 disabled:pointer-events-none"
            aria-label={`Actions for ${title}`}
            disabled={mutating}
          >
            <Ellipsis size={15} />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="right">
          <DropdownMenuItem onSelect={onStartRename}>
            <Pencil />
            Rename
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={onDelete}>
            <Trash2 />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
