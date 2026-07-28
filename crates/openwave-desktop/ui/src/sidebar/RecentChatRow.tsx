import { CircleAlert, Ellipsis, Pencil, Trash2 } from "lucide-react";

import type { Chat } from "@/api";
import { useChatListStore } from "@/ChatListStore";
import { useTypewriterOnce } from "@/useTypewriterOnce";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/** One conversation in a rail list, with its rename field and row actions. */
export function RecentChatRow({
  chat,
  active,
  needsAttention,
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
  needsAttention: boolean;
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
  // A name the server just derived is typed out, so the row visibly stops being
  // "New chat" instead of silently having always been something else. A name that
  // was already there when this mounted appears at once.
  const justNamed = useChatListStore(
    (state) => state.derivedTitleChatId === chat.id,
  );
  const displayTitle = useTypewriterOnce(title, justNamed);

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
        {displayTitle}
      </button>
      {needsAttention && (
        <span
          className="text-warning shrink-0"
          aria-label={`${title} needs attention`}
          title="Needs attention"
        >
          <CircleAlert aria-hidden="true" size={15} />
        </span>
      )}
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
