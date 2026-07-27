import { useNavigate } from "@tanstack/react-router";
import type { Chat } from "./api";
import { Logomark } from "./Logomark";
import {
  Ellipsis,
  FolderOpen,
  Library,
  Monitor,
  PanelLeftClose,
  Moon,
  Pencil,
  RotateCw,
  Settings,
  Shapes,
  SquarePen,
  Sun,
  Trash2,
} from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { WithTooltip } from "@/components/ui/tooltip";
import type { ThemeMode } from "./theme";
import { useChatListStore } from "./ChatListStore";
import { usePanelNav } from "./panel/usePanelNav";
import type { PanelContent } from "./panel/panelTypes";
import { useUiStore } from "./UiStore";

export type SidebarProps = {
  /** Render the in-sidebar hide control (off when the titlebar owns it). */
  collapseControl?: boolean;
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
 * The navigation aside: brand and theme, the panels of the open conversation,
 * the chat list, and footer actions.
 *
 * List state comes straight from the chat-list store; the callback props are
 * the mutations whose orchestration (fences, confirm dialog, chat lifecycle)
 * lives with the shell. Where the workspace is pointed comes from the URL, so
 * selecting a chat or a panel here is navigation rather than a store write.
 */
export function Sidebar({
  collapseControl = true,
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
  const chats = useChatListStore((state) => state.chats);
  const activeChatId = useChatListStore((state) => state.selected?.id ?? null);
  const chatsError = useChatListStore((state) => state.chatsError);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const renamingChatId = useChatListStore((state) => state.renamingChatId);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const settingsOpen = useUiStore((state) => state.settingsOpen);
  const openSettings = useUiStore((state) => state.openSettings);
  const closeSettings = useUiStore((state) => state.closeSettings);
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const navigate = useNavigate();
  const { layout, openPanel } = usePanelNav();

  const openPanelTypes = new Set(
    layout.mode === "split" ? [layout.left.type, layout.right.type] : ["chat"],
  );

  function showPanel(panel: PanelContent) {
    closeSettings();
    openPanel(panel);
  }

  function openChat(chat: Chat) {
    closeSettings();
    void navigate({ to: "/c/$chatId", params: { chatId: chat.id } });
  }
  return (
    <aside className="sidebar">
      <div className="sidebar-brand">
        <Logomark />
        <span>OpenWave</span>
        {collapseControl && (
          <WithTooltip label="Hide sidebar" side="bottom">
            <button
              type="button"
              className="theme-toggle"
              aria-label="Hide sidebar"
              onClick={toggleSidebar}
            >
              <PanelLeftClose size={15} />
            </button>
          </WithTooltip>
        )}
        <WithTooltip
          label={`Theme: ${themeMode} — click to change`}
          side="bottom"
        >
          <button
            type="button"
            className="theme-toggle"
            aria-label={`Theme: ${themeMode}. Click to change.`}
            onClick={onCycleTheme}
          >
            {themeMode === "light" ? (
              <Sun size={15} />
            ) : themeMode === "dark" ? (
              <Moon size={15} />
            ) : (
              <Monitor size={15} />
            )}
          </button>
        </WithTooltip>
      </div>

      <button
        type="button"
        className="new-chat"
        onClick={onNewChat}
        disabled={creatingChat || deletingChatId !== null}
      >
        <SquarePen size={15} />
        {creatingChat
          ? "Starting…"
          : deletingChatId
            ? "Deleting…"
            : "New chat"}
      </button>

      <div className="sidebar-section">
        <div className="conversation-list" aria-label="Workspace">
          <SidebarPanelButton
            label="Sources"
            icon={<Library size={15} />}
            active={!settingsOpen && openPanelTypes.has("sources")}
            onClick={() => showPanel({ type: "sources" })}
          />
          <SidebarPanelButton
            label="Outputs"
            icon={<Shapes size={15} />}
            active={!settingsOpen && openPanelTypes.has("outputs")}
            onClick={() => showPanel({ type: "outputs" })}
          />
          <SidebarPanelButton
            label="Folders"
            icon={<FolderOpen size={15} />}
            active={!settingsOpen && openPanelTypes.has("folders")}
            onClick={() => showPanel({ type: "folders" })}
          />
        </div>
      </div>

      <div className="sidebar-section">
        <span className="sidebar-label">Chats</span>
        <div className="conversation-list" aria-label="Chats">
          {chats.map((item) => {
            const chatTitle = item.title?.trim() || "New chat";
            const isActive = !settingsOpen && item.id === activeChatId;
            const mutating = deletingChatId !== null || creatingChat;

            if (renamingChatId === item.id) {
              return (
                <div key={item.id} className="conversation-row is-renaming">
                  <input
                    className="conversation-rename-input"
                    autoFocus
                    aria-label="Chat title"
                    value={renameChatDraft}
                    disabled={savingTitle}
                    onChange={(event) => setRenameDraft(event.target.value)}
                    onBlur={() => onCommitRename(item)}
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
                </div>
              );
            }

            return (
              <div
                key={item.id}
                className={`conversation-row${isActive ? " is-active" : ""}`}
              >
                <button
                  type="button"
                  className="conversation-item"
                  aria-current={isActive ? "page" : undefined}
                  disabled={mutating}
                  onClick={() => openChat(item)}
                >
                  <span className="conversation-title">{chatTitle}</span>
                </button>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <button
                      type="button"
                      className="conversation-menu"
                      aria-label={`Actions for ${chatTitle}`}
                      title="Chat actions"
                      disabled={mutating}
                    >
                      <Ellipsis size={15} />
                    </button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" side="right">
                    <DropdownMenuItem onSelect={() => onStartRename(item)}>
                      <Pencil />
                      Rename
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      variant="destructive"
                      onSelect={() => onDeleteChat(item)}
                    >
                      <Trash2 />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            );
          })}
        </div>
        {chatsError && <p className="sidebar-error">{chatsError}</p>}
      </div>

      <div className="sidebar-footer">
        {updateReady && (
          <button
            type="button"
            className="sidebar-action sidebar-update"
            onClick={onRestartForUpdate}
          >
            <RotateCw size={16} />
            <span>Restart to update</span>
            {updateVersion && (
              <span className="sidebar-update-version">v{updateVersion}</span>
            )}
          </button>
        )}
        <button
          type="button"
          className={`sidebar-action${settingsOpen ? " is-active" : ""}`}
          onClick={openSettings}
        >
          <Settings size={16} />
          Settings
        </button>
      </div>
    </aside>
  );
}

function SidebarPanelButton({
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
    <div className={`conversation-row${active ? " is-active" : ""}`}>
      <button
        type="button"
        className="conversation-item sidebar-panel-item"
        aria-current={active ? "page" : undefined}
        onClick={onClick}
      >
        {icon}
        <span className="conversation-title">{label}</span>
      </button>
    </div>
  );
}
