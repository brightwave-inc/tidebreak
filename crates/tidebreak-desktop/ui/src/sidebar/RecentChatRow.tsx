import {
  Archive,
  CircleAlert,
  Ellipsis,
  FolderInput,
  Pencil,
  Pin,
  PinOff,
  Trash2,
} from "lucide-react";

import type { Chat, Project } from "@/api";
import { useChatListStore } from "@/ChatListStore";
import { useTypewriterOnce } from "@/useTypewriterOnce";
import { Loader } from "@/components/motion/loader";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuPortal,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

/**
 * What one row says about its conversation, strongest claim first.
 *
 * A question or approval waiting on you outranks a turn in flight, and a turn
 * in flight outranks one that finished while you were elsewhere. The row you
 * are looking at is never unread.
 */
export type ChatRowState = "needs-attention" | "running" | "unread" | null;

export function chatRowState({
  chat,
  active,
  needsAttention,
}: {
  chat: Pick<Chat, "running" | "unread">;
  active: boolean;
  needsAttention: boolean;
}): ChatRowState {
  if (needsAttention) return "needs-attention";
  if (chat.running) return "running";
  if (chat.unread && !active) return "unread";
  return null;
}

const STATE_LABELS: Record<Exclude<ChatRowState, null>, string> = {
  "needs-attention": "needs attention",
  running: "working",
  unread: "new since you looked",
};

/** One conversation in a rail list, with its rename field and row actions. */
export function RecentChatRow({
  chat,
  active,
  needsAttention,
  renaming,
  renameDraft,
  savingTitle,
  mutating,
  projects,
  onRenameDraftChange,
  onOpen,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onMoveToProject,
  onTogglePin,
  onArchive,
  onDelete,
}: {
  chat: Chat;
  active: boolean;
  needsAttention: boolean;
  renaming: boolean;
  renameDraft: string;
  savingTitle: boolean;
  mutating: boolean;
  /** Every project the chat could be filed under, for the move submenu. */
  projects: Project[];
  onRenameDraftChange: (draft: string) => void;
  onOpen: () => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onMoveToProject: (projectId: string | null) => void;
  onTogglePin: () => void;
  onArchive: () => void;
  onDelete: () => void;
}) {
  const title = chat.title?.trim() || "New work";
  // A name the server just derived is typed out, so the row visibly stops being
  // "New work" instead of silently having always been something else. A name that
  // was already there when this mounted appears at once.
  const justNamed = useChatListStore(
    (state) => state.derivedTitleChatId === chat.id,
  );
  const displayTitle = useTypewriterOnce(title, justNamed);
  const state = chatRowState({ chat, active, needsAttention });
  const pinned = Boolean(chat.pinned_at);

  if (renaming) {
    return (
      <Input
        className="h-auto px-2 py-1.5 text-sm"
        autoFocus
        aria-label="Work title"
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
      data-chat-row={chat.id}
      className={cn(
        "group flex items-center rounded-md transition-colors hover:bg-muted",
        active && "bg-muted",
      )}
    >
      <button
        type="button"
        className={cn(
          "min-w-0 flex-1 cursor-pointer truncate px-2 py-1.5 text-left text-sm disabled:pointer-events-none disabled:opacity-50",
          state === "unread" && "font-medium",
        )}
        aria-current={active ? "page" : undefined}
        disabled={mutating}
        title={title}
        onClick={onOpen}
      >
        {displayTitle}
        {state && <span className="sr-only">, {STATE_LABELS[state]}</span>}
      </button>
      <RowStateMark state={state} />
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            // Revealed on hover, but kept in the layout so the row does not
            // reflow under the cursor.
            className="mr-1 cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 disabled:pointer-events-none data-[state=open]:opacity-100"
            aria-label={`Actions for ${title}`}
            disabled={mutating}
          >
            <Ellipsis size={15} />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="right">
          <DropdownMenuItem onSelect={onTogglePin}>
            {pinned ? <PinOff /> : <Pin />}
            {pinned ? "Unpin" : "Pin"}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onStartRename}>
            <Pencil />
            Rename
          </DropdownMenuItem>
          {(projects.length > 0 || chat.project_id) && (
            <DropdownMenuSub>
              <DropdownMenuSubTrigger>
                <FolderInput />
                Move to project
              </DropdownMenuSubTrigger>
              <DropdownMenuPortal>
                <DropdownMenuSubContent>
                  {chat.project_id && (
                    <>
                      <DropdownMenuItem onSelect={() => onMoveToProject(null)}>
                        No project
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                    </>
                  )}
                  {projects
                    .filter((project) => project.id !== chat.project_id)
                    .map((project) => (
                      <DropdownMenuItem
                        key={project.id}
                        onSelect={() => onMoveToProject(project.id)}
                      >
                        {project.title?.trim() || "Untitled project"}
                      </DropdownMenuItem>
                    ))}
                </DropdownMenuSubContent>
              </DropdownMenuPortal>
            </DropdownMenuSub>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={onArchive}>
            <Archive />
            Archive
          </DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onSelect={onDelete}>
            <Trash2 />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/**
 * The row's one status mark, in a fixed slot so titles line up whether or not
 * a row has one.
 */
function RowStateMark({ state }: { state: ChatRowState }) {
  if (!state) return null;
  return (
    <span
      className="grid size-4 shrink-0 place-items-center"
      title={
        state === "needs-attention"
          ? "Needs attention"
          : state === "running"
            ? "Working"
            : "New since you looked"
      }
      data-row-state={state}
    >
      {state === "needs-attention" && (
        <CircleAlert aria-hidden="true" className="size-3.5 text-warning" />
      )}
      {state === "running" && (
        <Loader variant="comet" size={12} className="text-live" decorative />
      )}
      {state === "unread" && (
        <span
          aria-hidden="true"
          className="size-1.5 rounded-full bg-foreground"
        />
      )}
    </span>
  );
}
