import { useEffect, useRef } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  ChevronRight,
  CircleAlert,
  Ellipsis,
  ListFilter,
  Plus,
} from "lucide-react";
import { create } from "zustand";

import type { Chat } from "@/api";
import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { useProjectListStore } from "@/ProjectListStore";
import { SearchInput } from "@/components/SearchInput";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { RecentChatRow } from "./RecentChatRow";

/** Case-insensitive match on the title, with untitled work matching "new work". */
export function matchesChatSearch(chat: Chat, query: string): boolean {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return true;
  const title = chat.title?.trim() || "New work";
  return title.toLowerCase().includes(trimmed);
}

/**
 * The conversations this section shows: the loose ones, narrowed by the filter.
 *
 * A chat filed under a project appears under that project and nowhere else, so
 * the rail never shows one conversation in two places.
 */
export function listedChats(chats: Chat[], query: string): Chat[] {
  return chats.filter(
    (chat) => chat.project_id === null && matchesChatSearch(chat, query),
  );
}

const CHATS_COLLAPSED_KEY = "tidebreak.chats-collapsed";

function readStoredCollapsed(): boolean {
  try {
    return window.localStorage.getItem(CHATS_COLLAPSED_KEY) === "true";
  } catch {
    return false;
  }
}

/**
 * The section's own chrome state, outside the component because the rail
 * remounts on every route change: a filter typed before opening a chat has to
 * still be there after, and a collapsed list has to stay collapsed. Collapse
 * is a durable preference; the filter is for the reader's current hunt, so it
 * lives only as long as the window. Exported so shell tests can reset it —
 * module state outlives their renders.
 */
export const useChatsSectionState = create<{
  collapsed: boolean;
  filtering: boolean;
  query: string;
  toggleCollapsed: () => void;
  setFiltering: (filtering: boolean) => void;
  setQuery: (query: string) => void;
}>()((set) => ({
  collapsed: readStoredCollapsed(),
  filtering: false,
  query: "",
  toggleCollapsed: () =>
    set((state) => {
      const collapsed = !state.collapsed;
      try {
        window.localStorage.setItem(CHATS_COLLAPSED_KEY, String(collapsed));
      } catch {
        // Preference persistence is best-effort.
      }
      return { collapsed };
    }),
  // Turning the filter off forgets the query: a hidden filter that still
  // narrows the list would look like chats had been lost.
  setFiltering: (filtering) =>
    set(filtering ? { filtering } : { filtering, query: "" }),
  setQuery: (query) => set({ query }),
}));

/**
 * The rail's list of conversations, most recent first — the whole list, since
 * this is the only chat list there is. The header row carries the section's
 * three controls: the title collapses it, the ellipsis holds its options, and
 * the plus starts a chat.
 */
export function ChatsSection({ activeChatId }: { activeChatId?: string }) {
  const navigate = useNavigate();
  const {
    newChat,
    deleteChat,
    startRename,
    commitRename,
    cancelRename,
    moveChatToProject,
  } = useApp();
  const chats = useChatListStore((state) => state.chats);
  const projects = useProjectListStore((state) => state.projects);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const renamingChatId = useChatListStore((state) => state.renamingChatId);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );
  const collapsed = useChatsSectionState((state) => state.collapsed);
  const filtering = useChatsSectionState((state) => state.filtering);
  const query = useChatsSectionState((state) => state.query);
  const { toggleCollapsed, setFiltering, setQuery } = useChatsSectionState.getState();

  // The filter appears from a menu selection, so nothing natural has focus.
  const filterRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (filtering) filterRef.current?.querySelector("input")?.focus();
  }, [filtering]);

  const listed = listedChats(chats, query);
  // A collapsed list hides the per-row markers, so the header has to say when
  // something in it is waiting.
  const hiddenAttention =
    collapsed &&
    [...chatIdsWithPendingPrompts].some((id) => id !== activeChatId);

  return (
    <div className="mt-4 flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-0.5 pr-1">
        <button
          type="button"
          aria-expanded={!collapsed}
          onClick={toggleCollapsed}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded-md px-2 py-1 text-left text-sm font-medium text-muted-foreground transition-colors hover:text-foreground"
        >
          <span>Work</span>
          <ChevronRight
            aria-hidden="true"
            className={cn("size-3.5 transition-transform", !collapsed && "rotate-90")}
          />
          {hiddenAttention && (
            <span
              className="text-warning ml-auto shrink-0"
              aria-label="Work needs attention"
              title="Work needs attention"
            >
              <CircleAlert aria-hidden="true" size={15} />
            </span>
          )}
        </button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
              aria-label="Work list options"
            >
              <Ellipsis size={15} />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuItem onSelect={() => setFiltering(!filtering)}>
              <ListFilter />
              {filtering ? "Hide filter" : "Filter work"}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <button
          type="button"
          className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
          aria-label={creatingChat ? "Starting…" : "New work"}
          disabled={creatingChat || deletingChatId !== null}
          onClick={newChat}
        >
          <Plus size={15} />
        </button>
      </div>

      {!collapsed && filtering && (
        <div ref={filterRef} className="shrink-0 px-1 pt-1 pb-1.5">
          <SearchInput
            size="sm"
            placeholder="Filter work"
            aria-label="Filter work"
            value={query}
            onValueChange={setQuery}
          />
        </div>
      )}

      {!collapsed && (
        <div
          className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto"
          aria-label="Work list"
        >
          {listed.map((chat) => (
            <RecentChatRow
              key={chat.id}
              chat={chat}
              active={chat.id === activeChatId}
              needsAttention={chatIdsWithPendingPrompts.has(chat.id)}
              renaming={renamingChatId === chat.id}
              renameDraft={renameChatDraft}
              savingTitle={savingTitle}
              mutating={deletingChatId !== null || creatingChat}
              projects={projects}
              onRenameDraftChange={setRenameDraft}
              onOpen={() => void navigate({ to: "/c/$chatId", params: { chatId: chat.id } })}
              onStartRename={() => startRename(chat)}
              onCommitRename={() => commitRename(chat)}
              onCancelRename={cancelRename}
              onMoveToProject={(projectId) => moveChatToProject(chat, projectId)}
              onDelete={() => deleteChat(chat)}
            />
          ))}
          {listed.length === 0 && query.trim() && (
            <p className="px-2 py-1 text-xs text-muted-foreground">
              No work title contains that.
            </p>
          )}
        </div>
      )}
    </div>
  );
}
