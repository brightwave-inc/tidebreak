import type { Chat } from "./api";
import { Logomark } from "./Logomark";
import {
  Ellipsis,
  FolderOpen,
  LibraryBig,
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

export type SidebarProps = {
  chats: Chat[];
  activeChatId: string;
  chatsError: string | null;
  primaryView: "chat" | "documents" | "settings";
  foldersPanelOpen: boolean;
  nativeHost: boolean;
  creatingChat: boolean;
  deletingChatId: string | null;
  renamingChatId: string | null;
  renameChatDraft: string;
  savingTitle: boolean;
  themeMode: ThemeMode;
  updateReady: boolean;
  updateVersion: string | null;
  onCycleTheme: () => void;
  onNewChat: () => void;
  onSelectChat: (chat: Chat) => void;
  onStartRename: (chat: Chat) => void;
  onRenameDraftChange: (value: string) => void;
  onCommitRename: (chat: Chat) => void;
  onCancelRename: () => void;
  onDeleteChat: (chat: Chat) => void;
  onShowDocuments: () => void;
  onToggleFolders: () => void;
  onShowSettings: () => void;
  onRestartForUpdate: () => void;
};

/** The navigation aside: brand/theme, chat list, and footer actions. */
export function Sidebar({
  chats,
  activeChatId,
  chatsError,
  primaryView,
  foldersPanelOpen,
  nativeHost,
  creatingChat,
  deletingChatId,
  renamingChatId,
  renameChatDraft,
  savingTitle,
  themeMode,
  updateReady,
  updateVersion,
  onCycleTheme,
  onNewChat,
  onSelectChat,
  onStartRename,
  onRenameDraftChange,
  onCommitRename,
  onCancelRename,
  onDeleteChat,
  onShowDocuments,
  onToggleFolders,
  onShowSettings,
  onRestartForUpdate,
}: SidebarProps) {
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

      {nativeHost && (
        <button
          type="button"
          className={`sidebar-action sidebar-library${primaryView === "documents" ? " is-active" : ""}`}
          onClick={onShowDocuments}
        >
          <LibraryBig size={16} />
          Sources
        </button>
      )}

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
                    onChange={(event) =>
                      onRenameDraftChange(event.target.value)
                    }
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
            onClick={onToggleFolders}
          >
            <FolderOpen size={16} />
            Folders
          </button>
        )}
        <button
          type="button"
          className={`sidebar-action${primaryView === "settings" ? " is-active" : ""}`}
          onClick={onShowSettings}
        >
          <Settings size={16} />
          Settings
        </button>
      </div>
    </aside>
  );
}
