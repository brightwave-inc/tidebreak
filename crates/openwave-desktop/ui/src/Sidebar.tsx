import type { Chat } from "./api";
import { Logomark } from "./Logomark";
import {
  Ellipsis,
  FolderOpen,
  Monitor,
  Moon,
  Pencil,
  RotateCw,
  Settings,
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
import { useUiStore } from "./UiStore";

export type SidebarProps = {
  nativeHost: boolean;
  themeMode: ThemeMode;
  updateReady: boolean;
  updateVersion: string | null;
  onCycleTheme: () => void;
  onNewChat: () => void;
  onSelectChat: (chat: Chat) => void;
  onStartRename: (chat: Chat) => void;
  onCommitRename: (chat: Chat) => void;
  onCancelRename: () => void;
  onDeleteChat: (chat: Chat) => void;
  onRestartForUpdate: () => void;
};

/**
 * The navigation aside: brand/theme, chat list, and footer actions. List and
 * view state come straight from the chat-list and UI stores; the callback
 * props are the mutations whose orchestration (fences, confirm dialog,
 * session lifecycle) lives with the owner.
 */
export function Sidebar({
  nativeHost,
  themeMode,
  updateReady,
  updateVersion,
  onCycleTheme,
  onNewChat,
  onSelectChat,
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
  const primaryView = useUiStore((state) => state.primaryView);
  const foldersPanelOpen = useUiStore(
    (state) => state.settingsPanel === "folders",
  );
  const showSettings = useUiStore((state) => state.showSettings);
  const toggleFoldersPanel = useUiStore((state) => state.toggleFoldersPanel);
  return (
    <aside className="sidebar">
      <div className="sidebar-brand">
        <Logomark />
        <span>OpenWave</span>
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
        <span className="sidebar-label">Chats</span>
        <div className="conversation-list" aria-label="Chats">
          {chats.map((item) => {
            const chatTitle = item.title?.trim() || "New chat";
            const isActive = primaryView === "chat" && item.id === activeChatId;
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
                  onClick={() => onSelectChat(item)}
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
        {nativeHost && (
          <button
            type="button"
            className={`sidebar-action${primaryView === "chat" && foldersPanelOpen ? " is-active" : ""}`}
            onClick={() => toggleFoldersPanel({ showChat: true })}
          >
            <FolderOpen size={16} />
            Folders
          </button>
        )}
        <button
          type="button"
          className={`sidebar-action${primaryView === "settings" ? " is-active" : ""}`}
          onClick={showSettings}
        >
          <Settings size={16} />
          Settings
        </button>
      </div>
    </aside>
  );
}
