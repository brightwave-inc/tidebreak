import { useNavigate } from "@tanstack/react-router";
import { Bug, Download, Ellipsis, Pencil, Trash2 } from "lucide-react";

import type { Chat } from "./api";
import { useApp } from "./AppContext";
import { chatDebugDeps, copyChatDebug, saveChatDebug } from "./ChatDebugBundle";
import { useChatListStore } from "./ChatListStore";
import { hasNativeHost } from "./host";
import { useTypewriterOnce } from "./useTypewriterOnce";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";

/**
 * BW-style breadcrumb header: "Chats / chat title".
 *
 * Clicking "Chats" steps out of the conversation to home — the chat list lives
 * on the rail now, so out of a chat is the only place the crumb can lead.
 * Clicking the title swaps it for an inline edit field. The ellipsis menu
 * offers Rename and Delete, wired to the shell's existing orchestration.
 */
export function ChatHeaderTitle({ chat }: { chat: Chat }) {
  const navigate = useNavigate();
  const { startRename, commitRename, cancelRename, deleteChat } = useApp();
  const renaming = useChatListStore((state) => state.renamingChatId === chat.id);
  const renameDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const justNamed = useChatListStore(
    (state) => state.derivedTitleChatId === chat.id,
  );
  const title = chat.title?.trim() || "New chat";
  const displayTitle = useTypewriterOnce(title, justNamed);

  return (
    <div className="flex min-w-0 items-center gap-2 text-sm">
      <button
        type="button"
        className="shrink-0 font-medium text-muted-foreground hover:underline cursor-pointer"
        onClick={() => void navigate({ to: "/" })}
      >
        Chats
      </button>
      <span className="text-muted-foreground shrink-0">/</span>

      {renaming ? (
        <div className="inline-grid min-w-0">
          {/* Hidden span to auto-size the grid cell */}
          <span
            className="invisible col-start-1 row-start-1 max-w-sm px-2 py-1 text-sm font-medium whitespace-nowrap"
            aria-hidden="true"
          >
            {renameDraft || " "}
          </span>
          <Input
            className="col-start-1 row-start-1 h-auto max-w-sm min-w-0 px-2 py-1 text-sm"
            autoFocus
            aria-label="Chat title"
            value={renameDraft}
            disabled={savingTitle}
            onChange={(event) => setRenameDraft(event.target.value)}
            onBlur={() => commitRename(chat)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                event.currentTarget.blur();
              }
              if (event.key === "Escape") {
                event.preventDefault();
                cancelRename();
              }
            }}
          />
        </div>
      ) : (
        <WithTooltip label="Rename chat">
          <button
            type="button"
            className="min-w-0 truncate font-medium hover:underline cursor-pointer"
            onClick={() => startRename(chat)}
          >
            {displayTitle}
          </button>
        </WithTooltip>
      )}

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0"
            disabled={deletingChatId !== null}
          >
            <Ellipsis className="size-4" />
            <span className="sr-only">Chat menu</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onSelect={() => startRename(chat)}>
            <Pencil />
            Rename
          </DropdownMenuItem>
          {/* Diagnostics need the native host to read the journal, so they are
              absent in a browser build rather than shown and broken. */}
          {hasNativeHost() && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                onSelect={() => void copyChatDebug(chat.id, chatDebugDeps())}
              >
                <Bug />
                Copy debug info
              </DropdownMenuItem>
              <DropdownMenuItem
                onSelect={() => void saveChatDebug(chat.id, chatDebugDeps())}
              >
                <Download />
                Save debug bundle…
              </DropdownMenuItem>
            </>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => deleteChat(chat)}>
            <Trash2 />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
