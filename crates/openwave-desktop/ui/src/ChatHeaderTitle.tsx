import { Ellipsis, Pencil, Trash2 } from "lucide-react";

import type { Chat } from "./api";
import { useApp } from "./AppContext";
import { useChatListStore } from "./ChatListStore";
import { useTypewriterOnce } from "./useTypewriterOnce";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/**
 * The open conversation's title, made actable from where it is shown.
 *
 * Rename and delete used to live only on the sidebar's chat row, out of reach
 * whenever the rail was collapsed. Clicking the title here swaps it for an edit
 * field, and the menu beside it offers the same two actions — both wired to the
 * shell's existing orchestration, so a deletion of the open chat lands home the
 * same way it does from the rail. The rename draft is the shared one, so the
 * two entry points cannot disagree about what is being typed.
 */
export function ChatHeaderTitle({ chat }: { chat: Chat }) {
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
  // Typed out only when the name arrived just now, in step with the sidebar row
  // showing the same conversation.
  const displayTitle = useTypewriterOnce(title, justNamed);

  if (renaming) {
    return (
      <Input
        className="h-auto min-w-0 max-w-sm flex-1 px-2 py-1 text-center text-sm"
        autoFocus
        aria-label="Chat title"
        value={renameDraft}
        disabled={savingTitle}
        onChange={(event) => setRenameDraft(event.target.value)}
        onBlur={() => commitRename(chat)}
        onKeyDown={(event) => {
          // Enter commits through the same blur the field would fire on losing
          // focus; Escape flags the pending blur to skip so it does not patch.
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
    );
  }

  return (
    <div className="flex min-w-0 max-w-sm flex-1 items-center justify-center gap-1">
      <button
        type="button"
        className="min-w-0 cursor-pointer truncate rounded-md px-2 py-1 text-center text-sm font-medium transition-colors hover:bg-muted"
        title="Rename chat"
        onClick={() => startRename(chat)}
      >
        {displayTitle}
      </button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
            aria-label={`Actions for ${title}`}
            disabled={deletingChatId !== null}
          >
            <Ellipsis size={15} />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="center">
          <DropdownMenuItem onSelect={() => startRename(chat)}>
            <Pencil />
            Rename
          </DropdownMenuItem>
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
