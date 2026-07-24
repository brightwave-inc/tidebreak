import type { ReactNode } from "react";
import { Settings } from "lucide-react";
import type { Chat } from "./api";
import { ChatTabs, requiresNativeHost } from "./ChatTabs";
import { CHAT_SURFACE } from "./Surface";
import { DeliverablesView } from "./DeliverablesView";
import { DocumentsView } from "./DocumentsView";
import { FoldersView } from "./FoldersView";
import { useUiStore } from "./UiStore";

export type ChatWorkspaceProps = {
  chat: Chat;
  status: string;
  nativeHost: boolean;
  /** The transcript surface, supplied by the owner that holds session state. */
  transcript: ReactNode;
};

/**
 * The workspace for the selected chat: one header carrying the conversation
 * title, the surface switcher, and status, over whichever scoped surface is
 * selected. The header lives here rather than inside each surface so the
 * switcher keeps its position when the body changes.
 */
export function ChatWorkspace({
  chat,
  status,
  nativeHost,
  transcript,
}: ChatWorkspaceProps) {
  const selected = useUiStore((state) => state.surface);
  const showSettings = useUiStore((state) => state.showSettings);
  // A surface the host cannot serve is offered as disabled in the switcher, so
  // reaching one here means the host went away underneath the selection.
  const surface =
    !nativeHost && requiresNativeHost(selected.kind) ? CHAT_SURFACE : selected;

  return (
    <div className="chat-workspace">
      <header className="conversation-header">
        <div className="conversation-title-row">
          <h1>{chat.title?.trim() || "New chat"}</h1>
        </div>
        <ChatTabs nativeHost={nativeHost} />
        <div className="conversation-header-actions">
          <div className="mobile-settings-actions">
            <button
              type="button"
              className="btn"
              aria-label="Settings"
              onClick={showSettings}
            >
              <Settings size={14} />
            </button>
          </div>
          <span className="status" title={status}>
            {status}
          </span>
        </div>
      </header>

      <div className="workspace-body">
        {surface.kind === "documents" ? (
          <DocumentsView chatId={chat.id} />
        ) : surface.kind === "deliverables" ? (
          <DeliverablesView chatId={chat.id} initialFilename={surface.itemId} />
        ) : surface.kind === "folders" ? (
          <FoldersView chat={chat} />
        ) : (
          transcript
        )}
      </div>
    </div>
  );
}
